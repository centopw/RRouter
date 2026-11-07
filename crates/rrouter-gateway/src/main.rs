//! `rrouter` — one binary: config, store, refresher, dispatcher, HTTP server.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rrouter_gateway::{
    app,
    config::{Config, LogFormat},
    state::{concurrency_for, dispatcher},
    AppState,
};
use rrouter_router::Catalog;
use rrouter_store::Store;
use tracing_subscriber::EnvFilter;

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
            "usage: rrouter [--version] [--print-config] [--healthcheck]\n\n\
             Every option is an environment variable; see README."
        );
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--healthcheck") {
        return healthcheck();
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

    init_tracing(&config.log_filter, config.log_format);
    match run(config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            eprintln!("rrouter: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Container probe: `GET /health` over a plain socket, so the distroless image needs no
/// shell, curl or Node. Reads the same host/port environment the server does.
fn healthcheck() -> ExitCode {
    let host = std::env::var("RROUTER_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    // Binding a wildcard address is not connectable; probe loopback instead.
    let host = if host == "0.0.0.0" || host == "::" { "127.0.0.1".to_string() } else { host };
    let port = std::env::var("RROUTER_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(20128);
    match probe_health(&host, port) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rrouter: healthcheck: {e}");
            ExitCode::FAILURE
        }
    }
}

fn probe_health(host: &str, port: u16) -> std::io::Result<()> {
    use std::io::{Read, Write};
    use std::net::ToSocketAddrs;

    let addr = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::other(format!("no address for {host}:{port}")))?;
    let mut stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(
        b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
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

fn init_tracing(filter: &str, format: LogFormat) {
    let env_filter = EnvFilter::try_new(filter).unwrap_or_else(|_| EnvFilter::new("rrouter=info"));
    let builder = tracing_subscriber::fmt().with_env_filter(env_filter).with_target(false);
    // Both shapes keep the same fields; json is one object per line for log pipelines.
    match format {
        LogFormat::Text => builder.compact().init(),
        LogFormat::Json => builder.json().init(),
    }
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
        dispatcher: Arc::new(dispatcher(&config)),
        http,
        config: config.clone(),
        started: Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        concurrency: concurrency_for(config.max_concurrency),
    });

    // Retention lives on its own task: the first pass waits a few seconds, so startup
    // and the initial bind are never delayed by a large delete.
    state.spawn_retention();

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

    // The signal must be *observed* separately from the shutdown it triggers: bounding
    // the whole `serve` future with the grace timer would exit the process after N
    // seconds of normal uptime.
    let stop = Arc::new(tokio::sync::Notify::new());
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

    serve_until_shutdown(server, stop, config.shutdown_grace_secs)
        .await
        .map_err(|e| format!("server: {e}"))?;
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
            serve_until_shutdown(std::future::pending::<Result<(), &'static str>>(), waiter, 1)
                .await
        });

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!handle.is_finished(), "no signal yet: the server must keep running");

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
