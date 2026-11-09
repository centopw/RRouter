//! Plumbing for provider-native endpoints the gateway forwards verbatim.
//!
//! Some capabilities are the provider's own wire format — `/v1/files`, `/v1/batches`,
//! `/v1/rerank` — and translating them would mean inventing a second canonical model for
//! no gain. This module owns the parts every such route repeats: resolving the account that
//! serves a capability, building its headers, joining the reviewed endpoint with a
//! sub-path and query, and turning an upstream failure into the shared `Error::Upstream`
//! (which keeps the provider's payload for the client-facing envelope).

use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use rrouter_auth::{Credentials, ProviderSpec};
use rrouter_core::Error;
use rrouter_store::ConnectionScope;

use crate::state::{AppState, Caller};

/// Upstream error bodies are truncated to this many bytes before they are stored or echoed.
const MAX_ERROR_BODY: usize = 4096;

/// A provider capability that is forwarded rather than translated.
#[derive(Clone, Copy)]
pub(crate) enum Capability {
    Files,
    Batches,
}

impl Capability {
    /// The provider's reviewed endpoint, when it declares one.
    pub(crate) fn endpoint(self, spec: &ProviderSpec) -> Option<&'static str> {
        match self {
            Self::Files => spec.files_url(),
            Self::Batches => spec.batches_url(),
        }
    }

    /// What a `501` says the provider does not serve.
    pub(crate) fn unsupported(self) -> &'static str {
        match self {
            Self::Files => "file uploads for this provider",
            Self::Batches => "batch jobs for this provider",
        }
    }
}

/// One resolved upstream account for a capability.
pub(crate) struct Target {
    pub spec: &'static ProviderSpec,
    pub credentials: Credentials,
    /// Reviewed endpoint, moved to the connection's origin and carrying its query.
    pub base: String,
}

/// The account that will serve `capability`, or `None` when nobody can.
///
/// Resolution follows the routing rules: only connections this caller may use (a team key
/// sees that team's accounts, never the instance's shared rows) and only providers that
/// declare the endpoint. `requested` pins a provider by id and is validated first, so a
/// typo is a `400` rather than a silent fallback to some other account.
pub(crate) async fn resolve_target(
    state: &AppState,
    caller: &Caller,
    requested: Option<&str>,
    capability: Capability,
) -> Result<Option<Target>, Error> {
    if let Some(provider) = requested {
        if rrouter_auth::registry::get(provider).is_none() {
            return Err(Error::BadRequest(format!("unknown provider {provider}")));
        }
    }
    let connections = state
        .store
        .list_connections_scoped(ConnectionScope::for_team(caller.team_id()), requested)
        .await?;
    let Some(conn) = connections.into_iter().find(|conn| {
        conn.is_active
            && rrouter_auth::registry::get(&conn.provider)
                .is_some_and(|spec| capability.endpoint(spec).is_some())
    }) else {
        return Ok(None);
    };
    let spec = rrouter_auth::registry::get(&conn.provider).ok_or_else(|| Error::NoRoute {
        model: String::new(),
        detail: format!("unknown provider {}", conn.provider),
    })?;
    let native = capability.endpoint(spec).ok_or_else(|| Error::NoRoute {
        model: String::new(),
        detail: format!("{} has no {}", conn.provider, capability.unsupported()),
    })?;
    let credentials = state
        .refresher
        .fresh(state.store.as_ref(), spec, conn.id)
        .await?;
    let base =
        rrouter_auth::capability_endpoint(native, credentials.base_url_override.as_deref(), spec);
    Ok(Some(Target {
        spec,
        credentials,
        base,
    }))
}

/// Provider headers (auth, mandatory headers, the client's allowlisted betas).
pub(crate) fn outbound_headers(
    spec: &ProviderSpec,
    creds: &Credentials,
    client: &HeaderMap,
) -> Result<HeaderMap, Error> {
    let mut pairs = rrouter_auth::build_headers(spec, creds)?;
    rrouter_auth::merge_client_headers(spec, &mut pairs, client);
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            headers.append(name, value);
        }
    }
    Ok(headers)
}

/// The reviewed endpoint plus a sub-path and the client's own query.
///
/// The query is appended verbatim (`&` vs `?` chosen by what the endpoint already has), so
/// a provider's pagination or filter parameters reach it byte for byte.
pub(crate) fn sub_url(base: &str, suffix: &str, query: Option<&str>) -> String {
    let mut url = format!("{base}{suffix}");
    if let Some(query) = query.filter(|query| !query.is_empty()) {
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(query);
    }
    url
}

/// The client's query with this gateway's own `provider` pin removed.
///
/// Everything else is kept byte for byte: a provider's filter, cursor and order parameters
/// are part of its API and re-encoding them would change the request.
pub(crate) fn client_query(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    let kept: Vec<&str> = raw
        .split('&')
        .filter(|pair| {
            !pair.is_empty()
                && pair.split_once('=').map(|(key, _)| key).unwrap_or(pair) != "provider"
        })
        .collect();
    (!kept.is_empty()).then(|| kept.join("&"))
}

/// Forward one request and relay the provider's answer as bytes.
///
/// A non-2xx answer becomes [`Error::Upstream`] with the parsed payload intact, so the
/// client-facing envelope keeps the provider's `type`/`code`/`message` instead of a
/// formatted summary. A success relays the status, the content type and the exact bytes —
/// no re-serialization, which is what keeps a batch file or a file download byte-safe.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn relay(
    state: &AppState,
    target: &Target,
    method: reqwest::Method,
    suffix: &str,
    query: Option<&str>,
    content_type: Option<&str>,
    body: Option<bytes::Bytes>,
    client: &HeaderMap,
) -> Result<Response, Error> {
    let url = sub_url(&target.base, suffix, query);
    let mut request = state
        .http
        .request(method, &url)
        .timeout(state.request_timeout())
        .headers(outbound_headers(target.spec, &target.credentials, client)?);
    if let Some(content_type) = content_type {
        request = request.header(CONTENT_TYPE, content_type);
    }
    if let Some(body) = body {
        request = request.body(body);
    }
    let response = request.send().await.map_err(transport_error)?;
    let status = response.status();
    let content_type = response.headers().get(CONTENT_TYPE).cloned();
    let bytes = response.bytes().await.map_err(transport_error)?;
    if !status.is_success() {
        return Err(upstream_error(target.spec.id, status.as_u16(), &bytes));
    }
    let mut out = Response::new(axum::body::Body::from(bytes));
    *out.status_mut() = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::OK);
    if let Some(content_type) = content_type {
        out.headers_mut().insert(CONTENT_TYPE, content_type);
    }
    Ok(out)
}

/// Forward one request, checking the status now and streaming the body to the client.
///
/// Used for a file's content, where the body is the provider's file and buffering it would
/// put an arbitrary file in gateway memory.
pub(crate) async fn relay_stream(
    state: &AppState,
    target: &Target,
    suffix: &str,
    query: Option<&str>,
    client: &HeaderMap,
) -> Result<Response, Error> {
    let url = sub_url(&target.base, suffix, query);
    let response = state
        .http
        .get(&url)
        .timeout(state.request_timeout())
        .headers(outbound_headers(target.spec, &target.credentials, client)?)
        .send()
        .await
        .map_err(transport_error)?;
    let status = response.status();
    if !status.is_success() {
        let bytes = response.bytes().await.map_err(transport_error)?;
        return Err(upstream_error(target.spec.id, status.as_u16(), &bytes));
    }
    let content_type = response.headers().get(CONTENT_TYPE).cloned();
    let mut out = Response::new(axum::body::Body::from_stream(response.bytes_stream()));
    *out.status_mut() = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::OK);
    if let Some(content_type) = content_type {
        out.headers_mut().insert(CONTENT_TYPE, content_type);
    }
    Ok(out)
}

/// An upstream failure with its body and parsed payload intact.
pub(crate) fn upstream_error(provider: &str, status: u16, bytes: &[u8]) -> Error {
    let text = String::from_utf8_lossy(bytes);
    let body = if text.len() > MAX_ERROR_BODY {
        let mut cut = MAX_ERROR_BODY;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text[..cut].to_string()
    } else {
        text.into_owned()
    };
    Error::Upstream {
        provider: provider.to_string(),
        status,
        body,
        payload: serde_json::from_slice(bytes).ok(),
    }
}

pub(crate) fn transport_error(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::Timeout(e.to_string())
    } else {
        Error::Transport(e.to_string())
    }
}

/// `?provider=` from a raw query, for pinning the account that serves the call.
pub(crate) fn query_provider(raw: Option<&str>) -> Option<String> {
    raw?.split('&')
        .find_map(|pair| pair.strip_prefix("provider="))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// One provider-assigned id from a path segment.
///
/// Provider ids are opaque tokens (`file-abc123`, `batch_xyz`). Anything that is not a
/// bounded token — a slash from a decoded `%2F`, a traversal segment, an oversized value —
/// is refused before a URL is built, so the forwarded path cannot escape the endpoint.
pub(crate) fn resource_id<'a>(kind: &str, raw: &'a str) -> Result<&'a str, Error> {
    let valid = !raw.is_empty()
        && raw.len() <= 128
        && raw != "."
        && raw != ".."
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'));
    if valid {
        Ok(raw)
    } else {
        Err(Error::BadRequest(format!("invalid {kind} id")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_are_tokens() {
        for ok in ["file-abc123", "batch_xyz.1", "a:b"] {
            assert_eq!(resource_id("file", ok).unwrap(), ok);
        }
        for bad in [
            "",
            ".",
            "..",
            "../etc/passwd",
            "a/b",
            "a b",
            "x".repeat(129).as_str(),
        ] {
            let err = resource_id("file", bad).expect_err(bad);
            assert!(matches!(err, Error::BadRequest(_)), "{bad}");
        }
    }

    #[test]
    fn the_provider_pin_is_not_forwarded() {
        assert_eq!(
            client_query(Some("purpose=batch&limit=2")).unwrap(),
            "purpose=batch&limit=2"
        );
        assert_eq!(
            client_query(Some("provider=openai&purpose=batch")).unwrap(),
            "purpose=batch"
        );
        assert_eq!(client_query(Some("provider=openai")), None);
        assert_eq!(client_query(Some("")), None);
        assert_eq!(client_query(None), None);
    }

    #[test]
    fn a_sub_path_and_query_join_the_endpoint() {
        assert_eq!(
            sub_url("https://api.openai.com/v1/files", "", None),
            "https://api.openai.com/v1/files"
        );
        assert_eq!(
            sub_url("https://api.openai.com/v1/files", "/file-1/content", None),
            "https://api.openai.com/v1/files/file-1/content"
        );
        assert_eq!(
            sub_url("https://api.openai.com/v1/files", "", Some("limit=2")),
            "https://api.openai.com/v1/files?limit=2"
        );
        // A connection-level query (api-version) already opened the query string.
        assert_eq!(
            sub_url(
                "https://tenant.example/v1/files?api-version=1",
                "",
                Some("limit=2")
            ),
            "https://tenant.example/v1/files?api-version=1&limit=2"
        );
    }
}
