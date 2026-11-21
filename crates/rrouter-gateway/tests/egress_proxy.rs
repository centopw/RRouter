//! Per-account egress: an account with a proxy must leave through it, an account without
//! one must never touch it, and a proxy the client cannot build must fail the attempt
//! instead of connecting directly.
//!
//! The point of a per-account proxy is address consistency — free-tier accounts are
//! throttled and suspended per (account, address) — so a silent direct connection is worse
//! than a failed request: it spends the account on the wrong identity. These tests run a
//! real forward proxy (absolute-form for plain-http upstreams, `CONNECT` for the rest) and
//! let it stamp the request it forwarded, which is the only way to prove *which* egress an
//! attempt actually used.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use http_body_util::BodyExt;
use rrouter_auth::Credentials;
use rrouter_gateway::{app, config::Config, AppState};
use rrouter_router::{Breaker, Catalog, Dispatcher, Planner};
use rrouter_store::Store;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

// ------------------------------------------------------------------ mock upstream

/// The provider every connection here points at. It records the egress marker the proxy
/// stamped, which is the observation each assertion rests on.
#[derive(Clone)]
struct Upstream {
    url: String,
    hits: Arc<AtomicUsize>,
    /// One entry per request: the `x-egress-proxy` value it arrived with (`""` when it
    /// came straight from the gateway).
    vias: Arc<Mutex<Vec<String>>>,
}

impl Upstream {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind upstream");
        let addr = listener.local_addr().expect("upstream addr");
        let upstream = Upstream {
            url: format!("http://{addr}"),
            hits: Arc::new(AtomicUsize::new(0)),
            vias: Arc::new(Mutex::new(Vec::new())),
        };
        let router = Router::new()
            .route("/v1/chat/completions", post(chat))
            .route("/v1/images/generations", post(images))
            .with_state(upstream.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        upstream
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    fn vias(&self) -> Vec<String> {
        self.vias.lock().expect("vias").clone()
    }
}

fn record(upstream: &Upstream, headers: &HeaderMap) {
    upstream.hits.fetch_add(1, Ordering::SeqCst);
    let via = headers
        .get("x-egress-proxy")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    upstream.vias.lock().expect("vias").push(via);
}

async fn chat(State(upstream): State<Upstream>, headers: HeaderMap) -> Response {
    record(&upstream, &headers);
    axum::Json(json!({
        "id": "chatcmpl-egress",
        "object": "chat.completion",
        "created": 1,
        "model": "gpt-4.1",
        "choices": [
            {"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}
        ],
        "usage": {"prompt_tokens": 3, "completion_tokens": 1}
    }))
    .into_response()
}

/// The images passthrough's own shape: the gateway relays it verbatim.
async fn images(State(upstream): State<Upstream>, headers: HeaderMap) -> Response {
    record(&upstream, &headers);
    axum::Json(json!({
        "created": 1,
        "data": [{"b64_json": "aGk="}]
    }))
    .into_response()
}

// ------------------------------------------------------------------ forward proxy

#[derive(Default)]
struct ProxyLog {
    /// TCP connections the proxy accepted: zero proves an account never used it.
    connections: AtomicUsize,
    /// One entry per forwarded head: `METHOD path` (origin-form, after rewriting).
    requests: Mutex<Vec<String>>,
    /// The authorities requests were forwarded to.
    authorities: Mutex<Vec<String>>,
}

/// A minimal HTTP forward proxy: absolute-form requests for a plain-http upstream,
/// `CONNECT` tunnels otherwise. It stamps `x-egress-proxy: <name>` on every forwarded
/// request so the upstream can report which egress carried it — the same fact a real
/// provider would see as a different source address.
struct ForwardProxy {
    addr: std::net::SocketAddr,
    log: Arc<ProxyLog>,
}

impl ForwardProxy {
    async fn start(name: &'static str) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind proxy");
        let addr = listener.local_addr().expect("proxy addr");
        let log = Arc::new(ProxyLog::default());
        let shared = Arc::clone(&log);
        tokio::spawn(async move {
            loop {
                let Ok((client, _)) = listener.accept().await else {
                    return;
                };
                shared.connections.fetch_add(1, Ordering::SeqCst);
                let log = Arc::clone(&shared);
                tokio::spawn(async move {
                    let _ = serve_client(client, name, log).await;
                });
            }
        });
        Self { addr, log }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn connections(&self) -> usize {
        self.log.connections.load(Ordering::SeqCst)
    }

    fn requests(&self) -> Vec<String> {
        self.log.requests.lock().expect("requests").clone()
    }

    fn authorities(&self) -> Vec<String> {
        self.log.authorities.lock().expect("authorities").clone()
    }
}

fn head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

/// One client connection: read exactly the request head, then pipe bytes both ways.
///
/// The body is never parsed — a proxy that buffered it would break streaming, and the
/// gateway's uploads are large — so everything past the head is forwarded verbatim.
async fn serve_client(
    mut client: tokio::net::TcpStream,
    name: &'static str,
    log: Arc<ProxyLog>,
) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(end) = head_end(&buf) {
            break end;
        }
        let read = client.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..read]);
        // A head this large is not a request this proxy knows how to forward.
        if buf.len() > 64 * 1024 {
            return Ok(());
        }
    };
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let rest = buf[end + 4..].to_vec();

    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();
    let headers: Vec<String> = lines.map(str::to_string).collect();

    if method == "CONNECT" {
        log.requests
            .lock()
            .expect("requests")
            .push(format!("CONNECT {target}"));
        log.authorities
            .lock()
            .expect("authorities")
            .push(target.clone());
        let mut upstream = tokio::net::TcpStream::connect(&target).await?;
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
        return Ok(());
    }

    let Some(absolute) = target.strip_prefix("http://") else {
        // Origin-form would mean the client already addressed the upstream directly; this
        // proxy has nothing useful to do with it.
        client
            .write_all(b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\n\r\n")
            .await?;
        return Ok(());
    };
    let (authority, path) = match absolute.split_once('/') {
        Some((authority, path)) => (authority.to_string(), format!("/{path}")),
        None => (absolute.to_string(), "/".to_string()),
    };
    log.requests
        .lock()
        .expect("requests")
        .push(format!("{method} {path}"));
    log.authorities
        .lock()
        .expect("authorities")
        .push(authority.clone());

    let mut upstream = tokio::net::TcpStream::connect(&authority).await?;
    // Origin-form, the client's own headers, the egress marker, and `connection: close`.
    // Closing after the exchange makes the next request re-enter the proxy (and be stamped
    // again) instead of being piped opaquely through a reused tunnel.
    let mut forwarded = format!("{method} {path} {version}\r\n");
    for header in &headers {
        let name = header.split(':').next().unwrap_or_default();
        if name.eq_ignore_ascii_case("connection") || name.eq_ignore_ascii_case("proxy-connection")
        {
            continue;
        }
        forwarded.push_str(header);
        forwarded.push_str("\r\n");
    }
    forwarded.push_str(&format!(
        "x-egress-proxy: {name}\r\nconnection: close\r\n\r\n"
    ));
    upstream.write_all(forwarded.as_bytes()).await?;
    if !rest.is_empty() {
        upstream.write_all(&rest).await?;
    }
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

// ---------------------------------------------------------------------- harness

struct Harness {
    state: Arc<AppState>,
    router: Router,
}

fn config() -> Config {
    Config {
        host: "127.0.0.1".parse().expect("ip"),
        port: 0,
        data_dir: std::env::temp_dir().join(format!("rrouter-egress-{}", uuid::Uuid::new_v4())),
        master_key: None,
        require_api_key: false,
        request_timeout_secs: 5,
        connect_timeout_secs: 2,
        log_filter: "rrouter=error".into(),
        static_dir: None,
        cors_origins: vec![],
        allow_remote_admin: false,
        admin_token: None,
        max_concurrency: 0,
        max_attempts: 4,
        // No same-account retry: a proxy failure must fall through to the next account (or
        // fail the request), not be retried against the same unusable egress.
        retry_attempts: 0,
        retry_base_ms: 200,
        retry_max_ms: 5000,
        shutdown_grace_secs: 20,
        strategy: rrouter_router::Strategy::Priority,
        cache: rrouter_gateway::config::CacheMode::Off,
        cache_ttl_secs: 300,
        cache_max_entries: 1000,
        usage_retention_days: 0,
        log_format: rrouter_gateway::config::LogFormat::Text,
        access_log: false,
        hedge_ms: 0,
        canary: None,
        subscription_reserve_percent: 90,
        semantic_cache: rrouter_gateway::config::CacheMode::Off,
        semantic_cache_threshold: 0.92,
        semantic_embedding_model: None,
        semantic_cache_ttl_secs: 3600,
        semantic_cache_max_entries: 1000,
    }
}

fn harness() -> Harness {
    let config = config();
    let data_dir = config.data_dir.clone();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("client");
    let state = Arc::new(AppState {
        store: Arc::new(Store::open_in_memory().expect("store")),
        catalog: Arc::new(Catalog::builtin()),
        refresher: Arc::new(rrouter_auth::Refresher::new(http.clone())),
        planner: Planner::new(),
        dispatcher: Arc::new(
            Dispatcher::new(Breaker::new()).with_max_attempts(config.max_attempts),
        ),
        http,
        proxy_clients: Default::default(),
        config: rrouter_gateway::config::ConfigSnapshot::from(config),
        started: std::time::Instant::now(),
        oauth: rrouter_auth::OAuthFlows::new(),
        counters: Default::default(),
        envelope_drops: Default::default(),
        providers: Default::default(),
        concurrency: std::sync::RwLock::new(None),
        auth: rrouter_gateway::auth::Auth::load(&data_dir, false),
    });
    let router = app(Arc::clone(&state));
    Harness { state, router }
}

impl Harness {
    /// One `openai` account pointed at `upstream`, optionally leaving through `proxy`.
    async fn connect(&self, upstream: &Upstream, proxy: Option<&ForwardProxy>) -> uuid::Uuid {
        let creds = Credentials {
            api_key: Some("test-key".into()),
            base_url_override: Some(upstream.url.clone()),
            proxy_url: proxy.map(ForwardProxy::url),
            ..Credentials::default()
        };
        self.state
            .store
            .create_connection("openai", "api_key", creds, Some("egress"))
            .await
            .expect("connection")
            .id
    }

    async fn call(&self, req: Request<Body>) -> (StatusCode, Value) {
        let res = self.router.clone().oneshot(req).await.expect("oneshot");
        let status = res.status();
        let bytes = res.into_body().collect().await.expect("body").to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// One chat request through the whole gateway.
    async fn chat(&self) -> (StatusCode, Value) {
        self.call(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-4.1",
                        "messages": [{"role": "user", "content": "hi"}]
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
    }

    /// One images passthrough request: the path that plans without conversation affinity,
    /// which is what makes account rotation observable here.
    async fn images(&self) -> (StatusCode, Value) {
        self.call(
            Request::builder()
                .method("POST")
                .uri("/v1/images/generations")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"model": "openai/gpt-image-1", "prompt": "a cat", "n": 1}).to_string(),
                ))
                .expect("request"),
        )
        .await
    }

    async fn api(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        let request = builder
            .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
            .expect("request");
        self.call(request).await
    }
}

// ------------------------------------------------------------------------ tests

/// (a) A connection with `proxy_url` reaches the upstream *via* the proxy: the proxy saw
/// the request, and the only request the upstream received carries that proxy's marker.
#[tokio::test]
async fn an_account_with_a_proxy_leaves_through_it() {
    let upstream = Upstream::start().await;
    let proxy = ForwardProxy::start("alpha").await;
    let h = harness();
    h.connect(&upstream, Some(&proxy)).await;

    let (status, body) = h.chat().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["choices"][0]["message"]["content"], json!("ok"));

    assert_eq!(proxy.connections(), 1, "the proxy was never dialled");
    assert_eq!(
        proxy.requests(),
        vec!["POST /v1/chat/completions".to_string()],
        "the upstream path must survive the rewrite"
    );
    assert_eq!(
        proxy.authorities(),
        vec![upstream
            .url
            .strip_prefix("http://")
            .expect("origin")
            .to_string()],
        "the proxy forwarded to the connection's own base URL"
    );
    assert_eq!(upstream.hits(), 1);
    assert_eq!(
        upstream.vias(),
        vec!["alpha".to_string()],
        "the upstream must have been reached through the proxy, not directly"
    );
}

/// (b) A connection without a proxy never touches one: the same request reaches the
/// upstream directly, and the proxy records no connection at all.
#[tokio::test]
async fn an_account_without_a_proxy_never_touches_it() {
    let upstream = Upstream::start().await;
    let proxy = ForwardProxy::start("alpha").await;
    let h = harness();
    h.connect(&upstream, None).await;

    let (status, body) = h.chat().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(upstream.hits(), 1);
    assert_eq!(upstream.vias(), vec![String::new()], "no egress marker");
    assert_eq!(
        proxy.connections(),
        0,
        "a direct account must not be routed through a proxy"
    );
}

/// (c) Two accounts of the same provider with different proxies both serve, and each
/// request leaves through its own account's proxy — the rotation lands on the other
/// account, and the upstream sees a different source each time.
#[tokio::test]
async fn two_accounts_with_different_proxies_both_serve() {
    let upstream = Upstream::start().await;
    let alpha = ForwardProxy::start("alpha").await;
    let beta = ForwardProxy::start("beta").await;
    let h = harness();
    // Equal priority: the planner's cursor starts each plan on a different account.
    h.connect(&upstream, Some(&alpha)).await;
    h.connect(&upstream, Some(&beta)).await;

    // The images passthrough plans without conversation affinity, so successive requests
    // rotate deterministically: the first lands on one account, the second on the other.
    let (status, body) = h.images().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = h.images().await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(upstream.hits(), 2);
    let mut vias = upstream.vias();
    vias.sort();
    assert_eq!(
        vias,
        vec!["alpha".to_string(), "beta".to_string()],
        "each account must leave through its own proxy"
    );
    assert_eq!(alpha.connections(), 1, "proxy alpha served one request");
    assert_eq!(beta.connections(), 1, "proxy beta served one request");

    // Both accounts keep their own URL: no account inherited the other's egress, and the
    // masked view (no userinfo in these URLs) still tells the two proxies apart.
    let mut urls: Vec<String> = h
        .state
        .store
        .list_connections(Some("openai"))
        .await
        .expect("connections")
        .iter()
        .filter_map(|conn| conn.credentials.proxy_masked())
        .collect();
    urls.sort();
    let mut expected = vec![alpha.url(), beta.url()];
    expected.sort();
    assert_eq!(urls, expected);
}

/// (d) A proxy URL reqwest refuses fails the attempt instead of connecting directly. The
/// upstream sees nothing: the account stays behind the address the operator separated it
/// to, even when that address is unusable.
#[tokio::test]
async fn an_unparseable_proxy_url_fails_the_attempt_without_a_direct_connection() {
    let upstream = Upstream::start().await;
    let h = harness();
    let creds = Credentials {
        api_key: Some("test-key".into()),
        base_url_override: Some(upstream.url.clone()),
        // Unparseable on purpose, and carrying userinfo so the masking can be checked.
        proxy_url: Some("http://user:secret@[::1:bad".into()),
        ..Credentials::default()
    };
    h.state
        .store
        .create_connection("openai", "api_key", creds, Some("broken"))
        .await
        .expect("connection");

    let (status, body) = h.chat().await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(upstream.hits(), 0, "a broken proxy must never fall back");
    let encoded = body.to_string();
    assert!(
        !encoded.contains("secret"),
        "the masked URL must not leak the password: {encoded}"
    );
    assert!(
        encoded.contains("proxy"),
        "the error should name the proxy as the cause: {encoded}"
    );
}

/// The API round-trips the proxy and the free-mode knobs, and only ever answers with the
/// masked form: `user:pass@` is credential material like any other.
#[tokio::test]
async fn connection_api_masks_the_proxy_and_round_trips_free_mode() {
    let h = harness();
    let (status, created) = h
        .api(
            "POST",
            "/api/connections",
            Some(json!({
                "provider": "openai",
                "auth_type": "api_key",
                // A closed port: the background model fetch fails locally, never online.
                "credentials": {"api_key": "sk-egress", "base_url_override": "http://127.0.0.1:1"},
                "proxy": "http://user:pass@127.0.0.1:3128",
                "free_mode": true,
                "free_rpm": 5,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["proxy"], json!("http://***@127.0.0.1:3128"));
    assert_eq!(created["free_rpm"], json!(5));
    assert!(
        !created.to_string().contains("user:pass"),
        "the raw proxy URL must never be echoed: {created}"
    );
    let id = created["id"].as_str().expect("id").to_string();

    // `free_mode: false` and `free_rpm: null` are how free mode is left behind; the proxy
    // is cleared with an empty string.
    let (status, updated) = h
        .api(
            "PATCH",
            &format!("/api/connections/{id}"),
            Some(json!({"free_mode": false, "free_rpm": null, "proxy": ""})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["proxy"], Value::Null, "an empty proxy clears it");
    assert_eq!(updated["free_rpm"], Value::Null, "a null cap clears it");

    // A socks proxy is stored verbatim and masked the same way; the scheme survives
    // because it is what tells the operator which hop the account uses.
    let (status, updated) = h
        .api(
            "PATCH",
            &format!("/api/connections/{id}"),
            Some(json!({"proxy_url": "socks5h://127.0.0.1:1080", "free_rpm": 9})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["proxy"], json!("socks5h://127.0.0.1:1080"));
    assert_eq!(updated["free_rpm"], json!(9));

    let stored = h
        .state
        .store
        .get_connection(uuid::Uuid::parse_str(&id).expect("uuid"))
        .await
        .expect("read")
        .expect("row");
    assert_eq!(
        stored.credentials.proxy_url.as_deref(),
        Some("socks5h://127.0.0.1:1080")
    );
    assert!(!stored.credentials.free_mode, "free mode was turned off");
    assert_eq!(stored.credentials.free_rpm, Some(9));

    // No response in this flow carries the API key either.
    let (_, list) = h.api("GET", "/api/connections", None).await;
    assert!(!list.to_string().contains("sk-egress"), "{list}");
}
