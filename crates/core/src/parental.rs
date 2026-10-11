//! Parental controls: a soft daily lock on sending new chat messages (past
//! conversations always stay readable — `commands::chat_stream` is the only
//! thing this gates). Three independent triggers, any of which locks:
//!
//! - a schedule window ("only usable 7am-9pm")
//! - a cumulative daily usage cap ("at most 2 hours of app-open time a day")
//! - a daily message cap ("at most 50 messages a day")
//!
//! `ParentalConfig` (what the parent sets, PIN included) and `ParentalState`
//! (today's running counters) persist as two plain JSON files, loaded/saved
//! the same way `settings.rs`/`custom.rs` do — corrupt or missing files fall
//! back to defaults rather than blocking startup. They're separate files
//! because `ParentalState` is written far more often (every usage-accrual
//! tick, every sent message) than the rarely-changed configuration.
//!
//! Usage time is simply "the app is open", not idle-adjusted — see
//! `crate::app`'s ticker (`crates/app/src/parental.rs`) for the accrual
//! loop. That keeps the mental model a beginner (or a kid) can predict:
//! screen time, not "time actively typing".

use crate::types::{LockReason, ParentalConfig, ParentalState, ParentalStatus, PinCheckResult};
use crate::Result;
use chrono::Timelike;
use sha2::{Digest, Sha256};
use std::path::Path;

pub fn load_config(path: &Path) -> ParentalConfig {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            tracing::warn!("parental config file is corrupt ({e}); using defaults");
            ParentalConfig::default()
        }),
        Err(_) => ParentalConfig::default(),
    }
}

pub fn save_config(path: &Path, config: &ParentalConfig) -> Result<()> {
    write_json(path, config)
}

pub fn load_state(path: &Path) -> ParentalState {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            tracing::warn!("parental state file is corrupt ({e}); starting fresh");
            ParentalState::default()
        }),
        Err(_) => ParentalState::default(),
    }
}

pub fn save_state(path: &Path, state: &ParentalState) -> Result<()> {
    write_json(path, state)
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(value)?;
    // Write atomically so a crash mid-write can't corrupt the file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Zero `state`'s counters if it isn't already dated `today` ("YYYY-MM-DD",
/// local calendar date). Returns whether a reset happened, so callers only
/// need to `save_state` when something actually changed.
pub fn roll_over(state: &mut ParentalState, today: &str) -> bool {
    if state.date == today {
        return false;
    }
    state.date = today.to_string();
    state.usage_seconds = 0;
    state.messages_sent = 0;
    true
}

/// Today's local calendar date ("YYYY-MM-DD") and minutes since local
/// midnight — the two pieces of wall-clock state `evaluate`/`roll_over`
/// need. Kept out of `evaluate` itself so the lock logic stays a pure,
/// easily-tested function of its inputs.
pub fn now_local_parts() -> (String, u32) {
    let now = chrono::Local::now();
    (
        now.format("%Y-%m-%d").to_string(),
        now.hour() * 60 + now.minute(),
    )
}

/// Is `minute_of_day` inside the allowed window `[start, end)`? A window
/// that wraps past midnight (`start > end`) is allowed on both sides of
/// midnight instead of neither. A malformed or zero-width window (equal
/// start/end, including "unset") never blocks — schedule locking is opt-in
/// by `config.schedule_enabled`, not by a subtly-wrong window.
fn within_window(minute_of_day: u32, start: &str, end: &str) -> bool {
    let (Some(start), Some(end)) = (parse_hhmm(start), parse_hhmm(end)) else {
        return true;
    };
    if start == end {
        return true;
    }
    if start < end {
        minute_of_day >= start && minute_of_day < end
    } else {
        minute_of_day >= start || minute_of_day < end
    }
}

fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// Is sending currently blocked, and if so why? Pure function of its inputs
/// (no clock, no I/O) so the schedule/cap logic is directly testable;
/// callers pass `minute_of_day` from `now_local_parts` and `state` after
/// `roll_over`. Checks schedule, then usage cap, then message cap — the
/// first that's tripped is reported (a caller only needs one reason to
/// explain the lock).
pub fn evaluate(
    config: &ParentalConfig,
    state: &ParentalState,
    minute_of_day: u32,
) -> Option<LockReason> {
    if !config.enabled {
        return None;
    }
    if config.schedule_enabled
        && !within_window(minute_of_day, &config.allowed_start, &config.allowed_end)
    {
        return Some(LockReason::Schedule);
    }
    if config.usage_cap_enabled && state.usage_seconds >= u64::from(config.usage_cap_minutes) * 60 {
        return Some(LockReason::UsageCap);
    }
    if config.message_cap_enabled && state.messages_sent >= config.message_cap_per_day {
        return Some(LockReason::MessageCap);
    }
    None
}

/// The actual "may this generation happen, and should it count toward the
/// daily cap" decision behind `crates/app/src/parental.rs`'s `enforce_send`/
/// `check_send` — not a parallel implementation kept only for testability,
/// but the logic those two thin wrappers call. Mirrors `enforce_send`'s
/// original short-circuit exactly when controls are off (`state` is left
/// untouched, not even rolled over) so its behaviour stays byte-identical;
/// when controls are on, rolls over to `today` then defers to [`evaluate`],
/// and on `Ok` increments `state.messages_sent` only when `count` is true.
/// Pure — no I/O, no `AppState` — so it's directly unit-testable, which is
/// what lets Phase 2's "does an improve leave the count alone unless
/// parental controls are on" acceptance criterion be a `cargo test`
/// assertion instead of a manual `get_parental_status` check.
pub fn gate_send(
    config: &ParentalConfig,
    state: &mut ParentalState,
    today: &str,
    minute_of_day: u32,
    count: bool,
) -> std::result::Result<(), LockReason> {
    if !config.enabled {
        return Ok(());
    }
    roll_over(state, today);
    if let Some(reason) = evaluate(config, state, minute_of_day) {
        return Err(reason);
    }
    if count {
        state.messages_sent += 1;
    }
    Ok(())
}

/// Build the `get_parental_status` response from the current config/state.
pub fn status(
    config: &ParentalConfig,
    state: &ParentalState,
    minute_of_day: u32,
) -> ParentalStatus {
    let reason = evaluate(config, state, minute_of_day);
    ParentalStatus {
        locked: reason.is_some(),
        reason,
        schedule_enabled: config.schedule_enabled,
        allowed_start: config.allowed_start.clone(),
        allowed_end: config.allowed_end.clone(),
        messages_sent_today: state.messages_sent,
        message_cap_per_day: config
            .message_cap_enabled
            .then_some(config.message_cap_per_day),
        usage_seconds_today: state.usage_seconds,
        usage_cap_seconds: config
            .usage_cap_enabled
            .then_some(u64::from(config.usage_cap_minutes) * 60),
    }
}

// ---------------------------------------------------------------------------
// PIN
// ---------------------------------------------------------------------------

/// A fresh random salt, hex-encoded. Not cryptographically load-bearing on
/// its own — the PIN space is a handful of digits — its job is just to keep
/// `pin_hash` from being directly comparable across installs, at the same
/// proportionate bar as the rest of this locally-stored config.
pub fn new_salt() -> String {
    let bytes: [u8; 16] = rand::random();
    to_hex(&bytes)
}

pub fn hash_pin(pin: &str, salt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(pin.as_bytes());
    to_hex(&hasher.finalize())
}

pub fn verify_pin(pin: &str, salt: &str, expected_hash: &str) -> bool {
    hash_pin(pin, salt) == expected_hash
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Current time as epoch seconds — the clock `attempt_pin` needs. Kept here
/// (rather than callers reaching for `chrono` themselves) for the same
/// reason as `now_local_parts`: `crates/app` shouldn't need `chrono` as a
/// direct dependency just to drive this one call.
pub fn now_epoch_seconds() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Wrong-PIN attempts allowed before any lockout kicks in.
const PIN_FREE_ATTEMPTS: u32 = 3;
/// Lockout after the first attempt past the free ones; doubles with each
/// further wrong attempt (5s, 10s, 20s, ...) up to `PIN_LOCKOUT_MAX_SECS`.
const PIN_LOCKOUT_BASE_SECS: i64 = 5;
const PIN_LOCKOUT_MAX_SECS: i64 = 300;

/// Seconds remaining on an active lockout, or 0 if none.
fn lockout_remaining(state: &ParentalState, now: i64) -> u32 {
    match state.pin_locked_until {
        Some(until) if until > now => (until - now) as u32,
        _ => 0,
    }
}

/// Check `pin` against `config`'s stored hash, honoring and updating
/// brute-force backoff on `state`. The one gate every PIN-checking command
/// must go through (`commands::verify_parental_pin`,
/// `commands::set_parental_controls`) — centralizing it here means a direct
/// call to either command shares the same lockout, not just the Settings
/// UI's own unlock screen.
pub fn attempt_pin(
    config: &ParentalConfig,
    state: &mut ParentalState,
    pin: &str,
    now: i64,
) -> PinCheckResult {
    let remaining = lockout_remaining(state, now);
    if remaining > 0 {
        return PinCheckResult {
            ok: false,
            locked_for_seconds: remaining,
        };
    }

    let matches = match (&config.pin_hash, &config.pin_salt) {
        (Some(hash), Some(salt)) => verify_pin(pin, salt, hash),
        _ => false,
    };

    if matches {
        state.pin_fail_count = 0;
        state.pin_locked_until = None;
        return PinCheckResult {
            ok: true,
            locked_for_seconds: 0,
        };
    }

    state.pin_fail_count += 1;
    let locked_for_seconds = if state.pin_fail_count > PIN_FREE_ATTEMPTS {
        let exponent = (state.pin_fail_count - PIN_FREE_ATTEMPTS - 1).min(10);
        let backoff = PIN_LOCKOUT_BASE_SECS
            .saturating_mul(1i64 << exponent)
            .min(PIN_LOCKOUT_MAX_SECS);
        state.pin_locked_until = Some(now + backoff);
        backoff as u32
    } else {
        0
    };
    PinCheckResult {
        ok: false,
        locked_for_seconds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(enabled: bool) -> ParentalConfig {
        ParentalConfig {
            enabled,
            ..ParentalConfig::default()
        }
    }

    fn state(usage_seconds: u64, messages_sent: u32) -> ParentalState {
        ParentalState {
            date: "2026-07-28".to_string(),
            usage_seconds,
            messages_sent,
            ..ParentalState::default()
        }
    }

    fn config_with_pin(pin: &str) -> ParentalConfig {
        let salt = new_salt();
        let hash = hash_pin(pin, &salt);
        ParentalConfig {
            pin_hash: Some(hash),
            pin_salt: Some(salt),
            ..config(true)
        }
    }

    #[test]
    fn disabled_never_locks_regardless_of_caps() {
        let mut c = config(false);
        c.schedule_enabled = true;
        c.usage_cap_enabled = true;
        c.usage_cap_minutes = 0;
        c.message_cap_enabled = true;
        c.message_cap_per_day = 0;
        assert_eq!(evaluate(&c, &state(9999, 9999), 0), None);
    }

    #[test]
    fn schedule_blocks_outside_a_same_day_window() {
        let mut c = config(true);
        c.schedule_enabled = true;
        c.allowed_start = "07:00".to_string();
        c.allowed_end = "21:00".to_string();
        let s = state(0, 0);

        assert_eq!(evaluate(&c, &s, 6 * 60 + 59), Some(LockReason::Schedule));
        assert_eq!(evaluate(&c, &s, 7 * 60), None);
        assert_eq!(evaluate(&c, &s, 20 * 60 + 59), None);
        assert_eq!(evaluate(&c, &s, 21 * 60), Some(LockReason::Schedule));
    }

    #[test]
    fn schedule_window_wraps_past_midnight() {
        let mut c = config(true);
        c.schedule_enabled = true;
        c.allowed_start = "20:00".to_string();
        c.allowed_end = "07:00".to_string();
        let s = state(0, 0);

        // Allowed late evening and early morning...
        assert_eq!(evaluate(&c, &s, 23 * 60), None);
        assert_eq!(evaluate(&c, &s, 60), None);
        // ...blocked during the day.
        assert_eq!(evaluate(&c, &s, 12 * 60), Some(LockReason::Schedule));
    }

    #[test]
    fn zero_width_window_never_blocks() {
        let mut c = config(true);
        c.schedule_enabled = true;
        c.allowed_start = "09:00".to_string();
        c.allowed_end = "09:00".to_string();
        assert_eq!(evaluate(&c, &state(0, 0), 3 * 60), None);
    }

    #[test]
    fn usage_cap_blocks_once_reached() {
        let mut c = config(true);
        c.usage_cap_enabled = true;
        c.usage_cap_minutes = 60;
        assert_eq!(evaluate(&c, &state(59 * 60, 0), 12 * 60), None);
        assert_eq!(
            evaluate(&c, &state(60 * 60, 0), 12 * 60),
            Some(LockReason::UsageCap)
        );
    }

    #[test]
    fn message_cap_blocks_once_reached() {
        let mut c = config(true);
        c.message_cap_enabled = true;
        c.message_cap_per_day = 5;
        assert_eq!(evaluate(&c, &state(0, 4), 12 * 60), None);
        assert_eq!(
            evaluate(&c, &state(0, 5), 12 * 60),
            Some(LockReason::MessageCap)
        );
    }

    #[test]
    fn schedule_is_reported_before_caps_when_both_trip() {
        let mut c = config(true);
        c.schedule_enabled = true;
        c.allowed_start = "07:00".to_string();
        c.allowed_end = "08:00".to_string();
        c.message_cap_enabled = true;
        c.message_cap_per_day = 1;
        assert_eq!(
            evaluate(&c, &state(0, 5), 20 * 60),
            Some(LockReason::Schedule)
        );
    }

    #[test]
    fn roll_over_resets_counters_on_a_new_day_and_is_idempotent_same_day() {
        let mut s = state(500, 10);
        assert!(roll_over(&mut s, "2026-07-29"));
        assert_eq!(s.usage_seconds, 0);
        assert_eq!(s.messages_sent, 0);
        assert_eq!(s.date, "2026-07-29");

        s.usage_seconds = 30;
        assert!(!roll_over(&mut s, "2026-07-29"));
        assert_eq!(s.usage_seconds, 30, "same-day roll_over must not reset");
    }

    #[test]
    fn default_state_always_rolls_over_on_first_check() {
        let mut s = ParentalState::default();
        assert!(roll_over(&mut s, "2026-07-28"));
    }

    #[test]
    fn config_roundtrip_and_corrupt_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parental-config.json");

        assert!(!load_config(&path).enabled);

        let mut c = config(true);
        c.pin_hash = Some("abc".to_string());
        save_config(&path, &c).unwrap();
        let loaded = load_config(&path);
        assert!(loaded.enabled);
        assert_eq!(loaded.pin_hash.as_deref(), Some("abc"));

        std::fs::write(&path, "{not json").unwrap();
        assert!(
            !load_config(&path).enabled,
            "corrupt file falls back to defaults"
        );
    }

    #[test]
    fn state_roundtrip_and_corrupt_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parental-state.json");

        assert_eq!(load_state(&path).messages_sent, 0);

        let s = state(120, 3);
        save_state(&path, &s).unwrap();
        assert_eq!(load_state(&path), s);

        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(
            load_state(&path).messages_sent,
            0,
            "corrupt file falls back to defaults"
        );
    }

    #[test]
    fn pin_hash_roundtrips_and_rejects_the_wrong_pin() {
        let salt = new_salt();
        let hash = hash_pin("1234", &salt);
        assert!(verify_pin("1234", &salt, &hash));
        assert!(!verify_pin("4321", &salt, &hash));
    }

    #[test]
    fn salts_are_not_reused() {
        assert_ne!(new_salt(), new_salt());
    }

    #[test]
    fn status_reports_none_for_disabled_limits() {
        let c = config(true);
        let s = status(&c, &state(10, 2), 12 * 60);
        assert!(!s.locked);
        assert_eq!(s.message_cap_per_day, None);
        assert_eq!(s.usage_cap_seconds, None);
        assert_eq!(s.messages_sent_today, 2);
        assert_eq!(s.usage_seconds_today, 10);
    }

    #[test]
    fn status_surfaces_caps_and_lock_reason_when_enabled() {
        let mut c = config(true);
        c.message_cap_enabled = true;
        c.message_cap_per_day = 5;
        c.usage_cap_enabled = true;
        c.usage_cap_minutes = 1;
        let s = status(&c, &state(60, 5), 12 * 60);
        assert!(s.locked);
        assert_eq!(s.reason, Some(LockReason::UsageCap));
        assert_eq!(s.message_cap_per_day, Some(5));
        assert_eq!(s.usage_cap_seconds, Some(60));
    }

    #[test]
    fn correct_pin_succeeds_and_resets_the_failure_count() {
        let c = config_with_pin("1234");
        let mut s = state(0, 0);
        s.pin_fail_count = 2;
        let result = attempt_pin(&c, &mut s, "1234", 1_000);
        assert!(result.ok);
        assert_eq!(result.locked_for_seconds, 0);
        assert_eq!(s.pin_fail_count, 0);
        assert_eq!(s.pin_locked_until, None);
    }

    #[test]
    fn wrong_pin_attempts_are_free_until_the_threshold() {
        let c = config_with_pin("1234");
        let mut s = state(0, 0);
        for _ in 0..PIN_FREE_ATTEMPTS {
            let result = attempt_pin(&c, &mut s, "0000", 1_000);
            assert!(!result.ok);
            assert_eq!(result.locked_for_seconds, 0, "must not lock out yet");
        }
    }

    #[test]
    fn wrong_pin_locks_out_and_backs_off_further_on_repeat_offenses() {
        let c = config_with_pin("1234");
        let mut s = state(0, 0);
        let now = 1_000;
        for _ in 0..PIN_FREE_ATTEMPTS {
            attempt_pin(&c, &mut s, "0000", now);
        }

        // The attempt right past the free ones locks out for the base duration.
        let first_lock = attempt_pin(&c, &mut s, "0000", now);
        assert!(!first_lock.ok);
        assert_eq!(first_lock.locked_for_seconds, 5);

        // Still locked partway through — even the *correct* PIN is refused.
        let still_locked = attempt_pin(&c, &mut s, "1234", now + 2);
        assert!(!still_locked.ok);
        assert!(still_locked.locked_for_seconds > 0);

        // Once the lockout expires, the next wrong attempt backs off further.
        let second_lock = attempt_pin(&c, &mut s, "0000", now + 5);
        assert!(!second_lock.ok);
        assert_eq!(second_lock.locked_for_seconds, 10);
    }

    #[test]
    fn pin_lockout_duration_is_capped() {
        let c = config_with_pin("1234");
        let mut s = ParentalState {
            pin_fail_count: 100,
            ..state(0, 0)
        };
        let result = attempt_pin(&c, &mut s, "0000", 1_000);
        assert_eq!(result.locked_for_seconds, PIN_LOCKOUT_MAX_SECS as u32);
    }

    #[test]
    fn missing_pin_never_matches_but_still_counts_toward_lockout() {
        let c = config(true); // no pin_hash/pin_salt set
        let mut s = state(0, 0);
        let result = attempt_pin(&c, &mut s, "1234", 1_000);
        assert!(!result.ok);
        assert_eq!(s.pin_fail_count, 1);
    }

    #[test]
    fn gate_send_disabled_config_leaves_state_untouched_regardless_of_count() {
        let c = config(false);
        let mut s = state(0, 4);
        assert_eq!(gate_send(&c, &mut s, "2026-07-28", 12 * 60, true), Ok(()));
        assert_eq!(s.messages_sent, 4);
        assert_eq!(gate_send(&c, &mut s, "2026-07-28", 12 * 60, false), Ok(()));
        assert_eq!(s.messages_sent, 4);
    }

    #[test]
    fn gate_send_locked_refuses_regardless_of_count() {
        let mut c = config(true);
        c.message_cap_enabled = true;
        c.message_cap_per_day = 1;
        let mut s = state(0, 1);
        assert_eq!(
            gate_send(&c, &mut s, "2026-07-28", 12 * 60, true),
            Err(LockReason::MessageCap)
        );
        assert_eq!(
            s.messages_sent, 1,
            "a refused gate must never increment the count"
        );
        assert_eq!(
            gate_send(&c, &mut s, "2026-07-28", 12 * 60, false),
            Err(LockReason::MessageCap)
        );
        assert_eq!(s.messages_sent, 1);
    }

    #[test]
    fn gate_send_unlocked_check_only_leaves_messages_sent_unchanged() {
        let c = config(true);
        let mut s = state(0, 4);
        assert_eq!(gate_send(&c, &mut s, "2026-07-28", 12 * 60, false), Ok(()));
        assert_eq!(s.messages_sent, 4);
    }

    #[test]
    fn gate_send_unlocked_counting_increments_by_one() {
        let c = config(true);
        let mut s = state(0, 4);
        assert_eq!(gate_send(&c, &mut s, "2026-07-28", 12 * 60, true), Ok(()));
        assert_eq!(s.messages_sent, 5);
    }

    #[test]
    fn gate_send_rolls_over_to_a_new_day_before_counting() {
        let c = config(true);
        let mut s = state(500, 10); // stale date is "2026-07-28" via the `state` helper
        assert_eq!(gate_send(&c, &mut s, "2026-07-29", 12 * 60, true), Ok(()));
        assert_eq!(s.date, "2026-07-29");
        assert_eq!(s.usage_seconds, 0);
        assert_eq!(
            s.messages_sent, 1,
            "rolled-over count starts fresh, then counts this one"
        );
    }
}
