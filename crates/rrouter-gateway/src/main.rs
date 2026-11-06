//! `rrouter` — one binary: config, store, refresher, dispatcher, HTTP server.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rrouter_gateway::{app, config::Config, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher};
use rrouter_store::Store;
use tracing_subscriber::EnvFilter;

const BANNER: &str = "rrouter";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: rrouter [--print-config]\n\nEvery option is an environment variable; see README.");
        return ExitCode::SUCCESS;
    }

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(problem) => {
            eprintln!("rrouter: invalid configuration: {problem}");
            return ExitCode::FAILURE;
        }
    };
    if args.iter().any(|a| a == "--print-config") {
        match serde_json::to_string_pretty(&config.redacted()) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("rrouter: cannot render config: {e}");
                return ExitCode::FAILURE;
            }
        }
        return ExitCode::SUCCESS;
    }

    init_tracing(&config.log_filter);
    match run(config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            eprintln!("rrouter: {e}");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing(filter: &str) {
    let env_filter = EnvFilter::try_new(filter).unwrap_or_else(|_| EnvFilter::new("rrouter=info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .compact()
        .init();
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

fn run(config: Config) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    runtime.block_on(serve(config))
}

async fn serve(config: Config) -> Result<(), String> {
    std::fs::create_dir_all(&config.data_dir)
        .map_err(|e| format!("create data dir {}: {e}", config.data_dir.display()))?;
    let store = Store::open(&config.db_path(), config.master_key.clone())
        .map_err(|e| format!("open store: {e}"))?;

    let http = build_http_client(&config)?;
    let state = Arc::new(AppState {
        store: Arc::new(store),
        catalog: Arc::new(Catalog::builtin()),
        refresher: Arc::new(rrouter_auth::Refresher::new(http.clone())),
        planner: rrouter_router::Planner::new(),
        dispatcher: Arc::new(Dispatcher::new(Breaker::new())),
        http,
        config: config.clone(),
        started: Instant::now(),
    });

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
        if config.require_api_key { " (api key required)" } else { "" }
    );

    axum::serve(listener, app(state).into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| format!("server: {e}"))?;
    tracing::info!("{BANNER} stopped");
    Ok(())
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
