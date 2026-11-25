//! Canonical names for the refs an operator types: combo and alias names, routing group
//! slugs, and custom model ids.
//!
//! One normaliser, called by every write path, is what lets request-time lookup compare
//! one spelling instead of trusting the operator to type it consistently. The rules are
//! deliberately path-shaped, because a name is also a URL/document ref `group/code/model`:
//!
//! - lower-case;
//! - whitespace and any run of separators collapse to a single `-`;
//! - leading and trailing `-` are trimmed;
//! - characters that are not valid in a path segment are dropped;
//! - `.` and `:` survive, because model ids spell versions (`v4.1`) and refs (`combo:x`)
//!   with them;
//! - `/` survives **only** between two non-empty parts, i.e. where it is a real
//!   provider/model (or group/code/model) separator.
//!
//! Lookup tolerance is the other half: stored rows that predate the normaliser (mixed
//! case, stray spaces) keep resolving because a request ref is compared in canonical form
//! against the canonical form of every stored name.

/// Longest canonical name a write path may store.
pub const MAX_CANONICAL_NAME: usize = 200;

/// One-path-segment ref normalised to its canonical spelling.
///
/// Unicode letters and digits are kept as they are (an operator may name a provider in
/// their own script); every other character that cannot sit in a path segment becomes a
/// separator or is dropped.
pub fn canonical_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut pending_dash = false;
    for ch in raw.trim().chars() {
        if ch.is_whitespace() || matches!(ch, '_' | '\u{3000}') {
            pending_dash = true;
            continue;
        }
        if ch == '/' {
            // A slash is meaningful only between two parts; runs collapse like any other
            // separator, so `main//x` and `main / x` are the same ref.
            pending_dash = true;
            continue;
        }
        let keep = ch.is_alphanumeric() || matches!(ch, '.' | ':' | '-');
        if !keep {
            pending_dash = true;
            continue;
        }
        if ch == '-' {
            // A run of separators (a literal `-`, whitespace, `_`) is one `-`.
            pending_dash = true;
            continue;
        }
        if pending_dash && !out.is_empty() && !out.ends_with('-') && !out.ends_with('/') {
            out.push('-');
        }
        pending_dash = false;
        for lower in ch.to_lowercase() {
            out.push(lower);
        }
    }
    while out.len() > MAX_CANONICAL_NAME {
        out.pop();
    }
    out.trim_matches(['-', '/', '.']).to_string()
}

/// Splits a ref on `/`, normalises each segment, drops empty ones, and joins with one
/// `/`: `Main / OAI // DeepSeek-V4.1` becomes `main/oai/deepseek-v4.1`.
///
/// A ref whose segments all normalise away is the empty string, which every write path
/// rejects rather than storing an unreachable name.
pub fn canonical_ref(raw: &str) -> String {
    let mut parts: Vec<String> = raw
        .split('/')
        .map(canonical_name)
        .filter(|part| !part.is_empty())
        .collect();
    let mut joined = parts.join("/");
    while joined.len() > MAX_CANONICAL_NAME {
        joined = match parts.last_mut() {
            Some(last) if last.len() > 1 => {
                last.pop();
                parts.join("/")
            }
            _ => {
                parts.pop();
                parts.join("/")
            }
        };
    }
    joined
}

/// Whether two refs mean the same thing: equal after [`canonical_ref`].
pub fn same_ref(left: &str, right: &str) -> bool {
    canonical_ref(left) == canonical_ref(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_cases_and_collapses_separators() {
        assert_eq!(canonical_name("  Main Group  "), "main-group");
        assert_eq!(canonical_name("main___group"), "main-group");
        assert_eq!(canonical_name("main - - group"), "main-group");
        assert_eq!(canonical_name("--main--"), "main");
    }

    #[test]
    fn keeps_version_and_colon_but_drops_path_hostile_characters() {
        assert_eq!(
            canonical_name("DeepSeek-V4.1-Flash-1017"),
            "deepseek-v4.1-flash-1017"
        );
        assert_eq!(canonical_name("combo:Main"), "combo:main");
        assert_eq!(canonical_name("a?b#c%d"), "a-b-c-d");
        assert_eq!(canonical_name("a/b"), "a-b");
    }

    #[test]
    fn slash_survives_only_between_real_parts() {
        assert_eq!(
            canonical_ref("Main / OAI // DeepSeek-V4.1"),
            "main/oai/deepseek-v4.1"
        );
        assert_eq!(canonical_ref("main/"), "main");
        assert_eq!(canonical_ref("/main"), "main");
        assert_eq!(canonical_ref("main//x/"), "main/x");
        assert_eq!(canonical_ref("  "), "");
    }

    #[test]
    fn mixed_case_rows_match_their_canonical_ref() {
        assert!(same_ref("My Combo", "my-combo"));
        assert!(same_ref("combo:My Combo", "combo:my-combo"));
        assert!(!same_ref("my-combo", "my-combo-2"));
    }

    #[test]
    fn caps_the_length_on_a_segment_boundary() {
        let long = "x".repeat(MAX_CANONICAL_NAME + 50);
        assert_eq!(canonical_ref(&long).len(), MAX_CANONICAL_NAME);
        let deep = format!("{}/{}", "a".repeat(150), "b".repeat(150));
        let capped = canonical_ref(&deep);
        assert!(capped.len() <= MAX_CANONICAL_NAME);
        assert!(capped.starts_with('a') && !capped.ends_with('/'));
    }
}
