//! Config hot reload through `AppState`: file re-read, atomic swap, restart-only fields.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rrouter_gateway::auth::Auth;
use rrouter_gateway::config::{Config, ConfigSnapshot};
use rrouter_gateway::state::{concurrency_for, dispatcher, Slot};
use rrouter_gateway::AppState;
use rrouter_router::{Catalog, Strategy};
use rrouter_store::Store;

fn no_env(_: &str) -> Option<String> {
    None
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rrouter-reload-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A live config whose only non-default field is the data directory, so a file can match it
/// exactly (an empty file would otherwise differ on `data_dir` and be restart-only).
fn live_config(dir: &Path) -> Config {
    let data_dir = dir.to_string_lossy().into_owned();
    let env = move |name: &str| (name == "RROUTER_DATA_DIR").then(|| data_dir.clone());
    Config::resolve(&env, None).expect("config").config
}

fn state(config: Config) -> Arc<AppState> {
    let data_dir = config.data_dir.clone();
    let concurrency = concurrency_for(config.max_concurrency);
    Arc::new(AppState {
        store: Arc::new(Store::open_in_memory().expect("store")),
        catalog: Arc::new(Catalog::builtin()),
        refresher: Arc::new(rrouter_auth::Refresher::new(reqwest::Client::new())),
        planner: rrouter_router::Planner::new(),
        dispatcher: Arc::new(dispatcher(&config)),
        http: reqwest::Client::new(),
        config: ConfigSnapshot::from(config),
        started: std::time::Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        envelope_drops: Default::default(),
        concurrency: std::sync::RwLock::new(concurrency),
        auth: Auth::load(&data_dir, false),
    })
}

fn write_config(path: &Path, dir: &Path, strategy: &str) {
    std::fs::write(
        path,
        format!(
            r#"{{"data_dir":"{}","strategy":"{strategy}"}}"#,
            dir.display()
        ),
    )
    .expect("write config");
}

#[test]
fn reload_from_file_applies_a_strategy_without_restart() {
    let dir = temp_dir("strategy");
    let state = state(live_config(&dir));
    let file = dir.join("config.json");
    write_config(&file, &dir, "weighted");

    let outcome = state
        .reload_from(&file, &no_env)
        .expect("a valid file must reload");

    assert!(outcome.applied);
    assert_eq!(state.config.load().strategy, Strategy::Weighted);
    assert_eq!(
        outcome.plan.changed_fields().collect::<Vec<_>>(),
        vec!["strategy"]
    );
    assert_eq!(outcome.config.strategy, Strategy::Weighted);

    // The next reload reads the file again, not a cached parse.
    write_config(&file, &dir, "priority");
    let outcome = state.reload_from(&file, &no_env).expect("reload back");
    assert!(outcome.applied);
    assert_eq!(state.config.load().strategy, Strategy::Priority);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_invalid_file_leaves_the_running_config_untouched() {
    let dir = temp_dir("invalid");
    let state = state(live_config(&dir));
    let file = dir.join("bad.json");
    std::fs::write(
        &file,
        format!(r#"{{"data_dir":"{}","cache":"redis"}}"#, dir.display()),
    )
    .expect("write config");

    let error = state
        .reload_from(&file, &no_env)
        .expect_err("an invalid file must not apply");

    assert!(error.contains("cache"), "{error}");
    assert_eq!(state.config.load().strategy, Strategy::Priority);
    assert_eq!(
        state.config.load().cache,
        rrouter_gateway::config::CacheMode::Off
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn restart_only_changes_are_reported_but_never_applied() {
    let dir = temp_dir("restart");
    let state = state(live_config(&dir));
    let live = state.config.load().as_ref().clone();

    let mut candidate = live.clone();
    candidate.host = "0.0.0.0".parse().expect("ip");
    candidate.port = 20_999;
    candidate.data_dir = PathBuf::from("/elsewhere");
    candidate.static_dir = Some(PathBuf::from("/ext"));
    candidate.master_key = Some("ab".repeat(32));
    candidate.strategy = Strategy::Weighted;

    let outcome = state.apply_reload(&candidate);
    assert!(
        outcome.applied,
        "strategy still applies: {:?}",
        outcome.plan
    );

    let now = state.config.load();
    assert_eq!(now.strategy, Strategy::Weighted);
    assert_eq!(now.host, live.host);
    assert_eq!(now.port, live.port);
    assert_eq!(now.data_dir, live.data_dir);
    assert_eq!(now.static_dir, None);
    assert_eq!(now.master_key, None);

    let restart: Vec<&str> = outcome
        .plan
        .restart_required
        .iter()
        .map(|entry| entry.field)
        .collect();
    for field in ["host", "port", "data_dir", "static_dir", "master_key"] {
        assert!(restart.contains(&field), "{field} missing from {restart:?}");
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_unchanged_file_is_a_noop() {
    let dir = temp_dir("same");
    let state = state(live_config(&dir));
    let file = dir.join("config.json");
    write_config(&file, &dir, "priority");

    let outcome = state.reload_from(&file, &no_env).expect("reload");

    assert!(!outcome.applied);
    assert!(outcome.plan.is_empty(), "{:?}", outcome.plan);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_reload_swaps_the_concurrency_cap_and_keeps_permits() {
    let dir = temp_dir("concurrency");
    let state = state(live_config(&dir));

    let mut candidate = state.config.load().as_ref().clone();
    candidate.max_concurrency = 1;
    assert!(state.apply_reload(&candidate).applied);

    let first = state.try_slot();
    assert!(
        matches!(first, Slot::Granted(Some(_))),
        "one permit expected"
    );
    assert!(
        matches!(state.try_slot(), Slot::Full),
        "the reloaded cap must be in force"
    );
    drop(first);
    assert!(matches!(state.try_slot(), Slot::Granted(Some(_))));

    // Back to unlimited without restarting.
    let mut off = candidate.clone();
    off.max_concurrency = 0;
    assert!(state.apply_reload(&off).applied);
    assert!(matches!(state.try_slot(), Slot::Granted(None)));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_reload_plan_never_exposes_secret_values() {
    let dir = temp_dir("secrets");
    let state = state(live_config(&dir));

    let mut with_token = state.config.load().as_ref().clone();
    with_token.admin_token = Some("reload-secret-old-0123456789".into());
    assert!(state.apply_reload(&with_token).applied);

    let mut candidate = with_token.clone();
    candidate.admin_token = Some("reload-secret-new-abcdefghij".into());
    let outcome = state.apply_reload(&candidate);
    assert!(outcome.applied);

    let rendered = format!("{:?}", outcome.plan);
    assert!(
        !rendered.contains("reload-secret-old-0123456789"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("reload-secret-new-abcdefghij"),
        "{rendered}"
    );
    assert!(rendered.contains("rotated"), "{rendered}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn readers_keep_one_snapshot_while_reloads_land() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let dir = temp_dir("readers");
    let state = state(live_config(&dir));
    let stop = Arc::new(AtomicBool::new(false));
    let readers: Vec<_> = (0..3)
        .map(|_| {
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut observed = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let config = state.config.load();
                    let pair = (config.strategy, config.cache_ttl_secs);
                    assert!(
                        pair == (Strategy::Priority, 300) || pair == (Strategy::Weighted, 60),
                        "a reader observed a mixed snapshot: {pair:?}"
                    );
                    observed += 1;
                }
                observed
            })
        })
        .collect();

    let mut fast = state.config.load().as_ref().clone();
    fast.strategy = Strategy::Weighted;
    fast.cache_ttl_secs = 60;
    for _ in 0..300 {
        state.apply_reload(&fast);
        let mut slow = fast.clone();
        slow.strategy = Strategy::Priority;
        slow.cache_ttl_secs = 300;
        state.apply_reload(&slow);
    }
    stop.store(true, Ordering::Relaxed);
    let total: u64 = readers
        .into_iter()
        .map(|reader| reader.join().expect("reader"))
        .sum();
    assert!(total > 0);

    std::fs::remove_dir_all(&dir).ok();
}
