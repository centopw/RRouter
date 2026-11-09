//! `rrouter` — one binary: config, store, refresher, dispatcher, HTTP server.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rrouter_gateway::auth::Auth;
use rrouter_gateway::{
    app,
    config::{Config, Loaded, LogFormat},
    state::{concurrency_for, dispatcher},
    AppState,
};
use rrouter_router::Catalog;
use rrouter_store::{hash_password, random_token, Store};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer as _;

const BANNER: &str = "rrouter";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        // Installers and health checks read this without touching the data directory.
        println!("rrouter {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "usage: rrouter [--version] [--print-config] [--check-config] \
             [--healthcheck|--ready]\n\n\
             Every option is an environment variable, plus the optional JSON file named\n\
             by RROUTER_CONFIG. `--check-config` validates that merge and exits 0/1;\n\
             `--print-config` prints the effective, secrets-free configuration.",
        );
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--healthcheck") {
        return healthcheck();
    }
    if args.iter().any(|a| a == "--ready") {
        return readycheck();
    }
    if args.first().map(String::as_str) == Some("admin") {
        return admin(&args[1..]);
    }

    let loaded = match Config::load_env() {
        Ok(loaded) => loaded,
        Err(problem) => {
            eprintln!("rrouter: invalid configuration: {problem}");
            return ExitCode::FAILURE;
        }
    };
    if args.iter().any(|a| a == "--check-config") {
        return check_config(&loaded);
    }
    if args.iter().any(|a| a == "--print-config") {
        match serde_json::to_string_pretty(&loaded.redacted()) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("rrouter: cannot render config: {e}");
                return ExitCode::FAILURE;
            }
        }
        return ExitCode::SUCCESS;
    }

    // The exporter is built inside `run`, where the tokio runtime exists for the batch
    // processor; an invalid exporter stops startup instead of silently exporting nowhere.
    #[cfg(feature = "otel")]
    let result = run(loaded.config, loaded.otel);
    #[cfg(not(feature = "otel"))]
    let result = run(loaded.config);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            eprintln!("rrouter: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `--check-config`: the file and the environment were already merged and validated by
/// [`Config::load_env`], so a failure never reaches here — the caller prints it and exits
/// 1 with the offending source named. Success reports where the values came from.
fn check_config(loaded: &Loaded) -> ExitCode {
    let (env, file, defaults) = loaded.sources.counts();
    match &loaded.config_file {
        Some(path) => println!("config file: {}", path.display()),
        None => println!("config file: none (RROUTER_CONFIG is unset)"),
    }
    println!("config ok: {env} from the environment, {file} from the file, {defaults} default");
    ExitCode::SUCCESS
}

/// Container probe: `GET /health` over a plain socket, so the distroless image needs no
/// shell, curl or Node. Reads the same host/port environment the server does.
///
/// Liveness only: the process answers, which is all an orchestrator should restart on.
fn healthcheck() -> ExitCode {
    probe_env("/health")
}

/// Readiness probe: `GET /ready`, which also reads the store. A process whose database
/// cannot be opened is running but cannot serve, so it must leave the load-balancer pool
/// instead of being killed and restarted in a loop.
fn readycheck() -> ExitCode {
    probe_env("/ready")
}

fn probe_env(path: &str) -> ExitCode {
    let host = std::env::var("RROUTER_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    // Binding a wildcard address is not connectable; probe loopback instead.
    let host = if host == "0.0.0.0" || host == "::" {
        "127.0.0.1".to_string()
    } else {
        host
    };
    let port = std::env::var("RROUTER_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(20128);
    match probe_health(&host, port, path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rrouter: probe {path}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Local operator commands. They open the store exactly like the server does and never
/// speak HTTP, so they work on a stopped instance.
fn admin(args: &[String]) -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(problem) => {
            eprintln!("rrouter: invalid configuration: {problem}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("rrouter: runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let store = match Store::open(&config.db_path(), config.master_key.clone()) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("rrouter: open store: {e}");
            return ExitCode::FAILURE;
        }
    };
    match args {
        [command] if command == "show-setup-code" => {
            let users_exist = runtime.block_on(store.users_exist()).unwrap_or(true);
            if users_exist {
                eprintln!("rrouter: this instance already has a user; the setup code is gone");
                return ExitCode::FAILURE;
            }
            let auth = Auth::load(&config.data_dir, false);
            match auth.code() {
                Some(code) => {
                    println!("{code}");
                    ExitCode::SUCCESS
                }
                None => ExitCode::FAILURE,
            }
        }
        [command, path] if command == "backup" => {
            let path = std::path::PathBuf::from(path);
            match runtime.block_on(store.backup_to(&path)) {
                Ok(()) => {
                    println!("Backup written to {}", path.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("rrouter: backup failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        [command, email] if command == "reset-password" => {
            let user = match runtime.block_on(store.user_by_email(email)) {
                Ok(Some(user)) => user,
                Ok(None) => {
                    eprintln!("rrouter: no user with email {email}");
                    return ExitCode::FAILURE;
                }
                Err(e) => {
                    eprintln!("rrouter: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let password = &random_token()[..20];
            let hash = match hash_password(password) {
                Ok(hash) => hash,
                Err(e) => {
                    eprintln!("rrouter: {e}");
                    return ExitCode::FAILURE;
                }
            };
            if let Err(e) = runtime.block_on(store.set_user_password(user.id, &hash)) {
                eprintln!("rrouter: {e}");
                return ExitCode::FAILURE;
            }
            println!("New password for {}: {password}", user.email);
            println!("Every session for that account was signed out.");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "usage: rrouter admin show-setup-code | admin reset-password <email> | admin backup <file>"
            );
            ExitCode::FAILURE
        }
    }
}

fn probe_health(host: &str, port: u16, path: &str) -> std::io::Result<()> {
    use std::io::{Read, Write};
    use std::net::ToSocketAddrs;

    let addr = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::other(format!("no address for {host}:{port}")))?;
    let mut stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    // `path` is a literal from this module (`/health`, `/ready`), never user input, so it
    // cannot smuggle a second request line.
    stream.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    let mut head = [0u8; 32];
    let n = stream.read(&mut head)?;
    let status = String::from_utf8_lossy(&head[..n]).to_string();
    if status.starts_with("HTTP/1.1 200") || status.starts_with("HTTP/1.0 200") {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "unexpected response: {}",
            status.lines().next().unwrap_or("<empty>")
        )))
    }
}

/// The filter and format together, as one replaceable layer: a reload swaps both atomically,
/// so a request never logs with the old filter and the new shape.
#[cfg(feature = "otel")]
type LogBase = tracing_subscriber::layer::Layered<
    Option<
        tracing_opentelemetry::OpenTelemetryLayer<
            tracing_subscriber::Registry,
            opentelemetry_sdk::trace::Tracer,
        >,
    >,
    tracing_subscriber::Registry,
>;
#[cfg(not(feature = "otel"))]
type LogBase = tracing_subscriber::Registry;

type LogLayer = Box<dyn tracing_subscriber::Layer<LogBase> + Send + Sync>;
type LogReload = tracing_subscriber::reload::Handle<LogLayer, LogBase>;

/// How often `RROUTER_CONFIG`'s mtime is checked when a config file is configured.
const CONFIG_POLL: Duration = Duration::from_secs(5);

fn log_layer(filter: &str, format: LogFormat) -> LogLayer {
    let env_filter = EnvFilter::try_new(filter).unwrap_or_else(|_| EnvFilter::new("rrouter=info"));
    // Both shapes keep the same fields; json is one object per line for log pipelines.
    match format {
        LogFormat::Text => Box::new(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .compact()
                .with_filter(env_filter),
        ),
        LogFormat::Json => Box::new(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .json()
                .with_filter(env_filter),
        ),
    }
}

/// Install the subscriber and hand back the handle a config reload uses to replace it.
///
/// The OpenTelemetry layer is innermost so the reloadable fmt layer and the system-log
/// capture layer keep working exactly as before, with or without the feature.
fn init_tracing(
    filter: &str,
    format: LogFormat,
    #[cfg(feature = "otel")] otel: Option<&rrouter_gateway::telemetry::otel::Exporter>,
) -> LogReload {
    let (layer, handle) = tracing_subscriber::reload::Layer::new(log_layer(filter, format));
    #[cfg(feature = "otel")]
    {
        // The capture layer is always installed: it is a no-op until an operator enables
        // system logs, so enabling never requires a restart.
        tracing_subscriber::registry()
            .with(otel.map(rrouter_gateway::telemetry::otel::Exporter::layer))
            .with(layer)
            .with(rrouter_gateway::syslog::CaptureLayer)
            .init();
    }
    #[cfg(not(feature = "otel"))]
    {
        tracing_subscriber::registry()
            .with(layer)
            .with(rrouter_gateway::syslog::CaptureLayer)
            .init();
    }
    handle
}

/// Apply a reloadable config change without a restart.
///
/// SIGHUP always triggers a reload; when `RROUTER_CONFIG` is set, its mtime is also polled
/// every [`CONFIG_POLL`] (no file-watcher dependency). The snapshot swap happens in
/// [`AppState::reload_from_file`]; this task owns the pieces that live outside it — the
/// tracing subscriber for `log_filter`/`log_format`.
fn spawn_config_reload(state: Arc<AppState>, log: LogReload) {
    let path = std::env::var("RROUTER_CONFIG")
        .ok()
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty());
    #[cfg(unix)]
    let mut hangup: Option<tokio::signal::unix::Signal> =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).ok();
    tokio::spawn(async move {
        let mut last_mtime = path.as_deref().and_then(file_mtime);
        loop {
            #[cfg(unix)]
            let signalled = match hangup.as_mut() {
                Some(signal) => tokio::select! {
                    _ = signal.recv() => true,
                    _ = tokio::time::sleep(CONFIG_POLL) => false,
                },
                None => {
                    tokio::time::sleep(CONFIG_POLL).await;
                    false
                }
            };
            #[cfg(not(unix))]
            let signalled = {
                tokio::time::sleep(CONFIG_POLL).await;
                false
            };

            if !signalled {
                let Some(path) = path.as_deref() else {
                    continue;
                };
                let mtime = file_mtime(path);
                if mtime.is_none() || mtime == last_mtime {
                    continue;
                }
                last_mtime = mtime;
            }

            let Some(path) = path.as_deref() else {
                tracing::warn!("SIGHUP received but RROUTER_CONFIG is not set; nothing to reload");
                continue;
            };
            match state.reload_from_file(std::path::Path::new(path)) {
                Ok(outcome) => {
                    if outcome
                        .changed_fields()
                        .any(|field| field == "log_filter" || field == "log_format")
                    {
                        let layer =
                            log_layer(&outcome.config.log_filter, outcome.config.log_format);
                        if let Err(error) = log.reload(layer) {
                            tracing::warn!(
                                error = %error,
                                "could not apply log settings; keeping the previous subscriber"
                            );
                        }
                    }
                }
                Err(problem) => tracing::error!(
                    path,
                    error = %problem,
                    "configuration reload failed; keeping the running config"
                ),
            }
        }
    });
}

fn file_mtime(path: &str) -> Option<std::time::SystemTime> {
    std::fs::metadata(path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
}

fn build_http_client(config: &Config) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(config.connect_timeout_secs))
        .pool_idle_timeout(Duration::from_secs(90))
        // No global timeout: streaming responses may legitimately run for minutes, so the
        // per-request timeout is applied by the proxy to the response *headers* instead.
        .build()
        .map_err(|e| format!("http client: {e}"))
}

#[cfg(not(feature = "otel"))]
fn run(config: Config) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    let log = init_tracing(&config.log_filter, config.log_format);
    runtime.block_on(serve(config, log))
}

/// Feature `otel`: build the exporter inside the runtime, install it as a layer, and
/// flush on the way out. A flush failure is logged; it must not change the exit code.
#[cfg(feature = "otel")]
fn run(config: Config, otel: Option<rrouter_gateway::config::OtelConfig>) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    // The OTLP batch processor exports from runtime tasks, so the exporter is built with
    // the runtime entered rather than before it exists.
    let _entered = runtime.enter();
    let exporter = match otel {
        Some(settings) => Some(rrouter_gateway::telemetry::otel::Exporter::from_config(
            &settings,
        )?),
        None => None,
    };
    let log = init_tracing(&config.log_filter, config.log_format, exporter.as_ref());
    runtime.block_on(async move {
        let result = serve(config, log).await;
        if let Some(exporter) = exporter {
            // Flushing blocks; run it off the async workers, but bound it so a dead
            // collector cannot hold the process after the server has drained.
            let flushed = tokio::time::timeout(
                Duration::from_secs(5),
                tokio::task::spawn_blocking(move || {
                    if let Err(e) = exporter.force_flush() {
                        tracing::warn!(error = %e, "OpenTelemetry flush failed");
                    }
                    if let Err(e) = exporter.shutdown() {
                        tracing::warn!(error = %e, "OpenTelemetry shutdown failed");
                    }
                }),
            )
            .await;
            if flushed.is_err() {
                tracing::warn!("OpenTelemetry shutdown timed out after 5s");
            }
        }
        result
    })
}

async fn serve(config: Config, log: LogReload) -> Result<(), String> {
    std::fs::create_dir_all(&config.data_dir)
        .map_err(|e| format!("create data dir {}: {e}", config.data_dir.display()))?;
    let store = Store::open(&config.db_path(), config.master_key.clone())
        .map_err(|e| format!("open store: {e}"))?;
    let users_exist = store
        .users_exist()
        .await
        .map_err(|e| format!("read users: {e}"))?;
    // Browser-session logins do not survive a redeploy; remember-me sessions do.
    if let Err(e) = store.prune_ephemeral_sessions().await {
        tracing::warn!(error = %e, "could not prune non-persistent sessions");
    }

    let http = build_http_client(&config)?;
    let state = Arc::new(AppState {
        store: Arc::new(store),
        catalog: Arc::new(Catalog::builtin()),
        refresher: Arc::new(rrouter_auth::Refresher::new(http.clone())),
        planner: rrouter_router::Planner::new(),
        dispatcher: Arc::new(dispatcher(&config)),
        http,
        config: rrouter_gateway::config::ConfigSnapshot::from(config.clone()),
        started: Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        concurrency: std::sync::RwLock::new(concurrency_for(config.max_concurrency)),
        auth: rrouter_gateway::auth::Auth::load(&config.data_dir, users_exist),
    });

    if let Err(e) = rrouter_gateway::settings::prime(&state).await {
        tracing::warn!(error = %e, "settings could not be primed; Token Saver defaults on");
    }

    // Retention lives on its own task: the first pass waits a few seconds, so startup
    // and the initial bind are never delayed by a large delete.
    state.spawn_retention();

    // SIGHUP (and the mtime poll when a config file is set) applies reloadable settings to
    // the live snapshot; the task also owns the tracing subscriber.
    spawn_config_reload(Arc::clone(&state), log);

    // Fail closed: binding beyond loopback while the admin gate is still in its
    // default loopback-only mode leaves a half-open deployment — the client API is
    // reachable, the admin API answers 403 even to the operator's own browser via a
    // reverse proxy, and nothing explains why. Require an explicit decision.
    if !config.host.is_loopback() && !config.allow_remote_admin {
        return Err(format!(
            "RROUTER_HOST={} is not loopback but RROUTER_ALLOW_REMOTE_ADMIN is false; \
             set RROUTER_ALLOW_REMOTE_ADMIN=true to serve the admin API remotely, or \
             bind 127.0.0.1",
            config.host
        ));
    }

    let listener = tokio::net::TcpListener::bind(config.socket_addr())
        .await
        .map_err(|e| format!("bind {}: {e}", config.socket_addr()))?;
    tracing::info!(
        "{BANNER} {} listening on http://{}{}",
        env!("CARGO_PKG_VERSION"),
        config.socket_addr(),
        if config.require_api_key {
            " (api key required)"
        } else {
            ""
        }
    );

    // The signal must be *observed* separately from the shutdown it triggers: bounding
    // the whole `serve` future with the grace timer would exit the process after N
    // seconds of normal uptime.
    let stop = Arc::new(tokio::sync::Notify::new());
    // Webhook delivery is off the request path: one worker task polls the outbox. It is
    // stopped by the same notification as the HTTP drain and awaited below, so no task
    // outlives `serve`.
    let mut webhooks = rrouter_gateway::webhooks::spawn(
        Arc::clone(&state.store),
        state.http.clone(),
        Arc::clone(&stop),
    );
    let graceful = {
        let stop = Arc::clone(&stop);
        async move {
            shutdown_signal().await;
            stop.notify_one();
        }
    };
    let server = axum::serve(
        listener,
        app(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(graceful);

    let result = serve_until_shutdown(server, Arc::clone(&stop), config.shutdown_grace_secs).await;
    // Wake the worker even when the server ended without a signal, then give it a moment
    // to finish the tick in flight. Aborting can leave rows leased; the next boot's
    // `recover_stale_events` reclaims them.
    stop.notify_one();
    if tokio::time::timeout(Duration::from_secs(2), &mut webhooks)
        .await
        .is_err()
    {
        webhooks.abort();
        tracing::warn!("webhook worker did not stop within 2s; aborted");
    }
    result.map_err(|e| format!("server: {e}"))?;
    tracing::info!("{BANNER} stopped");
    Ok(())
}

/// Run the server until it ends or a shutdown signal arrives; after the signal, give the
/// drain `grace_secs` (0 = wait indefinitely). A live SSE stream would otherwise let a
/// deploy hang forever.
async fn serve_until_shutdown<F, E>(
    server: F,
    stop: Arc<tokio::sync::Notify>,
    grace_secs: u64,
) -> std::result::Result<(), E>
where
    F: std::future::IntoFuture<Output = std::result::Result<(), E>>,
{
    // axum's graceful wrapper implements IntoFuture, not Future.
    let server = server.into_future();
    tokio::pin!(server);
    if grace_secs == 0 {
        return server.await;
    }
    tokio::select! {
        result = &mut server => result,
        () = stop.notified() => {
            match tokio::time::timeout(Duration::from_secs(grace_secs), &mut server).await {
                Ok(result) => result,
                Err(_) => {
                    tracing::warn!(
                        "shutdown grace of {grace_secs}s expired with requests still streaming; exiting"
                    );
                    Ok(())
                }
            }
        }
    }
}

/// Resolve on SIGINT (ctrl-c) or SIGTERM, whichever comes first.
async fn shutdown_signal() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("shutdown signal received");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grace timer must start with the signal, not with the process: a server that
    /// is still serving has to stay up past the grace period.
    #[tokio::test]
    async fn grace_starts_at_the_signal_not_at_startup() {
        let stop = Arc::new(tokio::sync::Notify::new());
        let waiter = Arc::clone(&stop);
        let handle = tokio::spawn(async move {
            serve_until_shutdown(
                std::future::pending::<Result<(), &'static str>>(),
                waiter,
                1,
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !handle.is_finished(),
            "no signal yet: the server must keep running"
        );

        stop.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(3), handle)
            .await
            .expect("the drain is bounded by the grace period")
            .expect("join");
        assert!(result.is_ok());
    }

    /// A real graceful shutdown does not wait for the grace: when the server future
    /// completes, the process stops immediately.
    #[tokio::test]
    async fn a_finished_server_exits_without_waiting() {
        let stop = Arc::new(tokio::sync::Notify::new());
        let started = Instant::now();
        serve_until_shutdown(async { Ok::<(), &'static str>(()) }, stop, 30)
            .await
            .expect("no error");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
