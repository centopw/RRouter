//! Opt-in platform log capture for the dashboard.
//!
//! The console log is always written; this ring buffer is separate and disabled by
//! default. While enabled it keeps the last [`CAPACITY`] tracing events in memory so an
//! operator can debug the gateway without reading the terminal. Nothing is persisted and
//! prompt/response bodies never reach these events because the proxy does not log them.

use std::collections::VecDeque;
use std::fmt::Debug;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// Events kept before the oldest is dropped.
pub const CAPACITY: usize = 2000;

static ENABLED: AtomicBool = AtomicBool::new(false);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
static RING: LazyLock<Mutex<VecDeque<Entry>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(CAPACITY)));

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub id: u64,
    pub ts: i64,
    pub level: String,
    pub target: String,
    pub message: String,
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn clear() {
    let mut buffer = RING.lock().unwrap_or_else(|e| e.into_inner());
    buffer.clear();
}

pub fn push(level: &str, target: &str, message: String) {
    if !enabled() {
        return;
    }
    let id = SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0);
    let mut buffer = RING.lock().unwrap_or_else(|e| e.into_inner());
    if buffer.len() == CAPACITY {
        buffer.pop_front();
    }
    buffer.push_back(Entry {
        id,
        ts,
        level: level.to_string(),
        target: target.to_string(),
        message,
    });
}

/// Entries with `id > after`, oldest first, capped at `limit`.
pub fn snapshot(after: u64, limit: usize) -> Vec<Entry> {
    let buffer = RING.lock().unwrap_or_else(|e| e.into_inner());
    let limit = limit.clamp(1, CAPACITY);
    let mut out: Vec<Entry> = buffer.iter().filter(|e| e.id > after).cloned().collect();
    if out.len() > limit {
        out.drain(..out.len() - limit);
    }
    out
}

/// Tracing layer that feeds [`push`]. Installed always; cheap no-op while disabled.
pub struct CaptureLayer;

impl<S: Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        if !enabled() {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let meta = event.metadata();
        push(meta.level().as_str(), meta.target(), fields.finish());
    }
}

#[derive(Default)]
struct Fields {
    message: String,
    rest: Vec<String>,
}

impl Fields {
    fn finish(self) -> String {
        if self.rest.is_empty() {
            self.message
        } else if self.message.is_empty() {
            self.rest.join(" ")
        } else {
            format!("{} {}", self.message, self.rest.join(" "))
        }
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.rest.push(format!("{}={value}", field.name()));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.rest.push(format!("{}={value:?}", field.name()));
        }
    }
}
