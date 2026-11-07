//! Incremental SSE framing, shared by every upstream reader and downstream writer.
//!
//! Upstreams emit either OpenAI-style `data: {json}\n\n` frames or Anthropic-style
//! `event: name\ndata: {json}\n\n` frames. The decoder is byte-oriented and handles
//! frames split across arbitrary chunk boundaries — the failure mode that desyncs
//! line-buffered Node parsers under 100k+ token payloads.

use bytes::{Buf, BytesMut};

/// One decoded SSE frame.
#[derive(Debug, Clone, PartialEq)]
pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}

impl SseFrame {
    pub fn is_done(&self) -> bool {
        self.data.trim() == "[DONE]"
    }
}

/// Byte-level SSE decoder. Feed raw chunks; drain frames.
#[derive(Debug, Default)]
pub struct SseCodec {
    buf: BytesMut,
}

impl SseCodec {
    pub fn new() -> Self {
        Self {
            buf: BytesMut::with_capacity(8 * 1024),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }

    /// Pull the next complete frame. Returns `None` when more bytes are needed.
    pub fn next_frame(&mut self) -> Option<SseFrame> {
        loop {
            let end = find_frame_end(&self.buf)?;
            let raw = self.buf.split_to(end);
            // Drop the blank-line separator that terminated the frame.
            let sep = frame_separator_len(&self.buf);
            self.buf.advance(sep);
            if let Some(frame) = parse_frame(&raw) {
                return Some(frame);
            }
            // Pure-comment or empty frame (e.g. Anthropic `ping`): keep scanning.
        }
    }

    /// Flush a trailing frame that never got its blank-line terminator
    /// (some upstreams close the connection right after the last `data:`).
    pub fn finish(&mut self) -> Option<SseFrame> {
        if self.buf.is_empty() {
            return None;
        }
        let raw = std::mem::take(&mut self.buf);
        parse_frame(&raw)
    }
}

fn frame_separator_len(buf: &[u8]) -> usize {
    match buf {
        [b'\r', b'\n', b'\r', b'\n', ..] => 4,
        [b'\r', b'\n', ..] => 2,
        [b'\n', b'\n', ..] => 2,
        _ => 0,
    }
}

fn find_frame_end(buf: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < buf.len() {
        match (&buf[i..=i + 1], i + 3 < buf.len()) {
            (b"\n\n", _) => return Some(i),
            (b"\r\n", true) if &buf[i + 2..i + 4] == b"\r\n" => return Some(i),
            _ => i += 1,
        }
    }
    None
}

fn parse_frame(raw: &[u8]) -> Option<SseFrame> {
    let text = String::from_utf8_lossy(raw);
    let mut event = None;
    let mut data = String::new();
    let mut has_data = false;
    for line in text
        .split_inclusive(['\n'])
        .map(|l| l.trim_end_matches(['\r', '\n']))
    {
        if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim_start().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            if has_data {
                data.push('\n');
            }
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            has_data = true;
        }
    }
    has_data.then_some(SseFrame { event, data })
}

/// Render one frame in OpenAI wire shape.
pub fn render_openai(data: &str, event: Option<&str>) -> String {
    let mut s = String::with_capacity(data.len() + 16);
    if let Some(ev) = event {
        s.push_str("event: ");
        s.push_str(ev);
        s.push('\n');
    }
    s.push_str("data: ");
    s.push_str(data);
    s.push_str("\n\n");
    s
}

/// Render one frame in Anthropic wire shape (`event:` always present).
pub fn render_anthropic(event: &str, data: &str) -> String {
    let mut s = String::with_capacity(data.len() + event.len() + 24);
    s.push_str("event: ");
    s.push_str(event);
    s.push_str("\ndata: ");
    s.push_str(data);
    s.push_str("\n\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_frames_split_across_chunks() {
        let mut codec = SseCodec::new();
        codec.push(b"data: {\"a\":");
        assert!(codec.next_frame().is_none());
        codec.push(b"1}\n\n");
        let f = codec.next_frame().unwrap();
        assert_eq!(f.data, "{\"a\":1}");
        assert!(f.event.is_none());
    }

    #[test]
    fn handles_crlf_and_event_names() {
        let mut codec = SseCodec::new();
        codec.push(b"event: content_block_delta\r\ndata: {\"x\":2}\r\n\r\n");
        let f = codec.next_frame().unwrap();
        assert_eq!(f.event.as_deref(), Some("content_block_delta"));
        assert_eq!(f.data, "{\"x\":2}");
    }

    #[test]
    fn multi_line_data_joined_with_newline() {
        let mut codec = SseCodec::new();
        codec.push(b"data: line1\ndata: line2\n\n");
        assert_eq!(codec.next_frame().unwrap().data, "line1\nline2");
    }

    #[test]
    fn skips_ping_frames_without_data() {
        let mut codec = SseCodec::new();
        codec.push(b"event: ping\n\ndata: [DONE]\n\n");
        let f = codec.next_frame().unwrap();
        assert!(f.is_done());
    }

    #[test]
    fn finish_flushes_unterminated_tail() {
        let mut codec = SseCodec::new();
        codec.push(b"data: {\"tail\":true}");
        assert!(codec.next_frame().is_none());
        assert_eq!(codec.finish().unwrap().data, "{\"tail\":true}");
    }
}
