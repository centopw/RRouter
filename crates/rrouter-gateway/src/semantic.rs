//! Prompt canonicalisation and vector math for the semantic response cache.
//!
//! The cache needs three things from a canonical [`ChatRequest`]: a stable text to embed,
//! a hash of that text, and a similarity measure between two stored vectors. All three
//! live here so the cache path has no ad-hoc copies and the tests can exercise them
//! without a database or an upstream.

use rrouter_core::{ChatRequest, Part, Role};

/// Stable prompt text for one canonical request.
///
/// Roles and parts are spelled out so two requests cannot collide just because their
/// concatenated human text happens to match: a tool call and a text block with the same
/// payload must not look identical to the embedder. Sampling controls (`temperature`,
/// `top_p`, `max_tokens`) are deliberately *not* part of the text — they change decoding,
/// not the meaning the cache matches on.
pub fn canonical_prompt(request: &ChatRequest) -> String {
    let mut out = String::with_capacity(256);
    if let Some(system) = &request.system {
        out.push_str("system: ");
        out.push_str(system);
        out.push('\n');
    }
    for tool in &request.tools {
        out.push_str("tool: ");
        out.push_str(&tool.name);
        out.push(' ');
        out.push_str(&tool.parameters.to_string());
        out.push('\n');
    }
    for message in &request.messages {
        out.push_str(role_tag(message.role));
        out.push_str(": ");
        for part in &message.parts {
            match part {
                Part::Text(text) => out.push_str(text),
                // The URL is not stable (signed links rotate) and never text the
                // embedder should compare; the presence of an image is the signal.
                Part::Image { .. } => out.push_str("[image]"),
                Part::ToolUse { name, input, .. } => {
                    out.push_str("[tool_use ");
                    out.push_str(name);
                    out.push(' ');
                    out.push_str(&input.to_string());
                    out.push(']');
                }
                Part::ToolResult { content, .. } => {
                    out.push_str("[tool_result ");
                    out.push_str(content);
                    out.push(']');
                }
                // Reasoning is model-internal, not text the client asked to match on.
                Part::Reasoning { .. } => {}
                Part::Opaque(_) => {}
            }
        }
        out.push('\n');
    }
    out
}

fn role_tag(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

/// SHA-256 hex of the prompt text: the exact-text dedupe key inside one cache scope.
pub fn prompt_hash(text: &str) -> String {
    rrouter_store::cache_key(text)
}

/// Raw little-endian `f32` bytes, the format the `semantic_cache.embedding` column holds.
pub fn encode_embedding(values: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// Decode a stored vector. A trailing partial float (a corrupt row) is ignored; the
/// caller's length check in [`cosine`] then treats it as a non-match.
pub fn decode_embedding(bytes: &[u8]) -> Vec<f32> {
    let (chunks, _) = bytes.as_chunks::<4>();
    chunks
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect()
}

/// Cosine similarity, or `0.0` when the vectors cannot be compared.
///
/// Different lengths mean a different embedding model or a corrupt row, and a zero norm
/// has no direction: neither can be "similar", so both are a guaranteed miss rather than
/// a NaN that would poison the comparison.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x), f64::from(*y));
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    (dot / (norm_a.sqrt() * norm_b.sqrt())) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use rrouter_core::{Message, ToolDef};
    use serde_json::json;

    fn request(text: &str) -> ChatRequest {
        let mut request = ChatRequest::new("m");
        request.system = Some("be terse".to_string());
        request.messages.push(Message::text(Role::User, text));
        request
    }

    #[test]
    fn canonical_prompt_is_stable_and_role_aware() {
        let first = canonical_prompt(&request("hello"));
        let again = canonical_prompt(&request("hello"));
        assert_eq!(first, again);
        assert!(first.contains("system: be terse"), "{first}");
        assert!(first.contains("user: hello"), "{first}");
        assert_ne!(
            first,
            canonical_prompt(&request("goodbye")),
            "different text, different prompt"
        );
    }

    #[test]
    fn canonical_prompt_separates_tools_and_roles() {
        let mut with_tool = request("hello");
        with_tool.tools.push(ToolDef {
            name: "lookup".to_string(),
            description: None,
            parameters: json!({"type": "object"}),
            strict: false,
        });
        assert_ne!(
            canonical_prompt(&with_tool),
            canonical_prompt(&request("hello")),
            "tool definitions change what the model can do"
        );
        assert!(canonical_prompt(&with_tool).contains("tool: lookup"));
    }

    #[test]
    fn embedding_roundtrip_and_cosine() {
        let values = vec![0.5f32, -0.25, 1.0];
        let bytes = encode_embedding(&values);
        assert_eq!(bytes.len(), 12);
        assert_eq!(decode_embedding(&bytes), values);

        assert!((cosine(&values, &values) - 1.0).abs() < 1e-6);
        assert_eq!(cosine(&values, &[1.0, 1.0]).abs(), 0.0, "length mismatch");
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0, "zero norm");
        assert!(cosine(&values, &[-0.5, 0.25, -1.0]) < -0.99);
    }

    #[test]
    fn prompt_hash_is_hex_and_content_addressed() {
        let hash = prompt_hash("hello");
        assert_eq!(hash.len(), 64);
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(hash, prompt_hash("goodbye"));
    }
}
