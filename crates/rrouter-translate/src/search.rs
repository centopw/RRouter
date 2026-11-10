//! Web search: one canonical request and result for provider-native search tools.
//!
//! The providers routed here run search inside their own model endpoint rather than
//! exposing a separate search API: Anthropic's Messages API with the `web_search` server
//! tool, OpenAI's Responses API (also used by Codex and xAI) with the `web_search` tool,
//! and Gemini's `generateContent` with Google Search grounding. Each answers with a
//! model-written summary plus citations, so the canonical form is an answer with an
//! ordered, de-duplicated list of sources.
//!
//! Provider selection, credentials, retries and the HTTP call live in the gateway. This
//! module owns request validation, the provider-specific wire body and response parsing.

use std::collections::{HashMap, HashSet};

use rrouter_core::{Error, Result};
use serde_json::{json, Map, Value};

/// Citations returned when the client does not ask for a count.
pub const DEFAULT_MAX_RESULTS: u32 = 5;
/// Ceiling for one request. The model may have read more pages than this; the extra
/// citations are dropped instead of sending an unbounded list back to the client.
pub const MAX_MAX_RESULTS: u32 = 20;
/// A search query is a sentence, not a document. The limit keeps a client from using
/// the endpoint as an unbounded prompt relay.
pub const MAX_QUERY_BYTES: usize = 8 * 1024;
/// Domain filters above this count are almost always a misconfigured caller.
const MAX_DOMAINS: usize = 10;

/// Which provider-native search surface a request is sent to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Anthropic Messages API with the `web_search` server tool.
    Anthropic,
    /// OpenAI Responses API with the `web_search` tool (OpenAI, Codex, xAI).
    OpenAiResponses,
    /// Gemini `generateContent` with the `google_search` tool.
    Gemini,
}

/// The canonical `/v1/search` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    /// Provider id, `provider/model`, alias or combo. `provider` is accepted as an alias
    /// for this field so clients that only name a provider work unchanged.
    pub model: String,
    pub query: String,
    pub max_results: u32,
    /// Only the domains the provider should search. Empty means no restriction.
    pub domain_filter: Vec<String>,
}

/// One cited source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    /// ISO-8601 page age, when the provider reports one (Anthropic does).
    pub published_at: Option<String>,
    /// A longer excerpt, when the provider grounds spans of its answer to the source.
    pub content: Option<String>,
}

/// Token usage reported by the provider. Tool-call fees are not represented here; the
/// gateway records the tokens it can price and never invents a search fee.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
}

/// The canonical result: the model's answer plus ordered citations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResponse {
    pub answer: String,
    pub results: Vec<SearchResult>,
    pub usage: SearchUsage,
}

const REQUEST_KEYS: &[&str] = &[
    "model",
    "provider",
    "query",
    "max_results",
    "search_type",
    "domain_filter",
];

fn search_err(message: impl Into<String>) -> Error {
    Error::Translation(format!("search: {}", message.into()))
}

/// Parse and validate a `/v1/search` body.
///
/// The shape is closed: a field this contract does not model is refused rather than
/// silently ignored, because a dropped domain filter changes which sources the provider
/// may use.
pub fn parse_request(body: &Value) -> Result<SearchRequest> {
    let Some(object) = body.as_object() else {
        return Err(search_err("request body must be a JSON object"));
    };
    for key in object.keys() {
        if !REQUEST_KEYS.contains(&key.as_str()) {
            return Err(search_err(format!(
                "unknown request field `{key}`; this endpoint accepts model, provider, \
                 query, max_results, search_type, domain_filter"
            )));
        }
    }

    let model = model_of(object)?;
    let query = required_text(object, "query")?;
    if query.len() > MAX_QUERY_BYTES {
        return Err(search_err(format!(
            "query is {} bytes; the limit is {MAX_QUERY_BYTES}",
            query.len()
        )));
    }

    let max_results = match object.get("max_results") {
        None | Some(Value::Null) => DEFAULT_MAX_RESULTS,
        Some(value) => {
            let Some(n) = value.as_u64() else {
                return Err(search_err("max_results must be an integer"));
            };
            let n = u32::try_from(n).unwrap_or(u32::MAX);
            if n == 0 {
                return Err(search_err("max_results must be at least 1"));
            }
            if n > MAX_MAX_RESULTS {
                return Err(search_err(format!(
                    "max_results must be at most {MAX_MAX_RESULTS}"
                )));
            }
            n
        }
    };

    if let Some(kind) = object.get("search_type") {
        match kind {
            Value::Null => {}
            Value::String(kind) if kind == "web" => {}
            Value::String(kind) => {
                return Err(search_err(format!(
                    "search_type `{kind}` is not supported; provider-native search is web only"
                )))
            }
            _ => return Err(search_err("search_type must be a string")),
        }
    }

    let domain_filter = domain_filter(object)?;

    Ok(SearchRequest {
        model,
        query,
        max_results,
        domain_filter,
    })
}

fn model_of(object: &Map<String, Value>) -> Result<String> {
    let model = optional_text(object, "model")?;
    let provider = optional_text(object, "provider")?;
    match (model, provider) {
        (Some(model), None) | (None, Some(model)) => Ok(model),
        (Some(model), Some(provider)) if model == provider => Ok(model),
        (Some(_), Some(_)) => Err(search_err("model and provider disagree")),
        (None, None) => Err(search_err("model is required (or provider)")),
    }
}

fn optional_text(object: &Map<String, Value>, key: &str) -> Result<Option<String>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.trim().is_empty() => {
            Err(search_err(format!("{key} must not be empty")))
        }
        Some(Value::String(value)) => Ok(Some(value.trim().to_string())),
        Some(_) => Err(search_err(format!("{key} must be a string"))),
    }
}

fn required_text(object: &Map<String, Value>, key: &str) -> Result<String> {
    match object.get(key) {
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        Some(Value::String(_)) => Err(search_err(format!("{key} must not be empty"))),
        Some(_) => Err(search_err(format!("{key} must be a string"))),
        None => Err(search_err(format!("{key} is required"))),
    }
}

fn domain_filter(object: &Map<String, Value>) -> Result<Vec<String>> {
    let Some(value) = object.get("domain_filter") else {
        return Ok(Vec::new());
    };
    let Value::Array(items) = value else {
        return Err(search_err("domain_filter must be an array of domains"));
    };
    if items.len() > MAX_DOMAINS {
        return Err(search_err(format!(
            "domain_filter has {} entries; the limit is {MAX_DOMAINS}",
            items.len()
        )));
    }
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let Some(domain) = item.as_str() else {
            return Err(search_err(format!(
                "domain_filter[{index}] must be a string"
            )));
        };
        let domain = domain.trim().to_ascii_lowercase();
        if domain.is_empty() {
            return Err(search_err(format!(
                "domain_filter[{index}] must not be empty"
            )));
        }
        if domain
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '/' | '?' | '#'))
        {
            return Err(search_err(format!(
                "domain_filter[{index}] must be a domain, not a URL"
            )));
        }
        if !out.contains(&domain) {
            out.push(domain);
        }
    }
    Ok(out)
}

/// Serialize the canonical request into the body the selected provider accepts.
pub fn build_request(mode: SearchMode, req: &SearchRequest) -> Value {
    match mode {
        SearchMode::Anthropic => {
            let mut tool = json!({
                "type": "web_search_20250305",
                "name": "web_search",
            });
            if !req.domain_filter.is_empty() {
                tool["allowed_domains"] = json!(req.domain_filter);
            }
            json!({
                "model": req.model,
                "max_tokens": 4096,
                "messages": [{"role": "user", "content": req.query}],
                "tools": [tool],
            })
        }
        SearchMode::OpenAiResponses => {
            let mut tool = json!({"type": "web_search"});
            if !req.domain_filter.is_empty() {
                tool["filters"] = json!({"allowed_domains": req.domain_filter});
            }
            json!({
                "model": req.model,
                "input": req.query,
                "tools": [tool],
                // RRouter never asks the provider to store the response.
                "store": false,
            })
        }
        SearchMode::Gemini => {
            // Gemini's Google Search tool has no domain filter field. State the
            // restriction in the prompt so the client's intent is not silently dropped.
            let text = if req.domain_filter.is_empty() {
                req.query.clone()
            } else {
                format!(
                    "{}\n\nOnly use sources from these domains: {}",
                    req.query,
                    req.domain_filter.join(", ")
                )
            };
            json!({
                "contents": [{"role": "user", "parts": [{"text": text}]}],
                "tools": [{"google_search": {}}],
            })
        }
    }
}

/// Parse a successful provider response into the canonical result.
///
/// A body that does not carry the selected provider's response anchor is a translation
/// error, which the gateway turns into a 502 so the dispatcher can try another account.
pub fn parse_response(mode: SearchMode, body: &Value) -> Result<SearchResponse> {
    let object = body
        .as_object()
        .ok_or_else(|| search_err("response body must be a JSON object"))?;
    match mode {
        SearchMode::Anthropic => parse_anthropic(object),
        SearchMode::OpenAiResponses => parse_openai(object),
        SearchMode::Gemini => parse_gemini(object),
    }
}

fn parse_anthropic(object: &Map<String, Value>) -> Result<SearchResponse> {
    let content = object
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| search_err("Anthropic response has no `content` array"))?;

    let mut answer = String::new();
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    append_text(&mut answer, text);
                }
            }
            Some("web_search_tool_result") => {
                let Some(items) = block.get("content").and_then(Value::as_array) else {
                    // `content` is an error object when the tool failed; the answer text
                    // that follows is still useful, and the failure is not a gateway bug.
                    continue;
                };
                for item in items {
                    if item.get("type").and_then(Value::as_str) != Some("web_search_result") {
                        continue;
                    }
                    let Some(url) = item.get("url").and_then(Value::as_str) else {
                        continue;
                    };
                    add_result(
                        &mut results,
                        &mut seen,
                        url,
                        item.get("title").and_then(Value::as_str).unwrap_or(""),
                        "",
                        item.get("page_age").and_then(Value::as_str),
                        None,
                    );
                }
            }
            _ => {}
        }
    }

    Ok(SearchResponse {
        answer,
        results,
        usage: SearchUsage {
            input_tokens: pointer_count(object, "/usage/input_tokens").unwrap_or(0),
            output_tokens: pointer_count(object, "/usage/output_tokens").unwrap_or(0),
            cache_read_tokens: pointer_count(object, "/usage/cache_read_input_tokens").unwrap_or(0),
        },
    })
}

fn parse_openai(object: &Map<String, Value>) -> Result<SearchResponse> {
    let output = object.get("output").and_then(Value::as_array);
    if output.is_none()
        && object.get("output_text").and_then(Value::as_str).is_none()
        && object.get("citations").and_then(Value::as_array).is_none()
    {
        return Err(search_err(
            "Responses API body has no `output`, `output_text` or `citations`",
        ));
    }

    let mut answer = String::new();
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    for item in output.into_iter().flatten() {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        append_text(&mut answer, text);
                    }
                    for annotation in part
                        .get("annotations")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if annotation.get("type").and_then(Value::as_str) != Some("url_citation")
                            && annotation.get("url").is_none()
                        {
                            continue;
                        }
                        let Some(url) = annotation.get("url").and_then(Value::as_str) else {
                            continue;
                        };
                        add_result(
                            &mut results,
                            &mut seen,
                            url,
                            annotation
                                .get("title")
                                .and_then(Value::as_str)
                                .unwrap_or(""),
                            "",
                            None,
                            None,
                        );
                    }
                }
            }
            Some("web_search_call") => {
                for source in item
                    .pointer("/action/sources")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let Some(url) = source
                        .get("url")
                        .or_else(|| source.get("uri"))
                        .and_then(Value::as_str)
                    else {
                        continue;
                    };
                    add_result(
                        &mut results,
                        &mut seen,
                        url,
                        source.get("title").and_then(Value::as_str).unwrap_or(""),
                        "",
                        None,
                        None,
                    );
                }
            }
            _ => {}
        }
    }

    if answer.is_empty() {
        if let Some(text) = object.get("output_text").and_then(Value::as_str) {
            append_text(&mut answer, text);
        }
    }
    if results.is_empty() {
        for citation in object
            .get("citations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match citation {
                Value::String(url) => add_result(&mut results, &mut seen, url, "", "", None, None),
                Value::Object(map) => {
                    let Some(url) = map
                        .get("url")
                        .or_else(|| map.get("uri"))
                        .and_then(Value::as_str)
                    else {
                        continue;
                    };
                    add_result(
                        &mut results,
                        &mut seen,
                        url,
                        map.get("title").and_then(Value::as_str).unwrap_or(""),
                        "",
                        None,
                        None,
                    );
                }
                _ => {}
            }
        }
    }

    Ok(SearchResponse {
        answer,
        results,
        usage: SearchUsage {
            input_tokens: pointer_count(object, "/usage/input_tokens")
                .or_else(|| pointer_count(object, "/usage/prompt_tokens"))
                .unwrap_or(0),
            output_tokens: pointer_count(object, "/usage/output_tokens")
                .or_else(|| pointer_count(object, "/usage/completion_tokens"))
                .unwrap_or(0),
            cache_read_tokens: pointer_count(object, "/usage/input_tokens_details/cached_tokens")
                .unwrap_or(0),
        },
    })
}

fn parse_gemini(object: &Map<String, Value>) -> Result<SearchResponse> {
    let candidates = object
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or_else(|| search_err("Gemini response has no `candidates` array"))?;
    let Some(candidate) = candidates.first() else {
        let reason = object
            .get("promptFeedback")
            .and_then(|feedback| feedback.get("blockReason"))
            .and_then(Value::as_str)
            .unwrap_or("no candidates");
        return Err(search_err(format!(
            "Gemini returned no candidates: {reason}"
        )));
    };

    let mut answer = String::new();
    for part in candidate
        .pointer("/content/parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            append_text(&mut answer, text);
        }
    }

    // A support ties a span of the answer to one or more grounding chunks. Use the
    // supported sentence as the result snippet, one per chunk.
    let mut snippets: HashMap<usize, String> = HashMap::new();
    for support in candidate
        .pointer("/groundingMetadata/groundingSupports")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(segment) = support
            .pointer("/segment/text")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
        else {
            continue;
        };
        for index in support
            .get("groundingChunkIndices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(index) = index.as_u64().and_then(|n| usize::try_from(n).ok()) else {
                continue;
            };
            snippets.entry(index).or_insert_with(|| segment.to_string());
        }
    }

    let mut results = Vec::new();
    let mut seen = HashSet::new();
    for (index, chunk) in candidate
        .pointer("/groundingMetadata/groundingChunks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let Some(web) = chunk.get("web") else {
            continue;
        };
        let Some(url) = web
            .get("uri")
            .or_else(|| web.get("url"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let snippet = snippets.get(&index).map(String::as_str).unwrap_or_default();
        add_result(
            &mut results,
            &mut seen,
            url,
            web.get("title").and_then(Value::as_str).unwrap_or(""),
            snippet,
            None,
            None,
        );
    }

    Ok(SearchResponse {
        answer,
        results,
        usage: SearchUsage {
            input_tokens: pointer_count(object, "/usageMetadata/promptTokenCount").unwrap_or(0),
            output_tokens: pointer_count(object, "/usageMetadata/candidatesTokenCount")
                .unwrap_or(0),
            cache_read_tokens: pointer_count(object, "/usageMetadata/cachedContentTokenCount")
                .unwrap_or(0),
        },
    })
}

fn append_text(answer: &mut String, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if !answer.is_empty() {
        answer.push('\n');
    }
    answer.push_str(text);
}

#[allow(clippy::too_many_arguments)]
fn add_result(
    results: &mut Vec<SearchResult>,
    seen: &mut HashSet<String>,
    url: &str,
    title: &str,
    snippet: &str,
    published_at: Option<&str>,
    content: Option<&str>,
) {
    let url = url.trim();
    // Only web URLs. A provider that returns a `data:` or `javascript:` citation is not
    // handing the client a source, and the gateway will not relay one.
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return;
    }
    if !seen.insert(url.to_string()) {
        return;
    }
    let clean = |value: &str| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    };
    results.push(SearchResult {
        title: title.trim().to_string(),
        url: url.to_string(),
        snippet: snippet.trim().to_string(),
        published_at: published_at.and_then(clean),
        content: content.and_then(clean),
    });
}

fn pointer_count(object: &Map<String, Value>, path: &str) -> Option<u32> {
    let mut parts = path.trim_start_matches('/').split('/');
    let mut value = object.get(parts.next()?)?;
    for part in parts {
        value = value.get(part)?;
    }
    count(value)
}

fn count(value: &Value) -> Option<u32> {
    if let Some(number) = value.as_u64() {
        return u32::try_from(number).ok();
    }
    let number = value.as_f64()?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    Some(number.round().min(f64::from(u32::MAX)) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request() -> SearchRequest {
        SearchRequest {
            model: "claude/claude-sonnet-4-5".into(),
            query: "What changed in the latest Rust release?".into(),
            max_results: 2,
            domain_filter: vec!["blog.rust-lang.org".into()],
        }
    }

    #[test]
    fn a_request_accepts_provider_as_a_model_alias() {
        let parsed = parse_request(&json!({
            "provider": "claude",
            "query": "rust release",
        }))
        .expect("valid");
        assert_eq!(parsed.model, "claude");
        assert_eq!(parsed.max_results, DEFAULT_MAX_RESULTS);
        assert!(parsed.domain_filter.is_empty());
    }

    #[test]
    fn request_validation_names_the_offending_field() {
        let cases: Vec<(Value, &str)> = vec![
            (json!({"query": "q"}), "model is required"),
            (
                json!({"model": "", "query": "q"}),
                "model must not be empty",
            ),
            (json!({"model": "claude"}), "query is required"),
            (
                json!({"model": "claude", "query": ""}),
                "query must not be empty",
            ),
            (
                json!({"model": "claude", "query": "q", "max_results": 0}),
                "max_results must be at least 1",
            ),
            (
                json!({"model": "claude", "query": "q", "max_results": 21}),
                "max_results must be at most 20",
            ),
            (
                json!({"model": "claude", "query": "q", "max_results": "5"}),
                "max_results must be an integer",
            ),
            (
                json!({"model": "claude", "query": "q", "search_type": "news"}),
                "provider-native search is web only",
            ),
            (
                json!({"model": "claude", "query": "q", "domain_filter": ["https://example.com"]}),
                "must be a domain, not a URL",
            ),
            (
                json!({"model": "claude", "query": "q", "unknown": true}),
                "unknown request field `unknown`",
            ),
        ];
        for (body, expected) in cases {
            let error = parse_request(&body).expect_err("invalid");
            assert!(
                error.to_string().contains(expected),
                "{body}: {error} did not contain {expected}"
            );
        }
    }

    #[test]
    fn domain_filters_are_normalized_and_deduplicated() {
        let parsed = parse_request(&json!({
            "model": "claude",
            "query": "q",
            "domain_filter": [" Example.COM ", "example.com", "*.rust-lang.org"],
        }))
        .expect("valid");
        assert_eq!(
            parsed.domain_filter,
            vec!["example.com".to_string(), "*.rust-lang.org".to_string()]
        );
    }

    #[test]
    fn anthropic_gets_the_server_tool_and_the_domain_filter() {
        let body = build_request(SearchMode::Anthropic, &request());
        assert_eq!(body["model"], json!("claude/claude-sonnet-4-5"));
        assert_eq!(body["messages"][0]["content"], json!(request().query));
        assert_eq!(body["tools"][0]["type"], json!("web_search_20250305"));
        assert_eq!(body["tools"][0]["name"], json!("web_search"));
        assert_eq!(
            body["tools"][0]["allowed_domains"],
            json!(["blog.rust-lang.org"])
        );
        assert!(
            body["tools"][0].get("max_uses").is_none(),
            "max_results is a citation cap, not the number of searches"
        );
    }

    #[test]
    fn openai_and_xai_get_the_responses_web_search_tool() {
        let body = build_request(SearchMode::OpenAiResponses, &request());
        assert_eq!(body["model"], json!("claude/claude-sonnet-4-5"));
        assert_eq!(body["input"], json!(request().query));
        assert_eq!(body["tools"][0]["type"], json!("web_search"));
        assert_eq!(
            body["tools"][0]["filters"]["allowed_domains"],
            json!(["blog.rust-lang.org"])
        );
    }

    #[test]
    fn gemini_gets_google_search_and_a_prompt_level_domain_hint() {
        let body = build_request(SearchMode::Gemini, &request());
        assert_eq!(body["tools"][0]["google_search"], json!({}));
        let text = body["contents"][0]["parts"][0]["text"]
            .as_str()
            .expect("text");
        assert!(text.contains("What changed"));
        assert!(text.contains("Only use sources from these domains"));
        assert!(text.contains("blog.rust-lang.org"));
    }

    #[test]
    fn anthropic_citations_and_usage_are_parsed() {
        let body = json!({
            "id": "msg_01",
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "server_tool_use",
                    "id": "srvtoolu_01",
                    "name": "web_search",
                    "input": {"query": "latest rust release"}
                },
                {
                    "type": "web_search_tool_result",
                    "tool_use_id": "srvtoolu_01",
                    "content": [
                        {
                            "type": "web_search_result",
                            "url": "https://blog.rust-lang.org/2026/01/01/Rust-1.90.0.html",
                            "title": "Announcing Rust 1.90.0",
                            "page_age": "2026-01-01T00:00:00Z"
                        },
                        {
                            "type": "web_search_result",
                            "url": "https://docs.rs/std",
                            "title": "std docs"
                        }
                    ]
                },
                {"type": "text", "text": "Rust 1.90 was released."}
            ],
            "usage": {"input_tokens": 120, "output_tokens": 40, "cache_read_input_tokens": 12}
        });
        let parsed = parse_response(SearchMode::Anthropic, &body).expect("anthropic");
        assert_eq!(parsed.answer, "Rust 1.90 was released.");
        assert_eq!(parsed.results.len(), 2);
        assert_eq!(parsed.results[0].title, "Announcing Rust 1.90.0");
        assert_eq!(
            parsed.results[0].published_at.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        assert_eq!(
            parsed.usage,
            SearchUsage {
                input_tokens: 120,
                output_tokens: 40,
                cache_read_tokens: 12
            }
        );
    }

    #[test]
    fn openai_annotations_and_sources_are_parsed_without_duplicates() {
        let body = json!({
            "id": "resp_01",
            "object": "response",
            "output": [
                {
                    "type": "web_search_call",
                    "id": "ws_01",
                    "status": "completed",
                    "action": {
                        "type": "search",
                        "query": "rust release",
                        "sources": [
                            {"url": "https://blog.rust-lang.org/release", "title": "Rust release"}
                        ]
                    }
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "The release is out.",
                            "annotations": [
                                {
                                    "type": "url_citation",
                                    "url": "https://blog.rust-lang.org/release",
                                    "title": "Rust release"
                                },
                                {
                                    "type": "url_citation",
                                    "url": "https://docs.rs/std",
                                    "title": "std docs"
                                }
                            ]
                        }
                    ]
                }
            ],
            "usage": {
                "input_tokens": 80,
                "output_tokens": 20,
                "input_tokens_details": {"cached_tokens": 5}
            }
        });
        let parsed = parse_response(SearchMode::OpenAiResponses, &body).expect("openai");
        assert_eq!(parsed.answer, "The release is out.");
        assert_eq!(parsed.results.len(), 2, "the source and annotation dedupe");
        assert_eq!(parsed.results[0].url, "https://blog.rust-lang.org/release");
        assert_eq!(parsed.results[1].url, "https://docs.rs/std");
        assert_eq!(
            parsed.usage,
            SearchUsage {
                input_tokens: 80,
                output_tokens: 20,
                cache_read_tokens: 5
            }
        );
    }

    #[test]
    fn gemini_grounding_chunks_and_support_snippets_are_parsed() {
        let body = json!({
            "candidates": [{
                "content": {"parts": [{"text": "Rust 1.90 is out."}], "role": "model"},
                "groundingMetadata": {
                    "groundingChunks": [
                        {"web": {"uri": "https://blog.rust-lang.org/release", "title": "Rust release"}},
                        {"web": {"uri": "https://docs.rs/std", "title": "std docs"}}
                    ],
                    "groundingSupports": [{
                        "segment": {"text": "Rust 1.90 is out."},
                        "groundingChunkIndices": [0, 1]
                    }]
                }
            }],
            "usageMetadata": {
                "promptTokenCount": 55,
                "candidatesTokenCount": 11,
                "cachedContentTokenCount": 4
            }
        });
        let parsed = parse_response(SearchMode::Gemini, &body).expect("gemini");
        assert_eq!(parsed.answer, "Rust 1.90 is out.");
        assert_eq!(parsed.results.len(), 2);
        assert_eq!(parsed.results[0].snippet, "Rust 1.90 is out.");
        assert_eq!(
            parsed.usage,
            SearchUsage {
                input_tokens: 55,
                output_tokens: 11,
                cache_read_tokens: 4
            }
        );
    }

    #[test]
    fn a_body_without_the_provider_anchor_is_a_translation_error() {
        for mode in [
            SearchMode::Anthropic,
            SearchMode::OpenAiResponses,
            SearchMode::Gemini,
        ] {
            let error = parse_response(mode, &json!({"ok": true})).expect_err("not a search body");
            assert!(error.to_string().contains("search:"), "{mode:?}: {error}");
        }
    }

    #[test]
    fn non_http_citations_are_dropped() {
        let body = json!({
            "content": [{
                "type": "web_search_tool_result",
                "content": [
                    {"type": "web_search_result", "url": "javascript:alert(1)"},
                    {"type": "web_search_result", "url": "https://example.com", "title": "ok"}
                ]
            }],
            "usage": {}
        });
        let parsed = parse_response(SearchMode::Anthropic, &body).expect("anthropic");
        assert_eq!(parsed.results.len(), 1);
        assert_eq!(parsed.results[0].url, "https://example.com");
    }
}
