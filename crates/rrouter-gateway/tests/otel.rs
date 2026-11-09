//! Optional OTLP/HTTP export (`otel` feature).
//!
//! Default build: assert that enabling the feature is what brings the OpenTelemetry
//! crates in, by reading `cargo tree`. Feature build: validate settings, export one span
//! to a local mock collector and verify the protobuf request, and prove shutdown flushes.

#[cfg(not(feature = "otel"))]
#[test]
fn the_default_build_links_no_otel_crates() {
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let output = std::process::Command::new(env!("CARGO"))
        .args([
            "tree",
            "-p",
            "rrouter-gateway",
            "-e",
            "normal",
            "--offline",
            "--manifest-path",
            manifest,
        ])
        .output()
        .expect("run cargo tree");
    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8_lossy(&output.stdout);
    assert!(
        !tree.contains("opentelemetry"),
        "the default feature set must not link OpenTelemetry crates:\n{tree}"
    );
}

#[cfg(feature = "otel")]
mod enabled {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::body::Bytes;
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::Router;
    use rrouter_core::Dialect;
    use rrouter_gateway::config::OtelConfig;
    use rrouter_gateway::telemetry;
    use tracing_subscriber::layer::SubscriberExt;

    #[derive(Default, Clone)]
    struct Captured {
        content_type: String,
        body: Vec<u8>,
    }

    type Sink = Arc<Mutex<Vec<Captured>>>;

    async fn collect(
        State(sink): State<Sink>,
        headers: HeaderMap,
        body: Bytes,
    ) -> impl IntoResponse {
        let content_type = headers
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        sink.lock().expect("sink").push(Captured {
            content_type,
            body: body.to_vec(),
        });
        // Empty protobuf ExportTraceServiceResponse is valid.
        axum::http::StatusCode::OK
    }

    async fn collector() -> (Sink, String) {
        let sink: Sink = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind collector");
        let addr = listener.local_addr().expect("addr");
        let router = Router::new()
            .route("/v1/traces", post(collect))
            .with_state(Arc::clone(&sink));
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        (sink, format!("http://{addr}/v1/traces"))
    }

    fn config(endpoint: &str) -> OtelConfig {
        OtelConfig::validate(endpoint, "rrouter-test", 1.0).expect("valid config")
    }

    /// One request span, the shape the proxy creates. Returns after the span closes.
    fn emit_request_span() {
        let started = telemetry::start(None, None);
        let span =
            telemetry::request_span(uuid::Uuid::new_v4(), Dialect::OpenAiChat, &started.context);
        let _guard = span.enter();
        tracing::info!("inside the request");
        let attempt = telemetry::attempt_span(&span, "openai", "gpt-test", 1);
        let _attempt_guard = attempt.enter();
        tracing::info!("inside the attempt");
    }

    #[test]
    fn invalid_settings_are_rejected() {
        assert!(OtelConfig::validate("", "rrouter", 1.0).is_err());
        assert!(OtelConfig::validate("ftp://collector:4318/v1/traces", "rrouter", 1.0).is_err());
        assert!(OtelConfig::validate("not a url", "rrouter", 1.0).is_err());
        assert!(OtelConfig::validate("http://collector:4318/v1/traces", "", 1.0).is_err());
        assert!(OtelConfig::validate("http://collector:4318/v1/traces", "rrouter", 1.5).is_err());
        assert!(
            OtelConfig::validate("http://collector:4318/v1/traces", "rrouter", f64::NAN).is_err()
        );
        assert!(OtelConfig::validate("https://collector:4318/v1/traces", "rrouter", 0.25).is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_span_is_exported_as_otlp_http_and_flushed() {
        let (sink, endpoint) = collector().await;
        let exporter =
            telemetry::otel::Exporter::from_config(&config(&endpoint)).expect("exporter");
        let subscriber = tracing_subscriber::registry().with(exporter.layer());
        tracing::subscriber::with_default(subscriber, emit_request_span);

        exporter.force_flush().expect("flush");
        let captured = sink.lock().expect("sink").clone();
        assert_eq!(captured.len(), 1, "one OTLP batch request");
        assert!(
            captured[0].content_type.contains("protobuf"),
            "OTLP/HTTP protobuf content type, got {:?}",
            captured[0].content_type
        );
        assert!(!captured[0].body.is_empty());
        let body = String::from_utf8_lossy(&captured[0].body);
        assert!(
            body.contains("rrouter-test"),
            "service name is in the resource"
        );
        exporter.shutdown().expect("shutdown");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_flushes_buffered_spans() {
        let (sink, endpoint) = collector().await;
        let exporter =
            telemetry::otel::Exporter::from_config(&config(&endpoint)).expect("exporter");
        let subscriber = tracing_subscriber::registry().with(exporter.layer());
        tracing::subscriber::with_default(subscriber, emit_request_span);

        // No explicit flush: shutdown is the last chance for the buffer.
        exporter.shutdown().expect("shutdown flushes");
        let captured = sink.lock().expect("sink").clone();
        assert!(!captured.is_empty(), "shutdown exported the buffered span");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unreachable_collector_does_not_panic_or_hang() {
        let exporter =
            telemetry::otel::Exporter::from_config(&config("http://127.0.0.1:1/v1/traces"))
                .expect("exporter builds even when the collector is down");
        let subscriber = tracing_subscriber::registry().with(exporter.layer());
        tracing::subscriber::with_default(subscriber, emit_request_span);
        // The export fails, but the process keeps running and shutdown is bounded by the
        // exporter's own timeout.
        let _ = exporter.force_flush();
        let _ = exporter.shutdown();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
