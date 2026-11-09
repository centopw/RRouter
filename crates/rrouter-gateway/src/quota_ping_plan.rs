//! Pure scheduling math for subscription quota auto-ping.
//!
//! Time-of-day slots are wall-clock facts in the connection's IANA zone, so this module
//! uses `jiff` for civil dates and DST disambiguation. Smart phasing is deliberately
//! epoch-based: an offset inside a rolling window must survive a process restart and does
//! not depend on a calendar.

use std::collections::HashMap;

use jiff::tz::TimeZone;
use jiff::Timestamp;
use uuid::Uuid;

/// How long after a schedule slot a ping is still allowed to be sent.
pub const SLOT_GRACE_SECS: i64 = 30 * 60;
/// A reset ping waits five seconds so the provider has actually rolled the window.
pub const RESET_DELAY_SECS: i64 = 5;
/// Smart phase ticks can lag a minute; this keeps a missed exact phase recoverable.
pub const SMART_GRACE_SECS: i64 = 5 * 60;

#[derive(Debug, Clone, PartialEq)]
pub struct Schedule {
    pub anchor: String,
    pub timezone: String,
    pub window_hours: f64,
    pub slots_per_day: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// Unix seconds of the slot start.
    pub at: i64,
    /// Stable identity of this slot, stored as `last_ping_slot`.
    pub key: String,
}

/// One smart account's inputs.
#[derive(Debug, Clone, PartialEq)]
pub struct SmartAccount {
    pub connection_id: Uuid,
    pub provider: String,
    pub recent_requests: u64,
    pub reset_at: Option<i64>,
    pub used_percent: f64,
    pub window_hours: f64,
}

/// What the API and scheduler need to know about one smart account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmartPlan {
    pub phase_offset_secs: i64,
    pub next_ping_at: Option<i64>,
    /// Set when this tick should send (the scheduler still checks `last_ping_slot`).
    pub due_key: Option<String>,
    pub idle: bool,
}

/// Is `name` a zone this build can resolve?
pub fn known_timezone(name: &str) -> bool {
    TimeZone::get(name.trim()).is_ok()
}

/// Next wall-clock slot strictly after `now`.
pub fn next_slot(schedule: &Schedule, now: i64) -> Option<Slot> {
    let timezone = TimeZone::get(schedule.timezone.trim()).ok()?;
    let anchor = anchor_seconds(&schedule.anchor)?;
    let window = window_seconds(schedule.window_hours)?;
    let now_z = Timestamp::from_second(now).ok()?.to_zoned(timezone.clone());
    let mut date = now_z.date();
    let mut best: Option<Slot> = None;
    for _ in 0..4 {
        for index in 0..schedule.slots_per_day {
            if let Some(slot) = slot_on(date, anchor, window, index, &timezone) {
                if slot.at > now && best.as_ref().is_none_or(|seen| slot.at < seen.at) {
                    best = Some(slot);
                }
            }
        }
        date = date.tomorrow().ok()?;
    }
    best
}

/// The most recent slot at or before `now`, if it is still inside the grace window.
pub fn due_slot(schedule: &Schedule, now: i64, grace: i64) -> Option<Slot> {
    let timezone = TimeZone::get(schedule.timezone.trim()).ok()?;
    let anchor = anchor_seconds(&schedule.anchor)?;
    let window = window_seconds(schedule.window_hours)?;
    let now_z = Timestamp::from_second(now).ok()?.to_zoned(timezone.clone());
    let today = now_z.date();
    let yesterday = today.yesterday().ok()?;
    let mut best: Option<Slot> = None;
    for date in [yesterday, today] {
        for index in 0..schedule.slots_per_day {
            if let Some(slot) = slot_on(date, anchor, window, index, &timezone) {
                if slot.at <= now
                    && now - slot.at <= grace
                    && best.as_ref().is_none_or(|seen| slot.at > seen.at)
                {
                    best = Some(slot);
                }
            }
        }
    }
    best
}

/// Phase offset for account `index` of `count`, measured inside one window.
pub fn phase_offset(window_hours: f64, index: usize, count: usize) -> i64 {
    if count == 0 {
        return 0;
    }
    let window = (window_hours * 3600.0).round().max(1.0);
    ((index as f64 * window / count as f64).round()) as i64
}

/// Smallest epoch at or after `now` congruent to `offset` modulo `window`.
pub fn next_phase(now: i64, offset: i64, window: i64) -> i64 {
    let window = window.max(1);
    let remainder = (now - offset).rem_euclid(window);
    if remainder == 0 {
        now
    } else {
        now + (window - remainder)
    }
}

/// Latest epoch at or before `now` congruent to `offset` modulo `window`.
pub fn previous_phase(now: i64, offset: i64, window: i64) -> i64 {
    let window = window.max(1);
    now - (now - offset).rem_euclid(window)
}

/// Plan every smart account. Accounts of one provider are ordered by recent traffic
/// (highest first) before phases are assigned; idle accounts get no next ping.
pub fn plan_smart(accounts: &[SmartAccount], now: i64) -> HashMap<Uuid, SmartPlan> {
    let mut by_connection = HashMap::with_capacity(accounts.len());
    let mut providers: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, account) in accounts.iter().enumerate() {
        providers
            .entry(account.provider.as_str())
            .or_default()
            .push(index);
    }
    for indices in providers.values_mut() {
        indices.sort_by(|a, b| {
            let a = &accounts[*a];
            let b = &accounts[*b];
            b.recent_requests
                .cmp(&a.recent_requests)
                .then_with(|| a.connection_id.cmp(&b.connection_id))
        });
        let count = indices.len();
        for (position, index) in indices.iter().enumerate() {
            let account = &accounts[*index];
            let offset = phase_offset(account.window_hours, position, count);
            let idle = account.recent_requests == 0 && account.used_percent <= 0.0;
            let plan = if idle {
                SmartPlan {
                    phase_offset_secs: offset,
                    next_ping_at: None,
                    due_key: None,
                    idle: true,
                }
            } else if let Some(reset_at) = account.reset_at {
                let due_at = reset_at + offset.max(0);
                SmartPlan {
                    phase_offset_secs: offset,
                    next_ping_at: Some(due_at),
                    due_key: (now >= due_at).then(|| format!("smart-reset:{reset_at}")),
                    idle: false,
                }
            } else {
                let window = (account.window_hours * 3600.0).round().max(1.0) as i64;
                let next = next_phase(now, offset, window);
                let previous = previous_phase(now, offset, window);
                SmartPlan {
                    phase_offset_secs: offset,
                    next_ping_at: Some(next),
                    due_key: (now - previous <= SMART_GRACE_SECS)
                        .then(|| format!("smart-phase:{previous}")),
                    idle: false,
                }
            };
            by_connection.insert(account.connection_id, plan);
        }
    }
    by_connection
}

/// Reset ping time: five seconds after the provider's reset.
pub fn reset_due_at(reset_at: i64) -> i64 {
    reset_at + RESET_DELAY_SECS
}

fn anchor_seconds(anchor: &str) -> Option<i64> {
    let (hour, minute) = anchor.trim().split_once(':')?;
    if hour.len() != 2 || minute.len() != 2 {
        return None;
    }
    let hour: i64 = hour.parse().ok()?;
    let minute: i64 = minute.parse().ok()?;
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) {
        return None;
    }
    Some(hour * 3600 + minute * 60)
}

fn window_seconds(hours: f64) -> Option<i64> {
    let seconds = (hours * 3600.0).round();
    (0.5..=24.0).contains(&hours).then_some(seconds as i64)
}

fn slot_on(
    date: jiff::civil::Date,
    anchor: i64,
    window: i64,
    index: u32,
    timezone: &TimeZone,
) -> Option<Slot> {
    let total = anchor + i64::from(index) * window;
    let mut day = date;
    for _ in 0..(total / 86_400) {
        day = day.tomorrow().ok()?;
    }
    let secs = total % 86_400;
    let hour = (secs / 3600) as i8;
    let minute = ((secs % 3600) / 60) as i8;
    let second = (secs % 60) as i8;
    let zoned = timezone.to_zoned(day.at(hour, minute, second, 0)).ok()?;
    let at = zoned.timestamp().as_second();
    Some(Slot {
        at,
        key: format!("schedule:{at}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(anchor: &str, timezone: &str, window: f64, slots: u32) -> Schedule {
        Schedule {
            anchor: anchor.into(),
            timezone: timezone.into(),
            window_hours: window,
            slots_per_day: slots,
        }
    }

    #[test]
    fn schedule_slots_wrap_and_respect_timezone() {
        // 2026-01-15T12:00:00Z.
        let now = 1_768_478_400;
        let s = schedule("09:30", "UTC", 5.0, 3);
        let next = next_slot(&s, now).expect("next slot");
        assert_eq!(next.at, now + 2 * 3600 + 30 * 60);
        assert_eq!(next.key, format!("schedule:{}", next.at));

        // New York is UTC-5 in January: 09:30 local is 14:30 UTC.
        let ny = schedule("09:30", "America/New_York", 5.0, 3);
        let next = next_slot(&ny, now).expect("next NY slot");
        assert_eq!(next.at, now + 2 * 3600 + 30 * 60);
    }

    #[test]
    fn slot_grace_is_thirty_minutes_and_one_slot_per_key() {
        let now = 1_768_478_400 - 15 * 60; // 11:45Z: 11:30 is 15m old
        let s = schedule("11:30", "UTC", 5.0, 3);
        let due = due_slot(&s, now, SLOT_GRACE_SECS).expect("11:30 slot still due");
        assert_eq!(due.at, now - 15 * 60);
        assert!(due_slot(&s, now, 60).is_none(), "grace expired");
        let later = next_slot(&s, due.at).expect("next");
        assert_ne!(later.key, due.key, "the next slot has a new identity");
    }

    #[test]
    fn dst_gap_uses_the_next_valid_local_time() {
        // 2026-03-08 02:30 does not exist in New York. Jiff's compatible
        // disambiguation moves it forward, so the slot still resolves.
        let now = 1_772_942_400; // 2026-03-08T00:00:00Z = 19:00 Mar 7 local
        let s = schedule("02:30", "America/New_York", 6.0, 4);
        let slot = next_slot(&s, now).expect("DST slot");
        assert!(slot.at > now);
    }

    #[test]
    fn smart_phases_follow_traffic_and_stagger() {
        let now = 1_768_478_400;
        let accounts = vec![
            SmartAccount {
                connection_id: Uuid::from_u128(1),
                provider: "claude".into(),
                recent_requests: 10,
                reset_at: None,
                used_percent: 20.0,
                window_hours: 5.0,
            },
            SmartAccount {
                connection_id: Uuid::from_u128(2),
                provider: "claude".into(),
                recent_requests: 50,
                reset_at: None,
                used_percent: 70.0,
                window_hours: 5.0,
            },
            SmartAccount {
                connection_id: Uuid::from_u128(3),
                provider: "claude".into(),
                recent_requests: 0,
                reset_at: None,
                used_percent: 0.0,
                window_hours: 5.0,
            },
        ];
        let plans = plan_smart(&accounts, now);
        let busy = plans.get(&Uuid::from_u128(2)).expect("busy");
        let quiet = plans.get(&Uuid::from_u128(1)).expect("quiet");
        let idle = plans.get(&Uuid::from_u128(3)).expect("idle");
        assert_eq!(busy.phase_offset_secs, 0, "highest usage gets phase 0");
        assert_eq!(quiet.phase_offset_secs, 5 * 3600 / 3);
        assert!(idle.idle);
        assert_eq!(idle.next_ping_at, None);
        assert!(busy.next_ping_at.is_some());
    }

    #[test]
    fn smart_with_a_reset_never_pings_before_it() {
        let now = 1_768_478_400;
        let reset = now + 600;
        let accounts = vec![SmartAccount {
            connection_id: Uuid::from_u128(4),
            provider: "codex".into(),
            recent_requests: 1,
            reset_at: Some(reset),
            used_percent: 10.0,
            window_hours: 5.0,
        }];
        let plan = plan_smart(&accounts, now)
            .get(&Uuid::from_u128(4))
            .expect("plan")
            .clone();
        assert_eq!(plan.next_ping_at, Some(reset));
        assert!(plan.due_key.is_none(), "not due before the reset");
        let due = plan_smart(&accounts, reset + 1)
            .get(&Uuid::from_u128(4))
            .expect("due")
            .clone();
        assert!(due.due_key.is_some());
    }

    #[test]
    fn invalid_timezones_and_anchors_are_rejected() {
        assert!(known_timezone("UTC"));
        assert!(known_timezone("America/New_York"));
        assert!(!known_timezone("Mars/Olympus"));
        assert!(anchor_seconds("09:30").is_some());
        assert!(anchor_seconds("24:00").is_none());
        assert!(anchor_seconds("9:30").is_none());
    }
}
