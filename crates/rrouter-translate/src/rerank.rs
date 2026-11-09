//! Rerank: one canonical request and one canonical result for the hosted APIs that share
//! a shape.
//!
//! Cohere (`POST /v1/rerank`) and Jina (`POST /v1/rerank`) take the same body — `model`,
//! `query`, `documents`, `top_n`, `return_documents` — and answer with an ordered list of
//! `{index, relevance_score, document?}`. One canonical form therefore covers both; the
//! caller owns provider selection and the HTTP call, this module owns the body and the
//! result.
//!
//! Upstream *failures* never reach these functions: they stay `rrouter_core::Error::Upstream`
//! and are rendered by [`crate::error::upstream_error_body`], which keeps the provider's own
//! message, code and status instead of flattening them into a generic sentence.

use rrouter_core::{Error, Result};
use serde_json::{Map, Value};

/// Documents one request may carry. Cohere recommends at most 1,000 in a single request;
/// the ceiling is enforced here so a client cannot hand a provider an unbounded body.
pub const MAX_DOCUMENTS: usize = 1000;

/// Highest accepted `top_n`. Asking for more results than a provider could ever rank is a
/// client bug; a value above the document count is *not* rejected, because both providers
/// treat it as "return everything" and clients commonly send a fixed `top_n`.
pub const MAX_TOP_N: u32 = 1000;

/// The canonical rerank request.
#[derive(Debug, Clone, PartialEq)]
pub struct RerankRequest {
    /// Model id the client asked for; the caller has already resolved it to the provider's
    /// own id, exactly as it does for chat.
    pub model: String,
    pub query: String,
    pub documents: Vec<String>,
    /// `None` returns every result the provider ranked.
    pub top_n: Option<u32>,
    /// Ask for the document text in the response. Always sent, because Cohere defaults to
    /// `false` and Jina to `true`: omitting it would make the answer provider-specific.
    pub return_documents: bool,
}

/// One ranked document.
#[derive(Debug, Clone, PartialEq)]
pub struct RerankResult {
    /// Position of the document in the *request's* list.
    pub index: u32,
    pub relevance_score: f64,
    /// Document text, when the provider echoed it back (`return_documents`).
    pub document: Option<String>,
}

/// What the provider billed, in whichever units it reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RerankUsage {
    /// Input tokens: Jina's `usage.total_tokens` is its only token figure, Cohere reports
    /// `usage.billed_units.input_tokens` or `meta.tokens.input_tokens`.
    pub input_tokens: u32,
    /// Jina's `usage.total_tokens`; zero when the provider does not report a total.
    pub total_tokens: u32,
    /// Cohere's billing unit; zero for a token-billed provider.
    pub search_units: u32,
}

/// The canonical rerank response.
#[derive(Debug, Clone, PartialEq)]
pub struct RerankResponse {
    pub model: String,
    pub results: Vec<RerankResult>,
    pub usage: RerankUsage,
}

const REQUEST_KEYS: &[&str] = &["model", "query", "documents", "top_n", "return_documents"];

fn rerank_err(message: impl Into<String>) -> Error {
    Error::Translation(format!("rerank: {}", message.into()))
}

/// Parse and validate a client's `/v1/rerank` body into the canonical request.
///
/// The shape is closed: a field this contract does not model is refused rather than
/// silently dropped, because a dropped rerank option changes the answer the client gets.
pub fn parse_request(body: &Value) -> Result<RerankRequest> {
    let Some(object) = body.as_object() else {
        return Err(rerank_err("request body must be a JSON object"));
    };
    for key in object.keys() {
        if !REQUEST_KEYS.contains(&key.as_str()) {
            return Err(rerank_err(format!(
                "unknown request field `{key}`; this endpoint accepts model, query, documents, top_n, return_documents"
            )));
        }
    }

    let model = required_text(object, "model")?;
    let query = required_text(object, "query")?;

    let documents = match object.get("documents") {
        Some(Value::Array(items)) => {
            if items.is_empty() {
                return Err(rerank_err("documents must not be empty"));
            }
            if items.len() > MAX_DOCUMENTS {
                return Err(rerank_err(format!(
                    "documents carries {} entries; the limit is {MAX_DOCUMENTS}",
                    items.len()
                )));
            }
            let mut out = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                match item.as_str() {
                    Some(text) if !text.is_empty() => out.push(text.to_string()),
                    Some(_) => {
                        return Err(rerank_err(format!("documents[{index}] must not be empty")))
                    }
                    // Document objects belong to each provider's own API; the canonical form
                    // is text, so an object is a contract error rather than something to guess.
                    None => {
                        return Err(rerank_err(format!(
                            "documents[{index}] must be a string, got {}",
                            json_kind(item)
                        )))
                    }
                }
            }
            out
        }
        Some(_) => return Err(rerank_err("documents must be an array of strings")),
        None => return Err(rerank_err("documents is required")),
    };

    let top_n = match object.get("top_n") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let Some(n) = value.as_u64() else {
                return Err(rerank_err("top_n must be an integer"));
            };
            let n = u32::try_from(n).unwrap_or(u32::MAX);
            if n == 0 {
                return Err(rerank_err("top_n must be at least 1"));
            }
            if n > MAX_TOP_N {
                return Err(rerank_err(format!("top_n must be at most {MAX_TOP_N}")));
            }
            Some(n)
        }
    };

    let return_documents = match object.get("return_documents") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err(rerank_err("return_documents must be a boolean")),
    };

    Ok(RerankRequest {
        model,
        query,
        documents,
        top_n,
        return_documents,
    })
}

/// Serialize the canonical request into the body both hosts accept.
///
/// Cohere v1 and Jina both document `documents` as strings (objects are optional extensions),
/// so this body is valid at either endpoint without a per-provider branch.
pub fn build_request(req: &RerankRequest) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), Value::from(req.model.clone()));
    body.insert("query".into(), Value::from(req.query.clone()));
    body.insert(
        "documents".into(),
        Value::Array(req.documents.iter().cloned().map(Value::from).collect()),
    );
    if let Some(top_n) = req.top_n {
        body.insert("top_n".into(), Value::from(top_n));
    }
    body.insert("return_documents".into(), Value::Bool(req.return_documents));
    Value::Object(body)
}

/// Parse a successful rerank body into the canonical response.
///
/// `model` is the id that was requested: Cohere v1 does not echo a model, so the caller's
/// value is the only truthful answer there. Anything without a `results` array — an OpenAI
/// chat completion, an HTML page, an empty body — is a typed translation error rather than
/// an empty result.
pub fn parse_response(body: &Value, model: &str) -> Result<RerankResponse> {
    let Some(object) = body.as_object() else {
        return Err(rerank_err("response body must be a JSON object"));
    };
    let Some(results) = object.get("results").and_then(Value::as_array) else {
        return Err(rerank_err(
            "response has no `results` array; this is not a rerank response",
        ));
    };

    let mut out = Vec::with_capacity(results.len());
    for (position, item) in results.iter().enumerate() {
        let index = item
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| {
                rerank_err(format!("results[{position}].index is missing or invalid"))
            })?;
        let relevance_score = item
            .get("relevance_score")
            .and_then(Value::as_f64)
            .ok_or_else(|| {
                rerank_err(format!(
                    "results[{position}].relevance_score is missing or not a number"
                ))
            })?;
        out.push(RerankResult {
            index,
            relevance_score,
            document: document_text(item.get("document")),
        });
    }

    let echoed = object
        .get("model")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(model);

    Ok(RerankResponse {
        model: echoed.to_string(),
        results: out,
        usage: usage_of(object),
    })
}

/// Document text from either family: a bare string (Jina may return one), a `{text}` object
/// (both families), or nothing when `return_documents` was off.
fn document_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Object(map) => map
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned),
        _ => None,
    }
}

/// Usage from either family: Cohere puts it under `usage` (v2) or `meta` (v1), Jina under
/// `usage`. Missing numbers are zero, never a guess.
fn usage_of(object: &Map<String, Value>) -> RerankUsage {
    let usage = object.get("usage").unwrap_or(&Value::Null);
    let meta = object.get("meta").unwrap_or(&Value::Null);
    let pick = |from: &Value, path: &str| from.pointer(path).and_then(count);
    RerankUsage {
        input_tokens: pick(usage, "/billed_units/input_tokens")
            .or_else(|| pick(meta, "/tokens/input_tokens"))
            .unwrap_or(0),
        total_tokens: pick(usage, "/total_tokens").unwrap_or(0),
        search_units: pick(usage, "/billed_units/search_units")
            .or_else(|| pick(meta, "/billed_units/search_units"))
            .unwrap_or(0),
    }
}

/// A token count: providers send integers, Cohere sometimes sends `192.0`.
fn count(value: &Value) -> Option<u32> {
    if let Some(n) = value.as_u64() {
        return u32::try_from(n).ok();
    }
    let n = value.as_f64()?;
    if !n.is_finite() || n < 0.0 {
        return None;
    }
    Some(n.round().min(f64::from(u32::MAX)) as u32)
}

fn required_text(object: &Map<String, Value>, key: &str) -> Result<String> {
    match object.get(key) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
        Some(Value::String(_)) => Err(rerank_err(format!("{key} must not be empty"))),
        Some(_) => Err(rerank_err(format!("{key} must be a string"))),
        None => Err(rerank_err(format!("{key} is required"))),
    }
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request() -> Value {
        json!({
            "model": "rerank-v3.5",
            "query": "What is the capital of the United States?",
            "documents": [
                "Carson City is the capital of Nevada.",
                "Washington, D.C. is the capital of the United States.",
                "Capitalization is a grammar term."
            ],
            "top_n": 2,
            "return_documents": true
        })
    }

    /// Cohere v1: no `model` in the body, billing under `meta`.
    fn cohere_response() -> Value {
        json!({
            "results": [
                {"index": 1, "relevance_score": 0.999071, "document": {"text": "Washington, D.C. is the capital of the United States."}},
                {"index": 0, "relevance_score": 0.32713068}
            ],
            "id": "8bc745a3-7871-4597-822e-18c95d5df48c",
            "meta": {
                "api_version": {"version": "1"},
                "billed_units": {"search_units": 1}
            }
        })
    }

    /// Jina: echoes `model`, billing under `usage.total_tokens`.
    fn jina_response() -> Value {
        json!({
            "model": "jina-reranker-v2-base-multilingual",
            "object": "list",
            "usage": {"total_tokens": 78},
            "results": [
                {"index": 1, "relevance_score": 0.91, "document": "Washington, D.C. is the capital of the United States."},
                {"index": 0, "relevance_score": 0.12, "document": null}
            ]
        })
    }

    #[test]
    fn a_cohere_v1_response_becomes_the_canonical_result() {
        let parsed = parse_response(&cohere_response(), "rerank-v3.5").expect("cohere");
        assert_eq!(
            parsed.model, "rerank-v3.5",
            "Cohere v1 does not echo a model; the requested id is the truthful answer"
        );
        assert_eq!(parsed.results.len(), 2);
        assert_eq!(parsed.results[0].index, 1);
        assert!((parsed.results[0].relevance_score - 0.999071).abs() < 1e-9);
        assert_eq!(
            parsed.results[0].document.as_deref(),
            Some("Washington, D.C. is the capital of the United States.")
        );
        assert_eq!(parsed.results[1].document, None);
        assert_eq!(
            parsed.usage,
            RerankUsage {
                input_tokens: 0,
                total_tokens: 0,
                search_units: 1
            }
        );
    }

    #[test]
    fn a_jina_response_becomes_the_canonical_result() {
        let parsed =
            parse_response(&jina_response(), "jina-reranker-v2-base-multilingual").expect("jina");
        assert_eq!(parsed.model, "jina-reranker-v2-base-multilingual");
        assert_eq!(parsed.results[0].index, 1);
        assert_eq!(
            parsed.results[0].document.as_deref(),
            Some("Washington, D.C. is the capital of the United States.")
        );
        assert_eq!(
            parsed.results[1].document, None,
            "a null document stays absent"
        );
        assert_eq!(
            parsed.usage,
            RerankUsage {
                input_tokens: 0,
                total_tokens: 78,
                search_units: 0
            }
        );
    }

    #[test]
    fn a_cohere_v2_usage_block_is_read_too() {
        let body = json!({
            "results": [{"index": 0, "relevance_score": 0.5}],
            "usage": {"billed_units": {"search_units": 2, "input_tokens": 192.0}},
        });
        let parsed = parse_response(&body, "rerank-v5.0-pro").expect("cohere v2");
        assert_eq!(
            parsed.usage,
            RerankUsage {
                input_tokens: 192,
                total_tokens: 0,
                search_units: 2
            },
            "Cohere sends 192.0; the count is an integer"
        );
    }

    #[test]
    fn one_body_is_valid_at_either_host() {
        let canonical = parse_request(&request()).expect("valid");
        let body = build_request(&canonical);
        assert_eq!(
            body,
            json!({
                "model": "rerank-v3.5",
                "query": "What is the capital of the United States?",
                "documents": [
                    "Carson City is the capital of Nevada.",
                    "Washington, D.C. is the capital of the United States.",
                    "Capitalization is a grammar term."
                ],
                "top_n": 2,
                "return_documents": true
            })
        );
        // Exactly the documented fields: no credential, no dialect passthrough.
        let keys: Vec<&str> = body
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            vec!["documents", "model", "query", "return_documents", "top_n"]
        );
        let text = body.to_string().to_ascii_lowercase();
        for secret in ["authorization", "api_key", "api-key", "bearer"] {
            assert!(
                !text.contains(secret),
                "translated body leaked {secret}: {text}"
            );
        }
    }

    #[test]
    fn return_documents_is_always_explicit_because_the_defaults_differ() {
        let mut canonical = parse_request(&request()).expect("valid");
        canonical.return_documents = false;
        assert_eq!(build_request(&canonical)["return_documents"], json!(false));
        // ... and a client that omits it gets the same deterministic value.
        let mut body = request();
        body.as_object_mut()
            .expect("object")
            .remove("return_documents");
        let parsed = parse_request(&body).expect("valid");
        assert!(!parsed.return_documents);
        assert_eq!(build_request(&parsed)["return_documents"], json!(false));
    }

    #[test]
    fn a_top_n_larger_than_the_document_count_is_forwarded() {
        // Both providers treat it as "return everything", and clients send fixed values.
        let mut body = request();
        body["top_n"] = json!(10);
        let parsed = parse_request(&body).expect("accepted");
        assert_eq!(parsed.top_n, Some(10));
        assert_eq!(build_request(&parsed)["top_n"], json!(10));
    }

    #[test]
    fn a_missing_top_n_returns_every_ranked_document() {
        let mut body = request();
        body.as_object_mut().expect("object").remove("top_n");
        let parsed = parse_request(&body).expect("valid");
        assert_eq!(parsed.top_n, None);
        assert!(build_request(&parsed).get("top_n").is_none());
        // `null` is how several SDKs serialize an absent optional; it means the same.
        body["top_n"] = Value::Null;
        assert_eq!(parse_request(&body).expect("valid").top_n, None);
    }

    #[test]
    fn request_validation_names_the_offending_field() {
        let cases: Vec<(Value, &str)> = vec![
            (
                json!({"query": "q", "documents": ["d"]}),
                "model is required",
            ),
            (
                json!({"model": "", "query": "q", "documents": ["d"]}),
                "model must not be empty",
            ),
            (
                json!({"model": "m", "documents": ["d"]}),
                "query is required",
            ),
            (
                json!({"model": "m", "query": "", "documents": ["d"]}),
                "query must not be empty",
            ),
            (json!({"model": "m", "query": "q"}), "documents is required"),
            (
                json!({"model": "m", "query": "q", "documents": []}),
                "documents must not be empty",
            ),
            (
                json!({"model": "m", "query": "q", "documents": "d"}),
                "documents must be an array",
            ),
            (
                json!({"model": "m", "query": "q", "documents": ["d", {"text": "d2"}]}),
                "documents[1] must be a string, got an object",
            ),
            (
                json!({"model": "m", "query": "q", "documents": ["d", ""]}),
                "documents[1] must not be empty",
            ),
            (
                json!({"model": "m", "query": "q", "documents": ["d"], "top_n": 0}),
                "top_n must be at least 1",
            ),
            (
                json!({"model": "m", "query": "q", "documents": ["d"], "top_n": 1001}),
                "top_n must be at most 1000",
            ),
            (
                json!({"model": "m", "query": "q", "documents": ["d"], "top_n": "2"}),
                "top_n must be an integer",
            ),
            (
                json!({"model": "m", "query": "q", "documents": ["d"], "return_documents": "yes"}),
                "return_documents must be a boolean",
            ),
            (
                json!({"model": "m", "query": "q", "documents": ["d"], "max_tokens_per_doc": 512}),
                "unknown request field `max_tokens_per_doc`",
            ),
        ];
        for (body, expected) in cases {
            let error = parse_request(&body).expect_err(&body.to_string());
            assert!(
                error.to_string().contains(expected),
                "{} did not mention {expected}",
                error
            );
            assert!(
                matches!(error, Error::Translation(_)),
                "a bad rerank body is a translation error"
            );
        }
    }

    #[test]
    fn the_document_limit_is_enforced() {
        let documents: Vec<String> = (0..=MAX_DOCUMENTS).map(|i| format!("doc {i}")).collect();
        let body = json!({"model": "m", "query": "q", "documents": documents});
        let error = parse_request(&body).expect_err("over the limit");
        assert!(error.to_string().contains("the limit is 1000"), "{error}");

        let documents: Vec<String> = (0..MAX_DOCUMENTS).map(|i| format!("doc {i}")).collect();
        assert!(parse_request(&json!({
            "model": "m", "query": "q", "documents": documents
        }))
        .is_ok());
    }

    #[test]
    fn an_unknown_response_shape_is_a_typed_error() {
        for body in [
            json!({"choices": [{"message": {"content": "hi"}}]}),
            json!({}),
            json!({"results": "nope"}),
            json!({"results": [{"relevance_score": 0.5}]}),
            json!({"results": [{"index": 0}]}),
        ] {
            let error = parse_response(&body, "m").expect_err(&body.to_string());
            assert!(
                matches!(error, Error::Translation(_)),
                "{body} must fail as a translation error"
            );
        }
        assert!(parse_response(&json!({"results": []}), "m")
            .expect("an empty ranking is a valid answer")
            .results
            .is_empty());
    }
}
