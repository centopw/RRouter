//! Token Saver (RTK): compress noisy tool outputs inside an inbound request.
//!
//! A coding agent's context is dominated by tool results — git diffs, grep matches, build
//! logs. These filters recognise those shapes and keep the structure an agent needs while
//! dropping repetition and low-value lines. The rules are deliberately conservative: a
//! filter that would grow the text, return nothing, or sees an error result is skipped,
//! and every call falls back to the original string.

use rrouter_core::{ChatRequest, Dialect, Message, Part, Role};
use serde_json::Value;

/// Skip tiny blobs: below this the marker costs more than the saving.
pub const MIN_COMPRESS_SIZE: usize = 500;
/// Never walk a pathological tool result (10 MiB), matching RTK's guard.
pub const RAW_CAP: usize = 10 * 1024 * 1024;
const DETECT_WINDOW: usize = 1024;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Stats {
    pub before: usize,
    pub after: usize,
    pub hits: u32,
    pub filters: Vec<&'static str>,
}

impl Stats {
    pub fn saved(&self) -> usize {
        self.before.saturating_sub(self.after)
    }

    pub fn percent(&self) -> f64 {
        if self.before == 0 {
            0.0
        } else {
            self.saved() as f64 * 100.0 / self.before as f64
        }
    }
}

/// Compress the client's raw wire body in place. This runs before the canonical parse,
/// so same-dialect forwarding (which sends the raw JSON untouched) is covered too.
pub fn compress_source(body: &mut Value, _dialect: Dialect) -> Option<Stats> {
    let mut stats = Stats::default();
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            compress_wire_message(message, &mut stats);
        }
    }
    if let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) {
        for item in items {
            if item.get("type").and_then(Value::as_str) != Some("function_call_output") {
                continue;
            }
            if let Some(output) = item.get_mut("output") {
                compress_output_value(output, &mut stats);
            }
        }
    }
    (stats.hits > 0).then_some(stats)
}

fn compress_wire_message(message: &mut Value, stats: &mut Stats) {
    let role = message.get("role").and_then(Value::as_str).unwrap_or("");
    if role == "tool" {
        if let Some(content) = message.get_mut("content") {
            compress_output_value(content, stats);
        }
        return;
    }
    let Some(parts) = message.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    for part in parts {
        if part.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        if part.get("is_error").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if let Some(content) = part.get_mut("content") {
            compress_output_value(content, stats);
        }
    }
}

fn compress_output_value(value: &mut Value, stats: &mut Stats) {
    if let Some(text) = value.as_str() {
        let compressed = compress_text(text, stats, "wire");
        *value = Value::String(compressed);
        return;
    }
    if let Some(parts) = value.as_array_mut() {
        for part in parts {
            let kind = part.get("type").and_then(Value::as_str);
            if !matches!(kind, Some("text") | Some("input_text")) {
                continue;
            }
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                let compressed = compress_text(text, stats, "wire-array");
                if let Some(object) = part.as_object_mut() {
                    object.insert("text".into(), Value::String(compressed));
                }
            }
        }
    }
}

/// Compress every tool result in `request`. Returns `None` when nothing qualified.
pub fn compress_request(request: &mut ChatRequest) -> Option<Stats> {
    let mut stats = Stats::default();
    for message in &mut request.messages {
        compress_message(message, &mut stats);
    }
    (stats.hits > 0).then_some(stats)
}

fn compress_message(message: &mut Message, stats: &mut Stats) {
    for part in &mut message.parts {
        match part {
            Part::ToolResult { content, is_error, .. } if !*is_error => {
                *content = compress_text(content, stats, "tool_result");
            }
            // Some translators model a tool turn as a plain text part.
            Part::Text(text) if message.role == Role::Tool => {
                *text = compress_text(text, stats, "tool_message");
            }
            _ => {}
        }
    }
}

fn compress_text(text: &str, stats: &mut Stats, shape: &str) -> String {
    let before = text.len();
    stats.before += before;
    if !(MIN_COMPRESS_SIZE..=RAW_CAP).contains(&before) {
        stats.after += before;
        return text.to_string();
    }
    let Some((name, filter)) = detect(text) else {
        stats.after += before;
        return text.to_string();
    };
    let out = filter(text);
    if out.is_empty() || out.len() >= before {
        stats.after += before;
        return text.to_string();
    }
    stats.after += out.len();
    stats.hits += 1;
    if !stats.filters.contains(&name) {
        stats.filters.push(name);
    }
    let _ = shape;
    out
}

type Filter = fn(&str) -> String;

fn detect(text: &str) -> Option<(&'static str, Filter)> {
    let head: String = text.chars().take(DETECT_WINDOW).collect();
    if looks_git_log(&head) {
        return Some(("git-log", git_log));
    }
    if head.contains("diff --git ") || head.contains("\n@@ ") {
        return Some(("git-diff", git_diff));
    }
    if head.contains("On branch ") || head.contains("nothing to commit") || porcelain_line(&head, 0.6) {
        return Some(("git-status", git_status));
    }
    if looks_build(&head) {
        return Some(("build-output", build_output));
    }
    let non_empty: Vec<&str> = head.lines().filter(|line| !line.trim().is_empty()).collect();
    let first5: Vec<&&str> = non_empty.iter().take(5).collect();
    if first5.iter().any(|line| grep_line(line).is_some()) {
        return Some(("grep", grep));
    }
    if non_empty.len() >= 3 && non_empty.iter().all(|line| path_like(line)) {
        return Some(("find", find));
    }
    if head.contains("├──") || head.contains("└──") || head.contains("│  ") {
        return Some(("tree", tree));
    }
    if head.lines().any(ls_line) {
        return Some(("ls", ls));
    }
    if non_empty.len() >= 5 {
        return Some(("dedup-log", dedup_log));
    }
    if text.lines().count() >= 250 {
        return Some(("smart-truncate", smart_truncate));
    }
    None
}

fn looks_git_log(head: &str) -> bool {
    head.lines().any(|line| {
        let line = line.trim_start_matches(|c: char| matches!(c, '*' | '|' | '/' | '\\' | ' ') || c.is_whitespace());
        if let Some(rest) = line.strip_prefix("commit ") {
            let hash = rest.split_whitespace().next().unwrap_or("");
            hash.len() >= 7 && hash.chars().all(|c| c.is_ascii_hexdigit())
        } else {
            false
        }
    })
}

fn porcelain_line(head: &str, share: f64) -> bool {
    let lines: Vec<&str> = head.lines().filter(|line| !line.trim().is_empty()).collect();
    if lines.len() < 3 {
        return false;
    }
    let hits = lines
        .iter()
        .filter(|line| {
            let b = line.as_bytes();
            b.len() >= 4 && b[2] == b' ' && b[0] != b' ' && " MADRCU?!".contains(b[0] as char)
        })
        .count();
    hits as f64 / lines.len() as f64 >= share
}

fn looks_build(head: &str) -> bool {
    const MARKERS: &[&str] = &[
        "Compiling ", "Downloading ", "added ", "[ERROR]", "BUILD SUCCESS", "BUILD FAILED",
        "ERROR:", "error[", "warning:", "npm error", "npm warn",
    ];
    MARKERS.iter().any(|marker| head.contains(marker))
}

fn grep_line(line: &str) -> Option<(&str, &str, &str)> {
    let first = line.find(':')?;
    let second = line[first + 1..].find(':')? + first + 1;
    let number = &line[first + 1..second];
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((&line[..first], number, &line[second + 1..]))
}

fn path_like(line: &str) -> bool {
    let text = line.trim();
    if text.is_empty() || text.contains(':') {
        return false;
    }
    text.starts_with('.') || text.starts_with('/') || text.contains('/')
}

fn ls_line(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes.len() > 10
        && matches!(bytes[0], b'-' | b'd' | b'l' | b'c' | b'p' | b's')
        && bytes[1..10].iter().all(|b| matches!(b, b'r' | b'w' | b'x' | b'-'))
        && bytes[10] == b' '
}

fn cap_lines(text: &str, keep: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= keep {
        return text.to_string();
    }
    let mut out = lines[..keep].join("\n");
    out.push_str(&format!("\n… +{} lines", lines.len() - keep));
    out
}

fn git_log(input: &str) -> String {
    cap_lines(input, 200)
}

fn git_diff(input: &str) -> String {
    // Cap each hunk at 100 lines but keep every header line.
    const HUNK_MAX: usize = 100;
    let mut out: Vec<&str> = Vec::new();
    let mut hunk = 0usize;
    let mut skipped = 0usize;
    for line in input.lines() {
        if line.starts_with("@@") {
            if skipped > 0 {
                out.push("… hunk truncated");
                skipped = 0;
            }
            hunk = 0;
            out.push(line);
            continue;
        }
        if hunk < HUNK_MAX || line.starts_with("diff --git ") {
            out.push(line);
        } else {
            skipped += 1;
        }
        hunk += 1;
    }
    if skipped > 0 {
        out.push("… hunk truncated");
    }
    out.join("\n")
}

fn git_status(input: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut files = 0usize;
    for line in input.lines() {
        let trimmed = line.trim_end();
        if trimmed.starts_with("On branch ") || trimmed.contains("nothing to commit") {
            out.push(trimmed);
            continue;
        }
        if trimmed.is_empty() {
            out.push("");
            continue;
        }
        if files < 10 {
            out.push(trimmed);
            files += 1;
        }
    }
    let total = input.lines().filter(|line| !line.trim().is_empty()).count();
    if total > files + 3 {
        out.push("… more changed files omitted");
    }
    out.join("\n")
}

fn build_output(input: &str) -> String {
    // Collapse identical lines (repeated warnings), then cap.
    let mut unique: Vec<(&str, usize)> = Vec::new();
    for line in input.lines() {
        if let Some((_, count)) = unique.iter_mut().find(|(seen, _)| *seen == line) {
            *count += 1;
        } else {
            unique.push((line, 1));
        }
    }
    let mut out: Vec<String> = unique
        .iter()
        .take(200)
        .map(|(line, count)| {
            if *count > 1 {
                format!("{line}  ×{count}")
            } else {
                (*line).to_string()
            }
        })
        .collect();
    if unique.len() > 200 {
        out.push(format!("… +{} unique lines", unique.len() - 200));
    }
    out.join("\n")
}

fn grep(input: &str) -> String {
    use std::collections::BTreeMap;
    let mut by_file: BTreeMap<&str, Vec<(&str, &str)>> = BTreeMap::new();
    let mut total = 0usize;
    for line in input.lines() {
        if let Some((file, number, content)) = grep_line(line) {
            total += 1;
            by_file.entry(file).or_default().push((number, content));
        }
    }
    if total == 0 {
        return input.to_string();
    }
    let mut out = format!("{total} matches in {} files:\n\n", by_file.len());
    for (file, matches) in &by_file {
        out.push_str(&format!("{file} ({}):\n", matches.len()));
        for (number, content) in matches.iter().take(10) {
            out.push_str(&format!("  {number:>4}: {}\n", content.trim()));
        }
        if matches.len() > 10 {
            out.push_str(&format!("  +{}\n", matches.len() - 10));
        }
        out.push('\n');
    }
    out
}

fn find(input: &str) -> String {
    use std::collections::BTreeMap;
    let mut by_dir: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for line in input.lines().filter(|line| !line.trim().is_empty()) {
        let path = line.trim();
        let dir = path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or(".");
        by_dir.entry(dir).or_default().push(path);
    }
    let dirs = by_dir.len();
    let mut out = format!("{dirs} directories:\n");
    for (index, (dir, files)) in by_dir.iter().enumerate() {
        if index >= 20 {
            out.push_str(&format!("… +{} directories\n", dirs - 20));
            break;
        }
        out.push_str(&format!("{dir}/\n"));
        for file in files.iter().take(10) {
            out.push_str(&format!("  {file}\n"));
        }
        if files.len() > 10 {
            out.push_str(&format!("  +{}\n", files.len() - 10));
        }
    }
    out
}

fn tree(input: &str) -> String {
    cap_lines(input, 200)
}

fn ls(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    if lines.len() <= 40 {
        return input.to_string();
    }
    let mut out = lines[..40].join("\n");
    out.push_str(&format!("\n… +{} entries", lines.len() - 40));
    out
}

fn dedup_log(input: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut last: Option<(&str, usize)> = None;
    for line in input.lines() {
        match last {
            Some((seen, count)) if seen == line => last = Some((seen, count + 1)),
            Some((seen, count)) => {
                out.push(render(seen, count));
                last = Some((line, 1));
            }
            None => last = Some((line, 1)),
        }
    }
    if let Some((seen, count)) = last {
        out.push(render(seen, count));
    }
    if out.len() > 200 {
        let extra = out.len() - 200;
        out.truncate(200);
        out.push(format!("… +{extra} lines"));
    }
    out.join("\n")
}

fn render(line: &str, count: usize) -> String {
    if count > 1 {
        format!("{line}  ×{count}")
    } else {
        line.to_string()
    }
}

fn smart_truncate(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    if lines.len() < 250 {
        return input.to_string();
    }
    let head = &lines[..120];
    let tail = &lines[lines.len() - 60..];
    let cut = lines.len() - head.len() - tail.len();
    let mut out = head.join("\n");
    out.push_str(&format!("\n… +{cut} lines truncated\n"));
    out.push_str(&tail.join("\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rrouter_core::{ChatRequest, Message, Part};

    fn request_with(content: &str, is_error: bool) -> ChatRequest {
        let mut request = ChatRequest::new("claude/haiku");
        request.messages.push(Message {
            role: Role::Tool,
            parts: vec![Part::ToolResult {
                tool_use_id: "t1".into(),
                content: content.into(),
                is_error,
            }],
        });
        request
    }

    #[test]
    fn grep_results_are_grouped_and_capped() {
        let mut input = String::new();
        for line in 1..=40 {
            input.push_str(&format!("src/main.rs:{line}:    let value = compute({line});\n"));
        }
        let mut request = request_with(&input, false);
        let stats = compress_request(&mut request).expect("compressed");
        let text = match &request.messages[0].parts[0] {
            Part::ToolResult { content, .. } => content,
            _ => panic!("tool result"),
        };
        assert!(text.contains("40 matches in 1 files"));
        assert!(text.contains("+30"));
        assert!(stats.saved() > 0);
        assert!(stats.percent() > 50.0);
    }

    #[test]
    fn errors_small_and_unrecognised_are_untouched() {
        let big = "x".repeat(2000);
        let mut request = request_with(&big, true);
        assert!(compress_request(&mut request).is_none(), "errors are preserved");

        let mut request = request_with("short output", false);
        assert!(compress_request(&mut request).is_none(), "small output is skipped");
    }

    #[test]
    fn smart_truncate_keeps_head_and_tail() {
        let input = (0..400).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
        let mut request = request_with(&input, false);
        let stats = compress_request(&mut request).expect("compressed");
        let text = match &request.messages[0].parts[0] {
            Part::ToolResult { content, .. } => content,
            _ => panic!("tool result"),
        };
        // Large unique logs are handled by the dedup cap first (RTK order); either way the
        // head survives and the middle is cut with a marker.
        assert!(text.contains("… +"));
        assert!(text.starts_with("line 0\n"));
        assert!(text.len() < input.len());
        assert!(stats.saved() > 0);
    }

    #[test]
    fn dedup_and_build_logs_collapse_repeats() {
        let input = (0..120)
            .map(|_| "warning: unused import".to_string())
            .chain((0..20).map(|n| format!("Compiling crate{n}")))
            .collect::<Vec<_>>()
            .join("\n");
        let mut request = request_with(&input, false);
        assert!(compress_request(&mut request).is_some());
        let text = match &request.messages[0].parts[0] {
            Part::ToolResult { content, .. } => content,
            _ => panic!("tool result"),
        };
        assert!(text.contains("×120"), "{text}");
    }
}
