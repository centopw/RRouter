// Reviewed community provider manifests: the contract `providers/schema.json` describes.
//
// This file is compiled twice — as a module of this crate (so the round-trip tests can
// exercise it) and `include!`d by `build.rs` (so the generator uses the same rules). One
// validator means a manifest the tests accept is a manifest the build accepts, and a
// manifest with an unknown key can never reach the generated provider table: the build
// script refuses it instead of silently ignoring the field.
//
// Everything here is pure `serde_json` + `std` and uses fully-qualified paths, because
// `include!` pastes it into the build script's root module.

/// One reviewed manifest, validated.
///
/// Fields the compiled [`ProviderSpec`](crate::registry::ProviderSpec) does not carry
/// (website, docs, key_url, notes) are review metadata and are intentionally absent.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct Manifest {
    pub id: String,
    pub display: String,
    pub base_url: String,
    /// The host above is a placeholder, so every connection supplies its own base URL.
    pub base_url_requires_override: bool,
    pub auth: Auth,
    /// Header that carries the key for [`Auth::ApiKeyHeader`]; empty otherwise.
    pub auth_header: String,
    pub default_dialect: DialectName,
    pub models_url: Option<String>,
    pub responses_url: Option<String>,
    pub messages_url: Option<String>,
    /// Native rerank endpoint, when the provider serves `/v1/rerank` itself.
    pub rerank_url: Option<String>,
    /// OpenAI-compatible file API (`POST /v1/files`, list, retrieve, content).
    pub files_url: Option<String>,
    /// OpenAI-compatible batch API (`POST /v1/batches`, list, retrieve, cancel).
    pub batches_url: Option<String>,
    pub images_url: Option<String>,
    pub audio_speech_url: Option<String>,
    pub audio_transcriptions_url: Option<String>,
    pub default_max_tokens: Option<u64>,
    pub stream_only: bool,
    pub extra_headers: Vec<(String, String)>,
    pub query: Vec<(String, String)>,
    pub max_tokens_key: MaxTokensName,
    pub passthrough_headers: Vec<String>,
}

/// How the credential is presented on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Auth {
    Bearer,
    XApiKey,
    ApiKeyHeader,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum DialectName {
    Chat,
    Responses,
    Messages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum MaxTokensName {
    MaxTokens,
    MaxCompletionTokens,
}

/// Every key a manifest may contain. Anything else is a build failure: an ignored key is a
/// reviewed endpoint that silently never takes effect.
#[allow(dead_code)]
pub const KNOWN_KEYS: &[&str] = &[
    "id",
    "display",
    "website",
    "docs",
    "key_url",
    "base_url",
    "base_url_requires_override",
    "auth",
    "auth_header",
    "default_dialect",
    "models_url",
    "responses_url",
    "messages_url",
    "rerank_url",
    "files_url",
    "batches_url",
    "images_url",
    "audio_speech_url",
    "audio_transcriptions_url",
    "default_max_tokens",
    "stream_only",
    "extra_headers",
    "query",
    "max_tokens_key",
    "passthrough_headers",
    "endpoints",
    "capabilities",
    "notes",
];

/// Headers a manifest may never set: credentials belong in the credential store, and
/// hop-by-hop headers are the HTTP client's business.
#[allow(dead_code)]
const FORBIDDEN_HEADERS: &[&str] = &[
    "authorization",
    "api-key",
    "x-api-key",
    "cookie",
    "set-cookie",
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "proxy-authorization",
];

/// Header names that can never be the auth carrier: Authorization has its own style, and
/// the rest belong to the HTTP client. `api-key`/`x-api-key` are deliberately absent —
/// naming one is exactly how [`Auth::ApiKeyHeader`] carries a key.
#[allow(dead_code)]
const AUTH_HEADER_FORBIDDEN: &[&str] = &[
    "authorization",
    "cookie",
    "set-cookie",
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "proxy-authorization",
];

/// Query-parameter names that would put a credential in a URL (and therefore in logs).
#[allow(dead_code)]
const FORBIDDEN_QUERY_KEYS: &[&str] = &[
    "api-key",
    "api_key",
    "apikey",
    "key",
    "token",
    "access_token",
    "authorization",
    "password",
    "secret",
    "sig",
    "signature",
];

/// The only client beta headers there are. A manifest cannot invent one, so a forwarded
/// header is always a reviewed protocol flag.
#[allow(dead_code)]
const PASSTHROUGH_ALLOWED: &[&str] = &["anthropic-beta", "openai-beta"];

/// Parse and validate one manifest. The error names the offending field and is shown
/// verbatim in the build failure, so a manifest author can fix it without reading code.
#[allow(dead_code)]
pub fn parse(value: &serde_json::Value) -> Result<Manifest, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "manifest must be a JSON object".to_string())?;

    for key in object.keys() {
        if !KNOWN_KEYS.contains(&key.as_str()) {
            return Err(format!("unknown key `{key}` (see providers/schema.json)"));
        }
    }

    let id = required_string(object, "id")?;
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(format!("id `{id}` must be a lowercase slug"));
    }

    let display = required_string(object, "display")?;
    let base_url = https_url(object, "base_url")?;
    if base_url.ends_with('/') {
        return Err("base_url must not end with a slash".to_string());
    }
    let base_url_requires_override = match object.get("base_url_requires_override") {
        None | Some(serde_json::Value::Null) => false,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "base_url_requires_override must be a boolean".to_string())?,
    };
    // The flag says "this host is a placeholder". A real endpoint that sets it would force
    // every connection to override a host that already works, so the two must agree.
    if base_url_requires_override && !is_placeholder_host(&base_url) {
        return Err(format!(
            "base_url_requires_override is set but `{}` is not a placeholder host              (first label must be resource, your-resource or placeholder)",
            host_of(&base_url).unwrap_or(&base_url)
        ));
    }

    let auth = match required_string(object, "auth")?.as_str() {
        "bearer" => Auth::Bearer,
        "x-api-key" => Auth::XApiKey,
        "api-key" => Auth::ApiKeyHeader,
        "none" => Auth::None,
        other => {
            return Err(format!(
                "auth `{other}` is not one of bearer, x-api-key, api-key, none"
            ))
        }
    };
    let auth_header = match optional_string(object, "auth_header")? {
        Some(header) => {
            if auth != Auth::ApiKeyHeader {
                return Err(format!(
                    "auth_header `{header}` is only valid with auth: api-key"
                ));
            }
            if !is_header_token(&header) {
                return Err(format!("auth_header `{header}` is not a header name"));
            }
            if AUTH_HEADER_FORBIDDEN.contains(&header.as_str()) {
                return Err(format!(
                    "auth_header `{header}` is refused: use auth: bearer for Authorization"
                ));
            }
            header
        }
        None if auth == Auth::ApiKeyHeader => "api-key".to_string(),
        None => String::new(),
    };

    let default_dialect = match required_string(object, "default_dialect")?.as_str() {
        "chat" => DialectName::Chat,
        "responses" => DialectName::Responses,
        "messages" | "anthropic" => DialectName::Messages,
        other => {
            return Err(format!(
                "default_dialect `{other}` is not one of chat, responses, messages"
            ))
        }
    };

    let max_tokens_key = match optional_string(object, "max_tokens_key")?.as_deref() {
        None | Some("max_tokens") => MaxTokensName::MaxTokens,
        Some("max_completion_tokens") => MaxTokensName::MaxCompletionTokens,
        Some(other) => {
            return Err(format!(
                "max_tokens_key `{other}` is not one of max_tokens, max_completion_tokens"
            ))
        }
    };

    let mut passthrough_headers = Vec::new();
    if let Some(items) = object.get("passthrough_headers") {
        let items = items
            .as_array()
            .ok_or_else(|| "passthrough_headers must be an array".to_string())?;
        for item in items {
            let name = item
                .as_str()
                .ok_or_else(|| "passthrough_headers entries must be strings".to_string())?;
            if !PASSTHROUGH_ALLOWED.contains(&name) {
                return Err(format!(
                    "passthrough_headers `{name}` is not a reviewed protocol beta header"
                ));
            }
            if passthrough_headers.iter().any(|seen| seen == name) {
                return Err(format!("passthrough_headers `{name}` is duplicated"));
            }
            passthrough_headers.push(name.to_string());
        }
        if passthrough_headers.len() > PASSTHROUGH_ALLOWED.len() {
            return Err("passthrough_headers lists more headers than exist".to_string());
        }
    }

    let extra_headers = header_map(object, "extra_headers")?;
    let query = query_params(object, "query")?;

    let default_max_tokens = match object.get("default_max_tokens") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(
            value
                .as_u64()
                .filter(|n| *n >= 1)
                .ok_or_else(|| "default_max_tokens must be a positive integer".to_string())?,
        ),
    };

    let stream_only = match object.get("stream_only") {
        None | Some(serde_json::Value::Null) => false,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "stream_only must be a boolean".to_string())?,
    };

    Ok(Manifest {
        id,
        display,
        base_url,
        base_url_requires_override,
        auth,
        auth_header,
        default_dialect,
        models_url: optional_url(object, "models_url")?,
        responses_url: optional_url(object, "responses_url")?,
        messages_url: optional_url(object, "messages_url")?,
        rerank_url: capability_url(object, "rerank_url")?,
        files_url: capability_url(object, "files_url")?,
        batches_url: capability_url(object, "batches_url")?,
        images_url: optional_url(object, "images_url")?,
        audio_speech_url: optional_url(object, "audio_speech_url")?,
        audio_transcriptions_url: optional_url(object, "audio_transcriptions_url")?,
        default_max_tokens,
        stream_only,
        extra_headers,
        query,
        max_tokens_key,
        passthrough_headers,
    })
}

fn required_string(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<String, String> {
    optional_string(object, key)?.ok_or_else(|| format!("{key} is required"))
}

fn optional_string(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                return Err(format!("{key} must not be empty"));
            }
            Ok(Some(value.to_string()))
        }
        Some(_) => Err(format!("{key} must be a string")),
    }
}

/// A reviewed endpoint: https, absolute, and a literal (no `${…}` or `{…}` templates —
/// a template would make the compiled spec route somewhere the reviewer never saw).
fn https_url(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<String, String> {
    let url = required_string(object, key)?;
    check_url(key, &url)?;
    Ok(url)
}

fn optional_url(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<String>, String> {
    match optional_string(object, key)? {
        Some(url) => {
            check_url(key, &url)?;
            Ok(Some(url))
        }
        None => Ok(None),
    }
}

/// A capability endpoint (`rerank_url`, `files_url`, `batches_url`) is posted to verbatim,
/// so it must be exact: no trailing slash, and no query of its own (a static parameter
/// belongs in `query`, where it is reviewed).
fn capability_url(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<String>, String> {
    let Some(url) = optional_url(object, key)? else {
        return Ok(None);
    };
    if url.ends_with('/') {
        return Err(format!("{key} must not end with a slash"));
    }
    if url.contains('?') {
        return Err(format!(
            "{key} must not carry a query string; declare it in `query`"
        ));
    }
    Ok(Some(url))
}

fn check_url(key: &str, url: &str) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err(format!("{key} must be an https URL"));
    }
    if url
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '{' | '}' | '$' | '<' | '>'))
    {
        return Err(format!(
            "{key} must be a literal URL, not a template: {url}"
        ));
    }
    Ok(())
}

/// First labels that mark a documented placeholder host rather than a usable endpoint.
#[allow(dead_code)]
pub const PLACEHOLDER_HOST_LABELS: &[&str] = &["resource", "your-resource", "placeholder"];

/// Host of an absolute URL, without userinfo, port, path, query or fragment.
#[allow(dead_code)]
pub fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map(|(_, rest)| rest)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = authority.split(':').next().unwrap_or(authority);
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

/// Whether the URL's first host label is a placeholder this gateway must never call.
#[allow(dead_code)]
pub fn is_placeholder_host(url: &str) -> bool {
    host_of(url)
        .and_then(|host| host.split('.').next())
        .is_some_and(|label| {
            PLACEHOLDER_HOST_LABELS
                .iter()
                .any(|placeholder| label.eq_ignore_ascii_case(placeholder))
        })
}

fn is_header_token(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_query_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '-'))
}

/// A value that looks like a credential. A reviewed manifest is public, so a token in one
/// would be a leaked secret rather than a configuration mistake.
fn looks_like_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("bearer ")
        || lower.starts_with("basic ")
        || lower.contains("sk-")
        || lower.contains("ghp_")
        || lower.contains("aiza")
        || lower.contains("-----begin")
}

fn header_map(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Vec<(String, String)>, String> {
    let Some(value) = object.get(key) else {
        return Ok(Vec::new());
    };
    let map = value
        .as_object()
        .ok_or_else(|| format!("{key} must be an object of string values"))?;
    let mut out = Vec::with_capacity(map.len());
    for (name, value) in map {
        let name = name.to_ascii_lowercase();
        if !is_header_token(&name) {
            return Err(format!("{key}.{name} is not a header name"));
        }
        if FORBIDDEN_HEADERS.contains(&name.as_str()) {
            return Err(format!(
                "{key}.{name} is refused: credentials belong in the credential store"
            ));
        }
        let value = value
            .as_str()
            .ok_or_else(|| format!("{key}.{name} must be a string"))?;
        let value = value.trim();
        if value.is_empty() {
            return Err(format!("{key}.{name} must not be empty"));
        }
        if looks_like_secret(value) {
            return Err(format!("{key}.{name} looks like a credential"));
        }
        out.push((name, value.to_string()));
    }
    Ok(out)
}

fn query_params(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Vec<(String, String)>, String> {
    let Some(value) = object.get(key) else {
        return Ok(Vec::new());
    };
    let map = value
        .as_object()
        .ok_or_else(|| format!("{key} must be an object of string values"))?;
    if map.len() > 8 {
        return Err(format!("{key} carries at most 8 parameters"));
    }
    let mut out = Vec::with_capacity(map.len());
    for (name, value) in map {
        if !is_query_token(name) {
            return Err(format!("{key}.{name} is not a query parameter name"));
        }
        if FORBIDDEN_QUERY_KEYS.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(format!(
                "{key}.{name} is refused: credentials never belong in a URL"
            ));
        }
        let value = value
            .as_str()
            .ok_or_else(|| format!("{key}.{name} must be a string"))?;
        if !is_query_token(value) {
            return Err(format!("{key}.{name} must be a token, got `{value}`"));
        }
        out.push((name.clone(), value.to_string()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn minimal() -> serde_json::Value {
        json!({
            "id": "acme",
            "display": "Acme AI",
            "base_url": "https://api.acme.ai/v1/chat/completions",
            "auth": "bearer",
            "default_dialect": "chat"
        })
    }

    fn with(key: &str, value: serde_json::Value) -> serde_json::Value {
        let mut body = minimal();
        body[key] = value;
        body
    }

    #[test]
    fn a_minimal_manifest_takes_the_conservative_defaults() {
        let manifest = parse(&minimal()).expect("valid");
        assert_eq!(manifest.auth, Auth::Bearer);
        assert_eq!(manifest.auth_header, "");
        assert_eq!(manifest.max_tokens_key, MaxTokensName::MaxTokens);
        assert!(manifest.passthrough_headers.is_empty());
        assert!(manifest.query.is_empty());
        assert!(manifest.extra_headers.is_empty());
        assert_eq!(manifest.default_max_tokens, None);
        assert!(!manifest.stream_only);
    }

    #[test]
    fn wire_shape_fields_are_carried() {
        let mut body = minimal();
        body["max_tokens_key"] = json!("max_completion_tokens");
        body["passthrough_headers"] = json!(["anthropic-beta", "openai-beta"]);
        body["query"] = json!({"api-version": "2024-10-21"});
        let manifest = parse(&body).expect("valid");
        assert_eq!(manifest.max_tokens_key, MaxTokensName::MaxCompletionTokens);
        assert_eq!(
            manifest.passthrough_headers,
            vec!["anthropic-beta".to_string(), "openai-beta".to_string()]
        );
        assert_eq!(
            manifest.query,
            vec![("api-version".to_string(), "2024-10-21".to_string())]
        );
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let mut body = minimal();
        body["max_completion_tokens"] = json!(true);
        let err = parse(&body).expect_err("unknown key must fail");
        assert!(err.contains("unknown key `max_completion_tokens`"), "{err}");
    }

    #[test]
    fn api_key_auth_defaults_and_defends_the_header() {
        let body = with("auth", json!("api-key"));
        assert_eq!(parse(&body).expect("valid").auth_header, "api-key");

        let mut explicit = with("auth", json!("api-key"));
        explicit["auth_header"] = json!("x-custom-key");
        assert_eq!(parse(&explicit).expect("valid").auth_header, "x-custom-key");

        let mut authorization = with("auth", json!("api-key"));
        authorization["auth_header"] = json!("authorization");
        assert!(parse(&authorization).is_err(), "must refuse authorization");

        let mut stray = minimal();
        stray["auth_header"] = json!("api-key");
        assert!(
            parse(&stray).is_err(),
            "auth_header without auth: api-key is a mistake"
        );
    }

    #[test]
    fn unrepresentable_wire_fields_are_refused() {
        assert!(parse(&with("max_tokens_key", json!("max_new_tokens"))).is_err());
        assert!(parse(&with("passthrough_headers", json!(["cookie"]))).is_err());
        assert!(parse(&with(
            "passthrough_headers",
            json!(["openai-beta", "openai-beta"])
        ))
        .is_err());
        assert!(parse(&with("auth", json!("oauth2"))).is_err());
        assert!(parse(&with("default_dialect", json!("grpc"))).is_err());
    }

    #[test]
    fn credentials_cannot_hide_in_headers_or_query_parameters() {
        for (key, value) in [
            ("authorization", json!({"authorization": "Bearer leaked"})),
            ("api-key", json!({"api-key": "anything"})),
            ("cookie", json!({"cookie": "session=1"})),
            ("secret-value", json!({"x-note": "Bearer sk-live-123"})),
        ] {
            assert!(
                parse(&with("extra_headers", value)).is_err(),
                "extra_headers.{key} must be refused"
            );
        }
        for key in ["api-key", "token", "access_token", "signature"] {
            let value = json!({ key: "abc" });
            assert!(
                parse(&with("query", value)).is_err(),
                "query.{key} must be refused"
            );
        }
    }

    #[test]
    fn a_placeholder_host_may_require_an_override_and_a_real_one_may_not() {
        let mut placeholder = minimal();
        placeholder["base_url"] =
            json!("https://RESOURCE.openai.azure.com/openai/v1/chat/completions");
        placeholder["base_url_requires_override"] = json!(true);
        let manifest = parse(&placeholder).expect("a placeholder may require an override");
        assert!(manifest.base_url_requires_override);

        let plain = parse(&minimal()).expect("valid");
        assert!(
            !plain.base_url_requires_override,
            "the flag defaults to false"
        );

        // A real host that requires an override would force every operator to override a
        // working endpoint, which is a manifest bug rather than a deployment choice.
        let mut real = minimal();
        real["base_url_requires_override"] = json!(true);
        let err = parse(&real).expect_err("must refuse");
        assert!(err.contains("not a placeholder host"), "{err}");

        let mut not_bool = minimal();
        not_bool["base_url_requires_override"] = json!("yes");
        assert!(parse(&not_bool).is_err());
    }

    #[test]
    fn placeholder_hosts_are_recognised_and_real_ones_are_not() {
        for url in [
            "https://RESOURCE.openai.azure.com/openai/v1/chat/completions",
            "https://resource.example.test/v1",
            "https://your-resource.openai.azure.com/v1",
            "https://placeholder.test/v1",
        ] {
            assert!(is_placeholder_host(url), "{url}");
        }
        for url in [
            "https://api.openai.com/v1/chat/completions",
            "https://contoso.openai.azure.com/openai/v1/chat/completions",
            "https://my-resource-group.example.test/v1",
            "not-a-url",
        ] {
            assert!(!is_placeholder_host(url), "{url}");
        }
        assert_eq!(
            host_of("https://user:pw@contoso.openai.azure.com:443/openai/v1?x=1#y"),
            Some("contoso.openai.azure.com")
        );
        assert_eq!(host_of("nonsense"), None);
    }

    #[test]
    fn file_and_batch_endpoints_follow_the_same_rules() {
        for key in ["files_url", "batches_url"] {
            let plain = parse(&minimal()).expect("valid");
            assert_eq!(
                if key == "files_url" {
                    plain.files_url.clone()
                } else {
                    plain.batches_url.clone()
                },
                None
            );

            let mut body = minimal();
            body[key] = json!("https://api.openai.com/v1/files");
            let manifest = parse(&body).expect("valid");
            assert_eq!(
                if key == "files_url" {
                    manifest.files_url.as_deref()
                } else {
                    manifest.batches_url.as_deref()
                },
                Some("https://api.openai.com/v1/files")
            );

            for bad in [
                "http://api.openai.com/v1/files",
                "https://api.openai.com/v1/files/",
                "https://api.openai.com/v1/files?purpose=batch",
                "https://{host}/v1/files",
            ] {
                let mut body = minimal();
                body[key] = json!(bad);
                assert!(parse(&body).is_err(), "{key} must refuse {bad}");
            }
        }
    }

    #[test]
    fn a_rerank_endpoint_is_optional_and_must_be_exact() {
        let plain = parse(&minimal()).expect("valid");
        assert_eq!(plain.rerank_url, None);

        let mut with_rerank = minimal();
        with_rerank["rerank_url"] = json!("https://api.jina.ai/v1/rerank");
        assert_eq!(
            parse(&with_rerank).expect("valid").rerank_url.as_deref(),
            Some("https://api.jina.ai/v1/rerank")
        );

        for bad in [
            "http://api.jina.ai/v1/rerank",
            "https://api.jina.ai/v1/rerank/",
            "https://api.jina.ai/v1/rerank?api-version=1",
            "https://{host}/v1/rerank",
        ] {
            let mut body = minimal();
            body["rerank_url"] = json!(bad);
            assert!(parse(&body).is_err(), "must refuse {bad}");
        }
    }

    #[test]
    fn urls_must_be_literal_https() {
        let mut http = minimal();
        http["base_url"] = json!("http://api.acme.ai/v1/chat/completions");
        assert!(parse(&http).is_err());

        let mut template = minimal();
        template["base_url"] =
            json!("https://{resource}.openai.azure.com/openai/v1/chat/completions");
        assert!(parse(&template).is_err(), "templates are not reviewable");

        let mut slash = minimal();
        slash["base_url"] = json!("https://api.acme.ai/v1/chat/completions/");
        assert!(parse(&slash).is_err());

        let mut optional = minimal();
        optional["models_url"] = json!("http://api.acme.ai/v1/models");
        assert!(parse(&optional).is_err(), "optional URLs are https too");
    }
}
