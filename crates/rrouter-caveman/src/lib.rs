//! Caveman: append a terse-output style instruction to an inbound chat body.
//!
//! This is a prompt directive, not a body transform: the caller's full prompt stays in
//! place and only the response-style guidance is added. The same instruction is aimed at
//! every level, so injection is idempotent, and every path is fail-open — an unexpected
//! body shape is left untouched instead of failing the request.

use rrouter_core::Dialect;
use serde_json::{json, Value};

/// Line shared by every level. Finding it means an instruction is already in the body,
/// so a second copy is never added (even after the operator changes the level).
const MARKER: &str = "Keep code, paths, commands, error text, URLs and identifiers verbatim.";

/// Terse but grammatical: filler, hedging and pleasantries go.
pub const LITE: &str = "Response style: terse. Drop filler, hedging and pleasantries; lead with the answer. \
Keep code, paths, commands, error text, URLs and identifiers verbatim. \
Keep security warnings, irreversible-action confirmations and ordered multi-step sequences in normal prose. \
Answer in the user's language.";

/// Maximum signal per word: articles and filler go, fragments allowed.
pub const FULL: &str = "Response style: maximum signal, minimum words. Drop articles and filler; sentence fragments allowed. \
Shape: [thing] [action] [reason]. [next step]. \
Keep code, paths, commands, error text, URLs and identifiers verbatim. \
Keep security warnings, irreversible-action confirmations and ordered multi-step sequences in normal prose. \
Answer in the user's language.";

/// Telegraphic: conjunctions and filler go, one word when one word is enough.
pub const ULTRA: &str = "Response style: telegraphic. One word when one word suffices; strip conjunctions and filler. \
Keep code, paths, commands, error text, URLs and identifiers verbatim. \
Keep security warnings, irreversible-action confirmations and ordered multi-step sequences in normal prose. \
Answer in the user's language.";

/// How terse the injected instruction asks the model to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Level {
    Lite = 0,
    Full = 1,
    Ultra = 2,
}

impl Level {
    /// Parse a stored level name. Unknown names are rejected, never defaulted.
    pub fn parse(s: &str) -> Option<Level> {
        match s {
            "lite" => Some(Level::Lite),
            "full" => Some(Level::Full),
            "ultra" => Some(Level::Ultra),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Lite => "lite",
            Level::Full => "full",
            Level::Ultra => "ultra",
        }
    }
}

/// The terse-style instruction for one level.
pub fn prompt(level: Level) -> &'static str {
    match level {
        Level::Lite => LITE,
        Level::Full => FULL,
        Level::Ultra => ULTRA,
    }
}

/// Add the instruction to a raw wire body. Returns true when it changed the body.
///
/// Fail-open: an unknown/unsupported shape returns false and never panics.
pub fn inject(body: &mut Value, dialect: Dialect, level: Level) -> bool {
    let text = prompt(level);
    match dialect {
        Dialect::OpenAiChat => chat(body, text),
        Dialect::OpenAiResponses => responses(body, text),
        Dialect::AnthropicMessages => anthropic(body, text),
        // No chat turn to steer.
        Dialect::OpenAiEmbeddings | Dialect::GoogleCodeAssist | Dialect::QoderAgent => false,
    }
}

/// OpenAI Chat Completions: the instruction joins the first system message, or opens a
/// new one when the request has none. User and assistant turns are never touched.
fn chat(body: &mut Value, text: &str) -> bool {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return false;
    };
    let mut first_system = None;
    for (index, message) in messages.iter().enumerate() {
        if message.get("role").and_then(Value::as_str) != Some("system") {
            continue;
        }
        if has_marker(message.get("content")) {
            return false;
        }
        first_system.get_or_insert(index);
    }
    let Some(index) = first_system else {
        messages.insert(0, json!({ "role": "system", "content": text }));
        return true;
    };
    let Some(content) = messages[index].get_mut("content") else {
        return false;
    };
    match content {
        Value::String(existing) => {
            append(existing, text);
            true
        }
        Value::Array(parts) => {
            parts.push(json!({ "type": "text", "text": text }));
            true
        }
        _ => false,
    }
}

/// OpenAI Responses: the instruction joins `instructions`, created when absent.
fn responses(body: &mut Value, text: &str) -> bool {
    let Some(root) = body.as_object_mut() else {
        return false;
    };
    if let Some(existing) = root.get_mut("instructions") {
        return match existing {
            Value::String(existing) => {
                if existing.contains(MARKER) {
                    false
                } else {
                    append(existing, text);
                    true
                }
            }
            Value::Null => {
                *existing = Value::String(text.to_owned());
                true
            }
            _ => false,
        };
    }
    root.insert("instructions".into(), Value::String(text.to_owned()));
    true
}

/// Anthropic Messages: the instruction joins `system`, as text either way it is shaped.
fn anthropic(body: &mut Value, text: &str) -> bool {
    let Some(root) = body.as_object_mut() else {
        return false;
    };
    if let Some(existing) = root.get_mut("system") {
        return match existing {
            Value::String(existing) => {
                if existing.contains(MARKER) {
                    false
                } else {
                    append(existing, text);
                    true
                }
            }
            Value::Array(blocks) => {
                if blocks.iter().any(carries_text) {
                    false
                } else {
                    blocks.push(json!({ "type": "text", "text": text }));
                    true
                }
            }
            Value::Null => {
                *existing = Value::String(text.to_owned());
                true
            }
            _ => false,
        };
    }
    root.insert("system".into(), Value::String(text.to_owned()));
    true
}

fn append(content: &mut String, text: &str) {
    content.push_str("\n\n");
    content.push_str(text);
}

fn has_marker(value: Option<&Value>) -> bool {
    value.is_some_and(carries_text)
}

/// Whether a wire value already carries the instruction, whatever its shape.
fn carries_text(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains(MARKER),
        Value::Array(items) => items.iter().any(carries_text),
        Value::Object(map) => map.get("text").is_some_and(carries_text),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system_content(body: &Value) -> &str {
        body["messages"][0]["content"]
            .as_str()
            .expect("system string")
    }

    #[test]
    fn levels_parse_and_round_trip() {
        for level in [Level::Lite, Level::Full, Level::Ultra] {
            assert_eq!(Level::parse(level.as_str()), Some(level));
            assert!(prompt(level).contains(MARKER), "shared marker line");
        }
        assert_eq!(Level::parse(""), None);
        assert_eq!(Level::parse("FULL"), None);
        assert_eq!(Level::parse("max"), None);
    }

    #[test]
    fn chat_appends_to_a_string_system_prompt() {
        let mut body = json!({
            "model": "gpt-4.1",
            "messages": [
                {"role": "system", "content": "be terse"},
                {"role": "user", "content": "hello"}
            ]
        });
        assert!(inject(&mut body, Dialect::OpenAiChat, Level::Full));
        assert_eq!(
            system_content(&body),
            format!("be terse\n\n{}", prompt(Level::Full))
        );
        assert_eq!(body["messages"][1]["content"], "hello");
    }

    #[test]
    fn chat_appends_a_text_part_to_an_array_system_prompt() {
        let mut body = json!({
            "messages": [{
                "role": "system",
                "content": [{"type": "text", "text": "be terse"}]
            }]
        });
        assert!(inject(&mut body, Dialect::OpenAiChat, Level::Ultra));
        let parts = body["messages"][0]["content"].as_array().expect("parts");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1]["text"], prompt(Level::Ultra));
        assert_eq!(parts[1]["type"], "text");
    }

    #[test]
    fn chat_inserts_a_system_message_when_none_exists() {
        let mut body = json!({
            "messages": [{"role": "user", "content": "hello"}]
        });
        assert!(inject(&mut body, Dialect::OpenAiChat, Level::Lite));
        let messages = body["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], prompt(Level::Lite));
        assert_eq!(messages[1]["content"], "hello");
    }

    #[test]
    fn responses_appends_or_creates_instructions() {
        let mut append = json!({"instructions": "be terse", "input": "hi"});
        assert!(inject(&mut append, Dialect::OpenAiResponses, Level::Full));
        assert_eq!(
            append["instructions"],
            format!("be terse\n\n{}", prompt(Level::Full))
        );

        let mut create = json!({"input": "hi"});
        assert!(inject(&mut create, Dialect::OpenAiResponses, Level::Full));
        assert_eq!(create["instructions"], prompt(Level::Full));
    }

    #[test]
    fn anthropic_handles_string_array_and_absent_system() {
        let mut string = json!({"system": "be terse", "messages": []});
        assert!(inject(&mut string, Dialect::AnthropicMessages, Level::Lite));
        assert_eq!(
            string["system"],
            format!("be terse\n\n{}", prompt(Level::Lite))
        );

        let mut blocks = json!({"system": [{"type": "text", "text": "be terse"}]});
        assert!(inject(&mut blocks, Dialect::AnthropicMessages, Level::Lite));
        let blocks = blocks["system"].as_array().expect("blocks");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1]["type"], "text");
        assert_eq!(blocks[1]["text"], prompt(Level::Lite));

        let mut absent = json!({"messages": []});
        assert!(inject(&mut absent, Dialect::AnthropicMessages, Level::Lite));
        assert_eq!(absent["system"], prompt(Level::Lite));
    }

    #[test]
    fn dialects_without_a_chat_turn_are_untouched() {
        for dialect in [
            Dialect::OpenAiEmbeddings,
            Dialect::GoogleCodeAssist,
            Dialect::QoderAgent,
        ] {
            let original = json!({"input": "hello", "system": "be terse"});
            let mut body = original.clone();
            assert!(!inject(&mut body, dialect, Level::Ultra));
            assert_eq!(body, original);
        }
    }

    #[test]
    fn a_second_injection_is_refused() {
        let mut body = json!({"messages": [{"role": "system", "content": "be terse"}]});
        assert!(inject(&mut body, Dialect::OpenAiChat, Level::Full));
        assert!(!inject(&mut body, Dialect::OpenAiChat, Level::Full));
        // A level change must not stack a second instruction either.
        assert!(!inject(&mut body, Dialect::OpenAiChat, Level::Ultra));
        let content = system_content(&body);
        assert_eq!(content.matches(MARKER).count(), 1);

        let mut created = json!({"messages": [{"role": "user", "content": "hi"}]});
        assert!(inject(&mut created, Dialect::OpenAiChat, Level::Lite));
        assert!(!inject(&mut created, Dialect::OpenAiChat, Level::Lite));
        assert_eq!(created["messages"].as_array().expect("messages").len(), 2);
    }

    #[test]
    fn junk_bodies_fail_open() {
        for dialect in [
            Dialect::OpenAiChat,
            Dialect::OpenAiResponses,
            Dialect::AnthropicMessages,
        ] {
            let mut not_an_object = json!([1, 2, 3]);
            assert!(!inject(&mut not_an_object, dialect, Level::Full));

            let mut wrong_type = json!({
                "messages": "not a list",
                "instructions": 7,
                "system": true
            });
            assert!(!inject(&mut wrong_type, dialect, Level::Full));

            let mut null_body = Value::Null;
            assert!(!inject(&mut null_body, dialect, Level::Full));
        }

        let mut null_content = json!({"messages": [{"role": "system", "content": null}]});
        assert!(!inject(&mut null_content, Dialect::OpenAiChat, Level::Full));
    }
}
