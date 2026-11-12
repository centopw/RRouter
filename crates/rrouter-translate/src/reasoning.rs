//! Normalized reasoning controls shared by every dialect.
//!
//! Clients express thinking strength in three shapes: OpenAI `reasoning_effort` (a
//! string), a unified `reasoning` object (`effort`, `max_tokens`, `enabled`,
//! `exclude`), or Anthropic `thinking`/`output_config.effort`. Each encoder maps the
//! canonical [`Reasoning`] to its provider-native field; no dialect reads another's.

use rrouter_core::{Dialect, Error, Reasoning, Result};
use serde_json::{json, Value};

/// Effort values the unified contract accepts, strongest first.
pub(crate) const EFFORTS: &[&str] = &["max", "xhigh", "high", "medium", "low", "minimal", "none"];

fn parse_err(dialect: Dialect, msg: &str) -> Error {
    Error::Translation(format!("{dialect}: {msg}"))
}

fn effort(value: &Value, dialect: Dialect) -> Result<Option<String>> {
    let Some(raw) = value.as_str() else {
        return Err(parse_err(dialect, "reasoning effort must be a string"));
    };
    let effort = raw.trim().to_lowercase();
    if !EFFORTS.contains(&effort.as_str()) {
        return Err(parse_err(
            dialect,
            &format!("unknown reasoning effort `{raw}`; use {EFFORTS:?}"),
        ));
    }
    Ok(Some(effort))
}

fn positive(value: &Value, dialect: Dialect, field: &str) -> Result<Option<u32>> {
    match value {
        Value::Null => Ok(None),
        other => other
            .as_u64()
            .filter(|value| *value > 0)
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| parse_err(dialect, &format!("{field} must be a positive integer"))),
    }
}

/// Parse the unified `reasoning` object (OpenAI Chat/Responses shape).
pub(crate) fn parse_object(value: &Value, dialect: Dialect) -> Result<Option<Reasoning>> {
    if value.is_null() {
        return Ok(None);
    }
    let Some(object) = value.as_object() else {
        return Err(parse_err(dialect, "`reasoning` must be an object"));
    };
    let effort = match object.get("effort") {
        None | Some(Value::Null) => None,
        Some(raw) => effort(raw, dialect)?,
    };
    let max_tokens = match object.get("max_tokens") {
        None => None,
        Some(raw) => positive(raw, dialect, "reasoning.max_tokens")?,
    };
    let enabled = match object.get("enabled") {
        None => None,
        Some(raw) => Some(
            raw.as_bool()
                .ok_or_else(|| parse_err(dialect, "reasoning.enabled must be a boolean"))?,
        ),
    };
    let exclude = match object.get("exclude") {
        None => None,
        Some(raw) => Some(
            raw.as_bool()
                .ok_or_else(|| parse_err(dialect, "reasoning.exclude must be a boolean"))?,
        ),
    };
    if effort.is_none() && max_tokens.is_none() && enabled.is_none() && exclude.is_none() {
        return Ok(None);
    }
    Ok(Some(Reasoning {
        effort,
        max_tokens,
        enabled,
        exclude,
    }))
}

/// Parse OpenAI's `reasoning_effort` string.
pub(crate) fn parse_effort_field(value: &Value, dialect: Dialect) -> Result<Option<Reasoning>> {
    if value.is_null() {
        return Ok(None);
    }
    Ok(effort(value, dialect)?.map(|effort| Reasoning {
        effort: Some(effort),
        max_tokens: None,
        enabled: None,
        exclude: None,
    }))
}

/// Parse Anthropic `thinking` plus `output_config.effort`.
pub(crate) fn parse_anthropic(body: &Value) -> Result<Option<Reasoning>> {
    let thinking = body.get("thinking").and_then(Value::as_object);
    let native_effort = body
        .get("output_config")
        .and_then(|config| config.get("effort"));
    if thinking.is_none() && native_effort.is_none() {
        return Ok(None);
    }
    let kind = thinking
        .and_then(|thinking| thinking.get("type"))
        .and_then(Value::as_str);
    let enabled = match kind {
        Some("enabled") | Some("adaptive") => Some(true),
        Some("disabled") => Some(false),
        _ => None,
    };
    let max_tokens = thinking
        .and_then(|thinking| thinking.get("budget_tokens"))
        .filter(|value| !value.is_null())
        .map(|value| positive(value, Dialect::AnthropicMessages, "thinking.budget_tokens"))
        .transpose()?
        .flatten();
    let effort = match native_effort {
        None | Some(Value::Null) => None,
        Some(raw) => effort(raw, Dialect::AnthropicMessages)?,
    };
    // A client that only sends a budget still has a level: infer it from the share of
    // `max_tokens`, so a later OpenAI turn can carry `reasoning_effort`.
    let effort = effort.or_else(|| {
        let max = body
            .get("max_tokens")
            .and_then(Value::as_u64)
            .filter(|v| *v > 0)?;
        let budget = u64::from(max_tokens?);
        let share = budget as f64 / max as f64;
        Some(
            match share {
                s if s < 0.15 => "minimal",
                s if s < 0.35 => "low",
                s if s < 0.65 => "medium",
                s if s < 0.90 => "high",
                _ => "max",
            }
            .to_string(),
        )
    });
    if enabled.is_none() && max_tokens.is_none() && effort.is_none() {
        return Ok(None);
    }
    Ok(Some(Reasoning {
        effort,
        max_tokens,
        enabled,
        exclude: None,
    }))
}

/// Whether the client wants thinking on. An absent `enabled` follows the effort.
pub(crate) fn is_enabled(reasoning: &Reasoning) -> bool {
    reasoning
        .enabled
        .unwrap_or_else(|| reasoning.effort.as_deref() != Some("none"))
}

/// Effort to send to an OpenAI-shaped provider. `none` disables; `max`/`xhigh` are
/// OpenAI's own 2026 levels and stay verbatim.
pub(crate) fn openai_effort(reasoning: &Reasoning) -> Option<String> {
    if !is_enabled(reasoning) {
        return Some("none".to_string());
    }
    reasoning.effort.clone()
}

/// Anthropic requires `1024 <= budget_tokens < max_tokens`. Returns the budget and the
/// maximum that must be sent so the pair is valid.
pub(crate) fn anthropic_budget(
    reasoning: &Reasoning,
    configured: Option<u32>,
) -> Option<(u32, u32)> {
    if !is_enabled(reasoning) {
        return None;
    }
    let configured = configured.unwrap_or(8_192).max(2_048);
    let share = match reasoning.effort.as_deref() {
        Some("minimal") => 0.10,
        Some("low") => 0.20,
        Some("medium") => 0.50,
        Some("high") => 0.80,
        Some("xhigh") | Some("max") => 0.95,
        _ => 0.50,
    };
    let wanted = reasoning
        .max_tokens
        .unwrap_or(((configured as f64) * share) as u32)
        .max(1_024);
    let max_tokens = configured.max(wanted + 1_024);
    let budget = wanted.min(max_tokens - 1_024).max(1_024);
    Some((budget, max_tokens))
}

/// Gemini's budget in thinking tokens, or a Gemini 3 thinking level.
pub(crate) fn google_thinking(reasoning: &Reasoning, model: &str) -> Value {
    if !is_enabled(reasoning) {
        return if model.contains("gemini-3") {
            json!({"thinkingLevel": "low", "includeThoughts": false})
        } else {
            json!({"thinkingBudget": 0, "includeThoughts": false})
        };
    }
    if model.contains("gemini-3") {
        let level = match reasoning.effort.as_deref() {
            Some("minimal") | Some("low") => "low",
            _ => "high",
        };
        return json!({"thinkingLevel": level, "includeThoughts": true});
    }
    let budget = reasoning.max_tokens.unwrap_or_else(|| {
        ((8_192.0
            * match reasoning.effort.as_deref() {
                Some("minimal") => 0.10,
                Some("low") => 0.20,
                Some("medium") => 0.50,
                Some("high") => 0.80,
                Some("xhigh") | Some("max") => 0.95,
                _ => 0.50,
            }) as u32)
            .max(1_024)
    });
    json!({"thinkingBudget": budget, "includeThoughts": true})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_parses_all_four_controls_and_rejects_unknown_effort() {
        let parsed = parse_object(
            &json!({"effort":"HIGH","max_tokens":2048,"enabled":true,"exclude":true}),
            Dialect::OpenAiChat,
        )
        .expect("parse")
        .expect("some");
        assert_eq!(parsed.effort.as_deref(), Some("high"));
        assert_eq!(parsed.max_tokens, Some(2048));
        assert_eq!(parsed.enabled, Some(true));
        assert_eq!(parsed.exclude, Some(true));

        assert!(parse_object(&json!({"effort":"turbo"}), Dialect::OpenAiChat).is_err());
        assert!(parse_object(&json!({"max_tokens":0}), Dialect::OpenAiChat).is_err());
    }

    #[test]
    fn anthropic_thinking_and_effort_parse() {
        let thinking =
            parse_anthropic(&json!({"thinking":{"type":"enabled","budget_tokens":4096}}))
                .expect("parse")
                .expect("some");
        assert_eq!(thinking.max_tokens, Some(4096));
        assert_eq!(thinking.enabled, Some(true));

        let adaptive = parse_anthropic(&json!({"thinking":{"type":"adaptive"}}))
            .expect("parse")
            .expect("some");
        assert_eq!(adaptive.enabled, Some(true));

        let effort = parse_anthropic(&json!({"output_config":{"effort":"low"}}))
            .expect("parse")
            .expect("some");
        assert_eq!(effort.effort.as_deref(), Some("low"));
    }

    #[test]
    fn anthropic_budget_stays_below_max_tokens() {
        let reasoning = Reasoning {
            effort: Some("high".into()),
            max_tokens: None,
            enabled: Some(true),
            exclude: None,
        };
        let (budget, max_tokens) = anthropic_budget(&reasoning, Some(8_000)).expect("budget");
        assert!(budget >= 1_024);
        assert!(max_tokens > budget);
    }

    #[test]
    fn gemini_3_uses_a_level_and_older_models_use_a_budget() {
        let reasoning = Reasoning {
            effort: Some("high".into()),
            max_tokens: None,
            enabled: Some(true),
            exclude: None,
        };
        assert_eq!(
            google_thinking(&reasoning, "gemini-3.5-flash")["thinkingLevel"],
            "high"
        );
        assert_eq!(
            google_thinking(&reasoning, "gemini-2.5-pro")["thinkingBudget"],
            6_553
        );
    }
}
