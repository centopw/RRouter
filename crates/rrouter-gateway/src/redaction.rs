//! PII redaction: a pure text scanner with a small, documented policy.
//!
//! Four classes are recognised and replaced with stable placeholders:
//!
//! | Class | Placeholder | Recognised as |
//! | --- | --- | --- |
//! | email | `[email]` | `local@domain.tld` with an alphabetic TLD |
//! | phone | `[phone]` | `+`-prefixed international numbers, or formatted national numbers with three groups |
//! | card | `[card]` | 13–19 digits (space/dash separated) that pass the Luhn check |
//! | secret | `[secret]` | `Bearer`/`Basic` auth values and known key shapes (`sk-…`, `ghp_…`, `xoxb-…`, `AKIA…`, `AIza…`, JWT triples) |
//!
//! Design rules, each paid for by a test:
//!
//! - **No whole-text buffering decision is made here**: the input is one `&str`, the output
//!   is borrowed when nothing matched, so the caller decides how to chunk. Streams use
//!   [`StreamRedactor`], which holds only a bounded carry (see [`STREAM_CARRY_BYTES`]).
//! - **Conservative about numbers.** A long number is only a card when Luhn passes (and it
//!   is not all one digit), and an unprefixed digit run is only a phone when it is
//!   formatted into three groups — a bare 10-digit run is as likely to be a timestamp or
//!   an order id, so it is left alone. A card-shaped run (`4-4-…`) is never treated as a
//!   phone, so a benign 16-digit order number keeps every digit instead of losing three
//!   groups to `[phone]`.
//! - **Deterministic overlap policy.** Candidates are sorted by earliest start, then the
//!   longest match, then class priority (`secret` > `card` > `email` > `phone`); any
//!   candidate overlapping an accepted one is dropped. No two placeholders overlap, and
//!   the same input always produces the same output.
//! - **UTF-8 safe.** Patterns are ASCII-only, so a match always begins and ends on a
//!   character boundary; multibyte text between matches is copied verbatim.
//! - **Nothing secret is retained.** [`RedactionReport`] holds counts only; its `Debug` and
//!   `Display` cannot print an input value, because no input value is stored.
//!
//! What it is not: an NER model, a DLP engine, or a guarantee. Names, addresses, and
//! unstructured secrets (a PEM block, a password in prose) are out of scope; the wiring
//! contract in `docs/GUARDRAILS.md` says where to run it and what remains the caller's job.

use std::borrow::Cow;
use std::fmt;

use rrouter_core::{ChatRequest, ChatResponse, Part};
use serde_json::Value;

/// The classes this scanner knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PiiClass {
    Email,
    Phone,
    Card,
    Secret,
}

impl PiiClass {
    /// Every class, in stable report order.
    pub const ALL: [PiiClass; 4] = [
        PiiClass::Email,
        PiiClass::Phone,
        PiiClass::Card,
        PiiClass::Secret,
    ];

    /// Index into [`RedactionReport::counts`].
    pub const fn index(self) -> usize {
        match self {
            Self::Email => 0,
            Self::Phone => 1,
            Self::Card => 2,
            Self::Secret => 3,
        }
    }

    /// Stable label for logs and metrics (`rrouter_redactions_total{class="email"}`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Phone => "phone",
            Self::Card => "card",
            Self::Secret => "secret",
        }
    }

    /// The text that replaces a match. Stable: receivers and tests can rely on it.
    pub const fn placeholder(self) -> &'static str {
        match self {
            Self::Email => "[email]",
            Self::Phone => "[phone]",
            Self::Card => "[card]",
            Self::Secret => "[secret]",
        }
    }
}

/// What was removed, by class.
///
/// Counts only: no original value is stored, so this type is safe to log, serialize into a
/// usage row, or export as a metric. `redacted` is `true` exactly when a count is non-zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RedactionReport {
    pub redacted: bool,
    pub counts: [u64; 4],
}

impl RedactionReport {
    pub fn count(&self, class: PiiClass) -> u64 {
        self.counts[class.index()]
    }

    pub fn total(&self) -> u64 {
        self.counts.iter().sum()
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

impl std::ops::AddAssign for RedactionReport {
    fn add_assign(&mut self, other: Self) {
        for (slot, add) in self.counts.iter_mut().zip(other.counts) {
            *slot += add;
        }
        self.redacted = self.total() > 0;
    }
}

impl fmt::Display for RedactionReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "redacted {}", self.total())?;
        for class in PiiClass::ALL {
            let count = self.count(class);
            if count > 0 {
                write!(f, " {}={count}", class.as_str())?;
            }
        }
        Ok(())
    }
}

/// One detected span, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Candidate {
    start: usize,
    end: usize,
    class: PiiClass,
}

/// Replace every detected value with its class placeholder.
///
/// Returns the input unchanged (borrowed, no allocation) when nothing matched.
pub fn redact(text: &str) -> (Cow<'_, str>, RedactionReport) {
    let matches = resolve(candidates(text));
    let mut report = RedactionReport::default();
    if matches.is_empty() {
        return (Cow::Borrowed(text), report);
    }

    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for entry in &matches {
        out.push_str(&text[cursor..entry.start]);
        out.push_str(entry.class.placeholder());
        report.counts[entry.class.index()] += 1;
        cursor = entry.end;
    }
    out.push_str(&text[cursor..]);
    report.redacted = true;
    (Cow::Owned(out), report)
}

/// Raw bytes the streaming redactor holds back before emitting. A match must fit inside
/// this many bytes to be caught when it is split across deltas; longer runs can straddle
/// the emission cut. Kept small enough that a live chat is not visibly delayed (the first
/// text of a short response may wait for the stream to end, and longer responses emit
/// after roughly this many bytes), and large enough for the email/card/secret shapes this
/// scanner recognises.
pub const STREAM_CARRY_BYTES: usize = 512;

/// Incremental redaction for streamed text.
///
/// The wire path feeds each text delta to [`push`](Self::push) and writes whatever comes
/// back; [`finish`](Self::finish) flushes the tail at `Done`, at an upstream/decode error,
/// or when a stream is abandoned. Only the last [`STREAM_CARRY_BYTES`] raw bytes are held,
/// so a match split across two deltas is still seen whole once its tail arrives.
///
/// Emission rules:
///
/// - a match that ends at the current buffer end is held (it may still grow);
/// - a match that crosses the emission cut is held, not split;
/// - nothing already emitted is ever rescanned, so placeholders cannot be redacted twice
///   and a match is counted once.
///
/// `finish` is idempotent. The buffer never exceeds one delta plus the carry bound.
#[derive(Debug, Default)]
pub struct StreamRedactor {
    carry: String,
}

impl StreamRedactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one delta; returns the text that is safe to emit now (possibly empty).
    pub fn push(&mut self, delta: &str) -> (String, RedactionReport) {
        self.carry.push_str(delta);
        self.emit_ready(false)
    }

    /// Flush the carried tail. Safe to call more than once.
    pub fn finish(&mut self) -> (String, RedactionReport) {
        self.emit_ready(true)
    }

    /// Flush after an upstream error, decode error or guardrail stop. Complete matches are
    /// redacted, and a trailing token that looks like the *start* of a PII value (an
    /// unterminated email, a long digit run, a known secret prefix) is dropped rather than
    /// emitted: no complete match exists to replace it, and emitting its prefix would be
    /// the one leak this path could otherwise cause.
    pub fn finish_interrupted(&mut self) -> (String, RedactionReport) {
        let (out, report) = self.emit_ready(true);
        (drop_partial_token(&out), report)
    }

    fn emit_ready(&mut self, final_flush: bool) -> (String, RedactionReport) {
        let mut report = RedactionReport::default();
        if self.carry.is_empty() {
            return (String::new(), report);
        }
        let spans = resolve(candidates(&self.carry));
        let len = self.carry.len();
        let mut cut = if final_flush {
            len
        } else {
            floor_char_boundary(&self.carry, len.saturating_sub(STREAM_CARRY_BYTES))
        };
        if !final_flush {
            // A match touching the buffer end may continue in the next delta.
            if let Some(last) = spans.last() {
                if last.end == len {
                    cut = cut.min(last.start);
                }
            }
            // Never cut inside a match: hold from its start instead.
            for span in &spans {
                if span.start < cut && cut < span.end {
                    cut = span.start;
                }
            }
        }
        if cut == 0 {
            return (String::new(), report);
        }
        let mut out = String::with_capacity(cut);
        let mut cursor = 0;
        for span in &spans {
            if span.end > cut {
                break;
            }
            out.push_str(&self.carry[cursor..span.start]);
            out.push_str(span.class.placeholder());
            report.counts[span.class.index()] += 1;
            cursor = span.end;
        }
        out.push_str(&self.carry[cursor..cut]);
        report.redacted = report.total() > 0;
        self.carry.replace_range(..cut, "");
        (out, report)
    }
}

/// Drop a trailing token that could be the beginning of a PII value. Used only when a
/// stream ends without its terminal frame; a normal flush emits everything.
fn drop_partial_token(text: &str) -> String {
    let token_char = |c: char| {
        c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '+' | ':' | '/' | '=' | '-')
    };
    let start = text
        .rfind(|c: char| !token_char(c))
        .map_or(0, |index| index + c_len_at(text, index));
    let tail = &text[start..];
    let digits = tail.chars().filter(char::is_ascii_digit).count();
    let secret_prefix = SECRET_PREFIXES
        .iter()
        .any(|(prefix, _)| tail.starts_with(prefix));
    if tail.contains('@') || digits >= 8 || secret_prefix {
        text[..start].to_string()
    } else {
        text.to_string()
    }
}

/// Byte length of the character starting at `index`.
fn c_len_at(text: &str, index: usize) -> usize {
    text[index..].chars().next().map_or(1, char::len_utf8)
}

/// The largest character boundary at or below `index`.
fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Every candidate from every scanner.
fn candidates(text: &str) -> Vec<Candidate> {
    let mut all = Vec::new();
    all.extend(scan_emails(text));
    all.extend(scan_cards(text));
    all.extend(scan_phones(text));
    all.extend(scan_secrets(text));
    all
}

/// Earliest start wins; at the same start the longest match wins; exact ties go to the
/// higher-priority class. Anything overlapping an accepted span is dropped.
fn resolve(mut candidates: Vec<Candidate>) -> Vec<Candidate> {
    candidates.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then_with(|| b.end.cmp(&a.end))
            .then_with(|| priority(a.class).cmp(&priority(b.class)))
    });
    let mut accepted = Vec::new();
    let mut last_end = 0;
    for candidate in candidates {
        if candidate.end > candidate.start && candidate.start >= last_end {
            last_end = candidate.end;
            accepted.push(candidate);
        }
    }
    accepted
}

const fn priority(class: PiiClass) -> u8 {
    match class {
        PiiClass::Secret => 0,
        PiiClass::Card => 1,
        PiiClass::Email => 2,
        PiiClass::Phone => 3,
    }
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// A token may start here: the preceding byte is not part of an identifier.
fn starts_token(bytes: &[u8], index: usize) -> bool {
    index == 0 || !is_word_byte(bytes[index - 1])
}

/// A token may end here.
fn ends_token(bytes: &[u8], index: usize) -> bool {
    index >= bytes.len() || !is_word_byte(bytes[index])
}

// ----- email ----------------------------------------------------------------------

fn scan_emails(text: &str) -> Vec<Candidate> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'@' {
            continue;
        }
        let mut start = index;
        while start > 0 && is_email_local(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = index + 1;
        while end < bytes.len() && is_email_domain(bytes[end]) {
            end += 1;
        }
        if start == index || end == index + 1 {
            continue;
        }
        if valid_local(&text[start..index])
            && valid_domain(&text[index + 1..end])
            && ends_token(bytes, end)
        {
            found.push(Candidate {
                start,
                end,
                class: PiiClass::Email,
            });
        }
    }
    found
}

fn is_email_local(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn is_email_domain(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')
}

fn valid_local(local: &str) -> bool {
    !local.is_empty()
        && local.len() <= 64
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
}

fn valid_domain(domain: &str) -> bool {
    if domain.is_empty() || domain.len() > 255 {
        return false;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    for (index, label) in labels.iter().enumerate() {
        if label.is_empty() || label.len() > 63 {
            return false;
        }
        if label.starts_with('-') || label.ends_with('-') {
            return false;
        }
        if index + 1 == labels.len()
            && (label.len() < 2 || !label.bytes().all(|byte| byte.is_ascii_alphabetic()))
        {
            // An alphabetic TLD is the cheapest guard against matching `a@b` or
            // `user@host-01`.
            return false;
        }
    }
    true
}

// ----- numbers: shared grouping ---------------------------------------------------

/// Consecutive digit runs inside one number token, separated by spaces, dashes, dots, or
/// parentheses.
#[derive(Debug, Clone, Copy)]
struct Group {
    start: usize,
    end: usize,
    digits: usize,
}

impl Group {
    fn value(&self, text: &str) -> u32 {
        text[self.start..self.end]
            .bytes()
            .fold(0u32, |value, byte| {
                value
                    .saturating_mul(10)
                    .saturating_add(u32::from(byte - b'0'))
            })
    }
}

/// Most digit groups considered inside one number token (`+44 20 7946 0958` has four,
/// French national formatting has six).
const MAX_GROUPS: usize = 6;

/// Group the digit runs of the token that starts at `start` (`+`, `(`, or a digit).
///
/// The scanners need group boundaries, not one maximal digit run: a phone followed by
/// another number must be judged per group, or every later digit is swallowed into a
/// "number" too long to be either a phone or a card.
fn collect_groups(text: &str, start: usize) -> Vec<Group> {
    let bytes = text.as_bytes();
    let mut groups = Vec::new();
    let mut cursor = start;
    if matches!(bytes[cursor], b'+' | b'(') {
        cursor += 1;
    }
    loop {
        let group_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor == group_start {
            break;
        }
        groups.push(Group {
            start: group_start,
            end: cursor,
            digits: cursor - group_start,
        });
        if groups.len() >= MAX_GROUPS {
            break;
        }
        // A separator run continues the token only when another digit follows.
        let mut next = cursor;
        while next < bytes.len() && matches!(bytes[next], b' ' | b'-' | b'.' | b'(' | b')') {
            next += 1;
        }
        if next > cursor && next < bytes.len() && bytes[next].is_ascii_digit() {
            cursor = next;
        } else {
            break;
        }
    }
    groups
}

// ----- payment cards --------------------------------------------------------------

fn scan_cards(text: &str) -> Vec<Candidate> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_digit() && starts_token(bytes, index) {
            let groups = collect_groups(text, index);
            if let Some(candidate) = card_window(text, bytes, &groups) {
                found.push(candidate);
            }
            // Resume after the first group so a number glued on by spaces is still
            // examined; `collect_groups` bounds the work per token.
            index = groups
                .first()
                .map_or(index + 1, |group| group.end)
                .max(index + 1);
        } else {
            index += 1;
        }
    }
    found
}

/// The longest 13–19 digit prefix that passes Luhn.
fn card_window(text: &str, bytes: &[u8], groups: &[Group]) -> Option<Candidate> {
    let mut best = None;
    let mut digits = 0usize;
    for group in groups {
        digits += group.digits;
        if digits > 19 {
            break;
        }
        if (13..=19).contains(&digits)
            && ends_token(bytes, group.end)
            && valid_card(&text[groups[0].start..group.end])
        {
            best = Some(Candidate {
                start: groups[0].start,
                end: group.end,
                class: PiiClass::Card,
            });
        }
    }
    best
}

/// Luhn, plus a rejection of repeated digits: `0000 …` is checksum-valid but never a PAN.
fn valid_card(span: &str) -> bool {
    let digits: Vec<u8> = span
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|byte| byte - b'0')
        .collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    if digits.iter().all(|digit| *digit == digits[0]) {
        return false;
    }
    let mut sum = 0u32;
    for (position, digit) in digits.iter().rev().enumerate() {
        let mut value = u32::from(*digit);
        if position % 2 == 1 {
            value *= 2;
            if value > 9 {
                value -= 9;
            }
        }
        sum += value;
    }
    sum.is_multiple_of(10)
}

// ----- phone numbers --------------------------------------------------------------

fn scan_phones(text: &str) -> Vec<Candidate> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let starts =
            byte == b'+' || byte == b'(' || (byte.is_ascii_digit() && starts_token(bytes, index));
        if !starts {
            index += 1;
            continue;
        }
        let groups = collect_groups(text, index);
        if let Some(candidate) = phone_window(text, bytes, &groups, index, byte == b'+') {
            found.push(candidate);
        }
        index = groups
            .first()
            .map_or(index + 1, |group| group.end)
            .max(index + 1);
    }
    found
}

/// The longest 8–15 digit prefix that looks like a phone number.
fn phone_window(
    text: &str,
    bytes: &[u8],
    groups: &[Group],
    start: usize,
    has_plus: bool,
) -> Option<Candidate> {
    let first_is_year = groups
        .first()
        .is_some_and(|group| group.digits == 4 && (1900..=2099).contains(&group.value(text)));
    // `1234 5678 9012 3456` is a long number, not a phone with a trailing group. A
    // card-shaped run (4-4-…) is decided by the card scanner and its Luhn check alone, so
    // a benign 16-digit order number is left untouched instead of losing three groups to
    // `[phone]`.
    let card_shaped = groups.len() >= 3 && groups[0].digits == 4 && groups[1].digits == 4;
    if card_shaped {
        return None;
    }
    let mut best = None;
    let mut digits = 0usize;
    for (offset, group) in groups.iter().enumerate() {
        digits += group.digits;
        if digits > 15 {
            break;
        }
        if !(8..=15).contains(&digits) || !ends_token(bytes, group.end) {
            continue;
        }
        let plausible = if has_plus {
            // An international prefix is unambiguous.
            true
        } else {
            // Unprefixed needs real formatting (three groups) and must not be a
            // year-prefixed id such as `2024-000123456`.
            offset >= 2 && !first_is_year
        };
        if plausible {
            best = Some(Candidate {
                start,
                end: group.end,
                class: PiiClass::Phone,
            });
        }
    }
    best
}

// ----- secrets --------------------------------------------------------------------

/// Provider prefixes with the minimum number of key characters that must follow.
const SECRET_PREFIXES: &[(&str, usize)] = &[
    ("sk-rr-", 10),
    ("sk-", 16),
    ("ghp_", 20),
    ("gho_", 20),
    ("ghu_", 20),
    ("ghs_", 20),
    ("ghr_", 20),
    ("xoxb-", 10),
    ("xoxp-", 10),
    ("xoxa-", 10),
    ("xoxr-", 10),
    ("xoxs-", 10),
    ("AKIA", 16),
    ("AIza", 35),
];

fn scan_secrets(text: &str) -> Vec<Candidate> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if !starts_token(bytes, index) {
            index += 1;
            continue;
        }
        if let Some(end) = match_auth(bytes, index).or_else(|| match_jwt(bytes, index)) {
            found.push(Candidate {
                start: index,
                end,
                class: PiiClass::Secret,
            });
            index = end;
            continue;
        }
        let mut matched = 0;
        for (prefix, minimum) in SECRET_PREFIXES {
            if bytes[index..].starts_with(prefix.as_bytes()) {
                let mut end = index + prefix.len();
                while end < bytes.len() && is_key_byte(bytes[end]) {
                    end += 1;
                }
                if end - (index + prefix.len()) >= *minimum {
                    matched = end;
                    break;
                }
            }
        }
        if matched > 0 {
            found.push(Candidate {
                start: index,
                end: matched,
                class: PiiClass::Secret,
            });
            index = matched;
            continue;
        }
        index += 1;
    }
    found
}

fn is_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn is_b64url(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

/// `Bearer <value>` / `Basic <value>`: the keyword and the value are replaced together so
/// a log line cannot keep the credential while losing the word that identified it.
fn match_auth(bytes: &[u8], start: usize) -> Option<usize> {
    let rest = &bytes[start..];
    let keyword = if rest.len() >= 6 && rest[..6].eq_ignore_ascii_case(b"bearer") {
        6
    } else if rest.len() >= 5 && rest[..5].eq_ignore_ascii_case(b"basic") {
        5
    } else {
        return None;
    };
    let mut index = start + keyword;
    if index >= bytes.len() || !bytes[index].is_ascii_whitespace() {
        return None;
    }
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    let value_start = index;
    while index < bytes.len() && is_auth_value_byte(bytes[index]) {
        index += 1;
    }
    (index - value_start >= 8).then_some(index)
}

fn is_auth_value_byte(byte: u8) -> bool {
    !byte.is_ascii_whitespace()
        && !matches!(
            byte,
            b'"' | b'\'' | b',' | b';' | b')' | b']' | b'}' | b'<' | b'>'
        )
}

/// A JWT triple (`eyJ….<payload>.<signature>`) is distinctive enough to redact on its own.
fn match_jwt(bytes: &[u8], start: usize) -> Option<usize> {
    if !bytes[start..].starts_with(b"eyJ") {
        return None;
    }
    let mut index = start;
    for segment in 0..3 {
        let begin = index;
        while index < bytes.len() && is_b64url(bytes[index]) {
            index += 1;
        }
        if index - begin < 8 {
            return None;
        }
        if segment < 2 {
            if index >= bytes.len() || bytes[index] != b'.' {
                return None;
            }
            index += 1;
        }
    }
    ends_token(bytes, index).then_some(index)
}

/// Redact the text a model will read from a canonical request.
///
/// Covers the system prompt, message text and reasoning, and tool-result content (the
/// place a pasted credential usually arrives). Tool schemas and tool-call arguments are
/// protocol payloads, not prose: rewriting them would change what a tool does, so they are
/// deliberately untouched.
pub fn redact_chat_request(request: &mut ChatRequest) -> RedactionReport {
    let mut report = RedactionReport::default();
    if let Some(system) = request.system.as_mut() {
        redact_in_place(system, &mut report);
    }
    for message in &mut request.messages {
        for part in &mut message.parts {
            match part {
                Part::Text(text) | Part::Reasoning { text, .. } => {
                    redact_in_place(text, &mut report)
                }
                Part::ToolResult { content, .. } => redact_in_place(content, &mut report),
                _ => {}
            }
        }
    }
    report
}

/// Redact the text a client would read from a canonical response.
///
/// Text and reasoning only. Tool calls are structured protocol payloads — a redacted
/// argument would break the tool call — and image/opaque parts carry no prose.
pub fn redact_chat_response(response: &mut ChatResponse) -> RedactionReport {
    let mut report = RedactionReport::default();
    for part in &mut response.parts {
        if let Part::Text(text) | Part::Reasoning { text, .. } = part {
            redact_in_place(text, &mut report);
        }
    }
    report
}

/// Redact the text-bearing fields of a client request body, in place.
///
/// This runs on the raw wire body so same-dialect forwarding (which sends the client's
/// body, not the canonical one) is covered too. Only the fields the model reads are
/// touched — prompt/system/content text — while ids, model names, images, tool schemas and
/// tool-call arguments stay byte-identical.
pub fn redact_json_body(body: &mut Value) -> RedactionReport {
    let mut report = RedactionReport::default();
    let Value::Object(map) = body else {
        return report;
    };
    for (key, value) in map.iter_mut() {
        match key.as_str() {
            // Top-level prompt containers across the three chat dialects.
            "messages" | "system" | "input" | "instructions" | "prompt" => {
                redact_json_text(value, &mut report);
            }
            _ => {}
        }
    }
    report
}

/// Recurse through the prompt shape, redacting text leaves only.
fn redact_json_text(value: &mut Value, report: &mut RedactionReport) {
    match value {
        Value::String(text) => redact_in_place(text, report),
        Value::Array(items) => {
            for item in items {
                redact_json_text(item, report);
            }
        }
        Value::Object(map) => {
            // Inside a message/part, only `content` and `text` carry prose. Nested
            // objects of other names are protocol payloads (tool schemas, tool inputs).
            for (key, child) in map.iter_mut() {
                if matches!(key.as_str(), "content" | "text") {
                    redact_json_text(child, report);
                }
            }
        }
        _ => {}
    }
}

fn redact_in_place(text: &mut String, report: &mut RedactionReport) {
    let (output, found) = redact(text);
    if found.redacted {
        *text = output.into_owned();
        *report += found;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rrouter_core::{FinishReason, Message, Role, Usage};
    use serde_json::json;

    /// Redact and hand back an owned string, for readable assertions.
    fn clean(text: &str) -> (String, RedactionReport) {
        let (output, report) = redact(text);
        (output.into_owned(), report)
    }

    #[test]
    fn class_placeholders_and_labels_are_stable() {
        assert_eq!(PiiClass::Email.placeholder(), "[email]");
        assert_eq!(PiiClass::Phone.placeholder(), "[phone]");
        assert_eq!(PiiClass::Card.placeholder(), "[card]");
        assert_eq!(PiiClass::Secret.placeholder(), "[secret]");
        for (index, class) in PiiClass::ALL.into_iter().enumerate() {
            assert_eq!(class.index(), index, "{class:?}");
        }
        assert_eq!(PiiClass::Secret.as_str(), "secret");
    }

    #[test]
    fn emails_are_redacted_and_surrounding_text_is_kept() {
        let text = "write to a.b+tag@example.co.uk or <ops@rrouter.dev>.";
        let (output, report) = clean(text);

        assert_eq!(output, "write to [email] or <[email]>.");
        assert_eq!(report.count(PiiClass::Email), 2);
        assert_eq!(report.total(), 2);
        assert!(report.redacted);
    }

    #[test]
    fn non_emails_are_left_alone() {
        for text in [
            "a@b",
            "@example.com",
            "user@host-01",
            "user@example",
            "a@@b.co",
        ] {
            let (output, report) = clean(text);
            assert_eq!(output, text, "{text}");
            assert!(report.is_empty(), "{text}: {report}");
        }
    }

    #[test]
    fn phone_numbers_are_redacted_when_formatted() {
        let text = "call +1 (415) 555-2671, 415-555-2671 or +44 20 7946 0958.";
        let (output, report) = clean(text);

        assert_eq!(output, "call [phone], [phone] or [phone].");
        assert_eq!(report.count(PiiClass::Phone), 3);
        assert!(report.count(PiiClass::Card) == 0);
    }

    #[test]
    fn bare_digit_runs_are_not_phone_numbers() {
        // Deliberate policy: ten digits and no formatting is as likely a timestamp or an
        // order id, so the scanner stays out of it.
        for text in ["timestamp 1700000000", "id 4155552671", "ms 1234567890"] {
            let (output, report) = clean(text);
            assert_eq!(output, text, "{text}");
            assert!(report.is_empty(), "{text}: {report}");
        }
    }

    #[test]
    fn luhn_valid_cards_are_redacted() {
        for card in [
            "4111 1111 1111 1111",
            "5500 0000 0000 0004",
            "3782 822463 10005",
            "6011-0009-9013-9424",
        ] {
            let text = format!("paid with {card} today");
            let (output, report) = clean(&text);
            assert_eq!(output, "paid with [card] today", "{card}");
            assert_eq!(report.count(PiiClass::Card), 1, "{card}");
            assert_eq!(
                report.count(PiiClass::Phone),
                0,
                "{card} must not also be a phone"
            );
        }
    }

    #[test]
    fn long_numbers_without_a_valid_luhn_are_kept() {
        for text in [
            "order 1234 5678 9012 3456",
            "ref 1234567890123456",
            "zeros 0000 0000 0000 0000",
            "invoice 4111 1111 1111 1112",
        ] {
            let (output, report) = clean(text);
            assert_eq!(output, text, "{text}");
            assert!(report.is_empty(), "{text}: {report}");
        }
    }

    #[test]
    fn a_random_order_id_is_not_redacted() {
        for text in [
            "ORD-2024-000123456",
            "order ORD-2024-000123456 is late",
            "INV-88213-4901",
            "SKU 998877665544332212",
        ] {
            let (output, report) = clean(text);
            assert_eq!(output, text, "{text}");
            assert!(report.is_empty(), "{text}: {report}");
        }
    }

    #[test]
    fn bearer_and_provider_keys_are_redacted() {
        for (input, expected) in [
            (
                "Authorization: Bearer abcdefghijklmnop",
                "Authorization: [secret]",
            ),
            ("header BEARER abcdefghijklmnop", "header [secret]"),
            ("proxy Basic dXNlcjpwYXNzd29yZA==", "proxy [secret]"),
            ("token=sk-rr-0123456789abcdef", "token=[secret]"),
            ("openai sk-abcdefghijklmnopqrstuvwxyz", "openai [secret]"),
            ("github ghp_0123456789abcdefghij", "github [secret]"),
            ("aws AKIAIOSFODNN7EXAMPLE here", "aws [secret] here"),
            ("slack xoxb-123456789012-abcdefghij", "slack [secret]"),
            (
                "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.c2lnbmF0dXJl",
                "jwt [secret]",
            ),
        ] {
            let (output, report) = clean(input);
            assert_eq!(output, expected, "{input}");
            assert_eq!(report.count(PiiClass::Secret), 1, "{input}");
        }
    }

    #[test]
    fn short_or_ordinary_words_are_not_secrets() {
        for text in [
            "Bearer of good news",
            "basic usage",
            "sk-learn is a library",
            "the ghp_ prefix",
        ] {
            let (output, report) = clean(text);
            assert_eq!(output, text, "{text}");
            assert!(report.is_empty(), "{text}: {report}");
        }
    }

    #[test]
    fn mixed_text_redacts_each_class_and_reports_counts() {
        let text = "jane.doe@example.com paid 4111 1111 1111 1111, \
                    rang +1 415 555 2671, key sk-rr-abcdefghij0123456789.";
        let (output, report) = clean(text);

        assert_eq!(output, "[email] paid [card], rang [phone], key [secret].");
        assert_eq!(report.count(PiiClass::Email), 1);
        assert_eq!(report.count(PiiClass::Card), 1);
        assert_eq!(report.count(PiiClass::Phone), 1);
        assert_eq!(report.count(PiiClass::Secret), 1);
        assert_eq!(report.total(), 4);
        assert_eq!(
            report.to_string(),
            "redacted 4 email=1 phone=1 card=1 secret=1"
        );
    }

    #[test]
    fn overlapping_matches_resolve_to_the_longest_at_the_same_start() {
        // The email starts at the same byte as the card inside it and is longer, so the
        // email wins and no digits survive to become a second candidate.
        let (output, report) = clean("mail 4111111111111111@example.com now");
        assert_eq!(output, "mail [email] now");
        assert_eq!(report.count(PiiClass::Email), 1);
        assert_eq!(report.count(PiiClass::Card), 0);
        // Deterministic: the same input gives the same answer.
        assert_eq!(clean("mail 4111111111111111@example.com now").0, output);
    }

    #[test]
    fn utf8_text_around_matches_is_preserved() {
        let text = "café: ja.ne@example.com — ☎ +33 1 23 45 67 89 🔐 naïve";
        let (output, report) = clean(text);

        assert_eq!(output, "café: [email] — ☎ [phone] 🔐 naïve");
        assert_eq!(report.count(PiiClass::Email), 1);
        assert_eq!(report.count(PiiClass::Phone), 1);
    }

    #[test]
    fn empty_and_plain_text_are_returned_borrowed() {
        let (output, report) = redact("");
        assert!(matches!(output, Cow::Borrowed("")));
        assert!(report.is_empty());
        assert!(!report.redacted);

        let (output, report) = redact("nothing sensitive here");
        assert!(matches!(output, Cow::Borrowed("nothing sensitive here")));
        assert!(report.is_empty());
    }

    #[test]
    fn redaction_is_idempotent() {
        let text = "a.b@example.com, +1 415 555 2671, 4111 1111 1111 1111, sk-rr-0123456789abcdef";
        let (once, report) = clean(text);
        assert_eq!(report.total(), 4);

        let (twice, second) = clean(&once);
        assert_eq!(twice, once, "placeholders must not match again");
        assert!(second.is_empty(), "{second}");
        assert!(!second.redacted);
    }

    #[test]
    fn the_report_never_carries_the_removed_values() {
        let secret = "sk-rr-supersecret0123456789";
        let (_, report) = clean(&format!("key {secret} and a.b@example.com"));

        let debug = format!("{report:?}");
        let display = report.to_string();
        for rendered in [&debug, &display] {
            assert!(!rendered.contains(secret), "{rendered}");
            assert!(!rendered.contains("a.b@example.com"), "{rendered}");
        }
        assert!(display.contains("email=1"), "{display}");
        assert!(display.contains("secret=1"), "{display}");
    }

    #[test]
    fn reports_can_be_accumulated_across_chunks() {
        let mut total = RedactionReport::default();
        total += clean("mail a@b.co").1;
        total += clean("card 4111 1111 1111 1111").1;
        total += clean("nothing here").1;

        assert_eq!(total.count(PiiClass::Email), 1);
        assert_eq!(total.count(PiiClass::Card), 1);
        assert_eq!(total.total(), 2);
        assert!(total.redacted);
    }

    // ----- canonical request / response / wire body --------------------------------

    #[test]
    fn a_canonical_request_is_redacted_but_ids_and_tool_arguments_are_not() {
        let mut request = ChatRequest::new("gpt");
        request.system = Some("contact a@b.co".into());
        request
            .messages
            .push(Message::text(Role::User, "card 4111 1111 1111 1111"));
        request.messages.push(Message {
            role: Role::Tool,
            parts: vec![Part::ToolResult {
                tool_use_id: "call_1".into(),
                content: "key sk-rr-0123456789abcdef".into(),
                is_error: false,
            }],
        });
        request.messages.push(Message {
            role: Role::Assistant,
            parts: vec![Part::ToolUse {
                id: "call_2".into(),
                name: "lookup".into(),
                input: json!({"text": "a@b.co"}),
            }],
        });

        let report = redact_chat_request(&mut request);

        assert_eq!(request.system.as_deref(), Some("contact [email]"));
        assert!(request.messages[0].joined_text().contains("[card]"));
        match &request.messages[1].parts[0] {
            Part::ToolResult {
                content,
                tool_use_id,
                ..
            } => {
                assert_eq!(content, "key [secret]");
                assert_eq!(tool_use_id, "call_1", "ids are not prose");
            }
            other => panic!("unexpected part {other:?}"),
        }
        match &request.messages[2].parts[0] {
            Part::ToolUse { input, id, .. } => {
                assert_eq!(input["text"], "a@b.co", "tool arguments stay intact");
                assert_eq!(id, "call_2");
            }
            other => panic!("unexpected part {other:?}"),
        }
        assert_eq!(report.count(PiiClass::Email), 1);
        assert_eq!(report.count(PiiClass::Card), 1);
        assert_eq!(report.count(PiiClass::Secret), 1);
    }

    #[test]
    fn a_canonical_response_is_redacted_but_tool_calls_are_not() {
        let mut response = ChatResponse {
            id: "resp".into(),
            model: "m".into(),
            parts: vec![
                Part::Text("write to a@b.co".into()),
                Part::Reasoning {
                    text: "card 4111 1111 1111 1111".into(),
                    signature: None,
                },
                Part::ToolUse {
                    id: "call_1".into(),
                    name: "send".into(),
                    input: json!({"to": "a@b.co"}),
                },
            ],
            finish_reason: FinishReason::Stop,
            usage: Usage::default(),
        };

        let report = redact_chat_response(&mut response);

        assert_eq!(response.parts[0], Part::Text("write to [email]".into()));
        match &response.parts[1] {
            Part::Reasoning { text, .. } => assert_eq!(text, "card [card]"),
            other => panic!("unexpected part {other:?}"),
        }
        match &response.parts[2] {
            Part::ToolUse { input, .. } => assert_eq!(input["to"], "a@b.co"),
            other => panic!("unexpected part {other:?}"),
        }
        assert_eq!(report.count(PiiClass::Email), 1);
        assert_eq!(report.count(PiiClass::Card), 1);
    }

    #[test]
    fn the_wire_body_is_redacted_only_where_the_model_reads() {
        let mut body = json!({
            "model": "not-an-email@example.com",
            "system": [{"type": "text", "text": "mail a@b.co"}],
            "messages": [
                {"role": "user", "content": "card 4111 1111 1111 1111"},
                {"role": "user", "content": [
                    {"type": "text", "text": "key sk-rr-0123456789abcdef"},
                    {"type": "image_url", "image_url": {"url": "https://x/a.png"}}
                ]},
                {"role": "tool", "content": "mail a@b.co"},
                {"role": "assistant", "tool_calls": [
                    {"function": {"arguments": "{\"to\":\"a@b.co\"}"}}
                ]}
            ],
            "tools": [
                {"type": "function", "function": {
                    "name": "send", "description": "mail a@b.co", "parameters": {}
                }}
            ]
        });

        let report = redact_json_body(&mut body);
        assert_eq!(report.count(PiiClass::Email), 2, "{report}");
        assert_eq!(report.count(PiiClass::Card), 1, "{report}");
        assert_eq!(report.count(PiiClass::Secret), 1, "{report}");

        assert_eq!(
            body["model"], "not-an-email@example.com",
            "a model id is not prose"
        );
        assert!(body["system"][0]["text"]
            .as_str()
            .expect("text")
            .contains("[email]"));
        assert!(body["messages"][0]["content"]
            .as_str()
            .expect("content")
            .contains("[card]"));
        assert!(body["messages"][1]["content"][0]["text"]
            .as_str()
            .expect("text")
            .contains("[secret]"));
        assert_eq!(
            body["messages"][1]["content"][1]["image_url"]["url"],
            "https://x/a.png"
        );
        assert!(body["messages"][2]["content"]
            .as_str()
            .expect("content")
            .contains("[email]"));
        assert!(
            body["messages"][3]["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .expect("arguments")
                .contains("a@b.co"),
            "tool arguments are protocol payloads"
        );
        assert!(body["tools"][0]["function"]["description"]
            .as_str()
            .expect("description")
            .contains("a@b.co"));

        // Idempotent: a second pass finds nothing.
        let second = redact_json_body(&mut body);
        assert!(second.is_empty(), "{second}");
    }

    // ----- streaming carry window -------------------------------------------------

    #[test]
    fn a_short_stream_is_held_then_redacted_at_finish() {
        let mut redactor = StreamRedactor::new();
        let (held, report) = redactor.push("mail alice@example.com");
        assert!(held.is_empty(), "a short buffer is not emitted early");
        assert!(!report.redacted);
        let (tail, report) = redactor.finish();
        assert_eq!(tail, "mail [email]");
        assert_eq!(report.count(PiiClass::Email), 1);
        assert!(redactor.finish().0.is_empty(), "finish is idempotent");
    }

    #[test]
    fn an_email_split_across_deltas_in_a_long_stream_never_leaks() {
        let mut redactor = StreamRedactor::new();
        let prefix = "x".repeat(STREAM_CARRY_BYTES + 100);
        let mut seen = redactor.push(&prefix).0;
        assert_eq!(seen.len(), 100, "only the part beyond the carry is emitted");
        seen.push_str(&redactor.push("mail alice@exam").0);
        seen.push_str(&redactor.push("ple.com done").0);
        seen.push_str(&redactor.finish().0);
        assert!(seen.contains("[email]"), "{seen}");
        assert!(!seen.contains("alice@exam"), "no prefix leak: {seen}");
        assert!(!seen.contains("ple.com"), "no suffix leak: {seen}");
    }

    #[test]
    fn a_card_and_a_secret_split_across_deltas_are_redacted() {
        let mut redactor = StreamRedactor::new();
        let prefix = "y".repeat(STREAM_CARRY_BYTES + 64);
        let mut seen = redactor.push(&prefix).0;
        seen.push_str(&redactor.push("card 4111 1111").0);
        seen.push_str(&redactor.push(" 1111 1111 key sk-rr-0123456789").0);
        seen.push_str(&redactor.push("abcdef").0);
        seen.push_str(&redactor.finish().0);
        assert!(seen.contains("[card]"), "{seen}");
        assert!(seen.contains("[secret]"), "{seen}");
        assert!(!seen.contains("4111 1111"), "{seen}");
        assert!(!seen.contains("sk-rr-0123456789"), "{seen}");
    }

    #[test]
    fn placeholders_split_across_deltas_are_not_redacted_twice() {
        let mut redactor = StreamRedactor::new();
        let prefix = "z".repeat(STREAM_CARRY_BYTES);
        let mut seen = redactor.push(&prefix).0;
        seen.push_str(&redactor.push("[ema").0);
        seen.push_str(&redactor.push("il] and [card]").0);
        seen.push_str(&redactor.finish().0);
        assert!(seen.contains("[email] and [card]"), "{seen}");
        assert!(!seen.contains("[[email]]"), "{seen}");
    }

    #[test]
    fn an_interrupted_stream_drops_an_unterminated_pii_tail() {
        let mut redactor = StreamRedactor::new();
        let prefix = "w".repeat(STREAM_CARRY_BYTES + 20);
        let mut seen = redactor.push(&prefix).0;
        seen.push_str(&redactor.push("mail alice@exam").0);
        let (tail, _) = redactor.finish_interrupted();
        seen.push_str(&tail);
        assert!(
            !seen.contains("alice@"),
            "the unterminated address is dropped: {seen}"
        );
        assert!(
            seen.ends_with("mail "),
            "text before the token survives: {seen}"
        );

        // A secret prefix split at the end is dropped the same way.
        let mut redactor = StreamRedactor::new();
        let _ = redactor.push("key sk-rr-0123");
        assert_eq!(redactor.finish_interrupted().0, "key ");
    }
}
