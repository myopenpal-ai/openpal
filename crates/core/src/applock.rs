//! App lock: a soft, non-cryptographic PIN gate on opening the app at all —
//! distinct from `parental.rs`'s PIN (which only gates sending chat
//! messages). This PIN never derives or protects the data-encryption key: a
//! 4-6 digit PIN is far too weak to safely be a real crypto factor, so the
//! DEK stays exactly as protected as it is today (the silent OS keychain
//! path, `bootstrap::mint_or_unlock`, unchanged). It's a pure UI/session
//! gate, checked only after the DEK has already been unlocked in the
//! background.
//!
//! `AppLockConfig`/`AppLockState` persist as two plain JSON files
//! (`applock.json`, `applock-state.json`), the same "corrupt or missing
//! falls back to defaults" shape `parental.rs` uses for its own config/
//! state. Every function here takes an explicit `&Path` and none of it is
//! ever cached on a managed Tauri state struct — `commands::verify_app_lock_pin`
//! and its siblings must work as `BootState`-only commands, readable and
//! writable before `AppState` (and so any place that could hold a live
//! copy) exists.
//!
//! Duplicates `parental.rs`'s `hash_pin`/`new_salt`/lockout shape rather
//! than sharing it — this crate's established preference for small,
//! locally-owned logic over cross-cutting abstraction (see
//! `crate::crypto::recovery`'s hand-rolled base32 for the same call), and it
//! keeps `parental.rs`'s already-tested lockout code untouched.

use crate::types::{AppLockConfig, AppLockPinCheckResult, AppLockState};
use crate::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

pub fn load_config(path: &Path) -> AppLockConfig {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            tracing::warn!("app lock config file is corrupt ({e}); using defaults");
            AppLockConfig::default()
        }),
        Err(_) => AppLockConfig::default(),
    }
}

pub fn save_config(path: &Path, config: &AppLockConfig) -> Result<()> {
    write_json(path, config)
}

pub fn load_state(path: &Path) -> AppLockState {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            tracing::warn!("app lock state file is corrupt ({e}); starting fresh");
            AppLockState::default()
        }),
        Err(_) => AppLockState::default(),
    }
}

pub fn save_state(path: &Path, state: &AppLockState) -> Result<()> {
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

/// A fresh random salt, hex-encoded — not cryptographically load-bearing on
/// its own for the same reason `parental::new_salt`'s isn't: the PIN space
/// is a handful of digits, so its job is just to keep `pin_hash` from being
/// directly comparable across installs.
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

/// Current time as epoch seconds — the clock `attempt_pin` needs, kept here
/// for the same reason as `parental::now_epoch_seconds`: `crates/app`
/// shouldn't need `chrono` as a direct dependency just to drive this call.
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
fn lockout_remaining(state: &AppLockState, now: i64) -> u32 {
    match state.pin_locked_until {
        Some(until) if until > now => (until - now) as u32,
        _ => 0,
    }
}

/// Check `pin` against `config`'s stored hash, honoring and updating
/// brute-force backoff on `state`. The one gate every app-lock command must
/// go through (`commands::verify_app_lock_pin`, `commands::set_app_lock_pin`)
/// — centralizing it here means a direct call to either command shares the
/// same lockout, not just the lock screen's own PIN pad. Unlike
/// `parental::attempt_pin`, also reports `fail_count` (not reset until a
/// correct attempt) so the frontend can decide when to offer a
/// recovery-code fallback.
pub fn attempt_pin(
    config: &AppLockConfig,
    state: &mut AppLockState,
    pin: &str,
    now: i64,
) -> AppLockPinCheckResult {
    let remaining = lockout_remaining(state, now);
    if remaining > 0 {
        return AppLockPinCheckResult {
            ok: false,
            locked_for_seconds: remaining,
            fail_count: state.pin_fail_count,
        };
    }

    let matches = match (&config.pin_hash, &config.pin_salt) {
        (Some(hash), Some(salt)) => verify_pin(pin, salt, hash),
        _ => false,
    };

    if matches {
        state.pin_fail_count = 0;
        state.pin_locked_until = None;
        return AppLockPinCheckResult {
            ok: true,
            locked_for_seconds: 0,
            fail_count: 0,
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
    AppLockPinCheckResult {
        ok: false,
        locked_for_seconds,
        fail_count: state.pin_fail_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_pin(pin: &str) -> AppLockConfig {
        let salt = new_salt();
        let hash = hash_pin(pin, &salt);
        AppLockConfig {
            enabled: true,
            pin_hash: Some(hash),
            pin_salt: Some(salt),
        }
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
    fn correct_pin_succeeds_and_resets_the_failure_count() {
        let c = config_with_pin("1234");
        let mut s = AppLockState {
            pin_fail_count: 2,
            ..AppLockState::default()
        };
        let result = attempt_pin(&c, &mut s, "1234", 1_000);
        assert!(result.ok);
        assert_eq!(result.locked_for_seconds, 0);
        assert_eq!(result.fail_count, 0);
        assert_eq!(s.pin_fail_count, 0);
        assert_eq!(s.pin_locked_until, None);
    }

    #[test]
    fn wrong_pin_attempts_are_free_until_the_threshold() {
        let c = config_with_pin("1234");
        let mut s = AppLockState::default();
        for i in 0..PIN_FREE_ATTEMPTS {
            let result = attempt_pin(&c, &mut s, "0000", 1_000);
            assert!(!result.ok);
            assert_eq!(result.locked_for_seconds, 0, "must not lock out yet");
            assert_eq!(result.fail_count, i + 1);
        }
    }

    #[test]
    fn wrong_pin_locks_out_and_backs_off_further_on_repeat_offenses() {
        let c = config_with_pin("1234");
        let mut s = AppLockState::default();
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
        let mut s = AppLockState {
            pin_fail_count: 100,
            ..AppLockState::default()
        };
        let result = attempt_pin(&c, &mut s, "0000", 1_000);
        assert_eq!(result.locked_for_seconds, PIN_LOCKOUT_MAX_SECS as u32);
    }

    #[test]
    fn missing_pin_never_matches_but_still_counts_toward_lockout() {
        let c = AppLockConfig {
            enabled: true,
            ..AppLockConfig::default()
        };
        let mut s = AppLockState::default();
        let result = attempt_pin(&c, &mut s, "1234", 1_000);
        assert!(!result.ok);
        assert_eq!(s.pin_fail_count, 1);
        assert_eq!(result.fail_count, 1);
    }

    #[test]
    fn config_roundtrip_and_corrupt_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("applock.json");

        assert!(!load_config(&path).enabled);

        let c = config_with_pin("1234");
        save_config(&path, &c).unwrap();
        let loaded = load_config(&path);
        assert!(loaded.enabled);
        assert_eq!(loaded.pin_hash, c.pin_hash);

        std::fs::write(&path, "{not json").unwrap();
        assert!(
            !load_config(&path).enabled,
            "corrupt file falls back to defaults"
        );
    }

    #[test]
    fn state_roundtrip_and_corrupt_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("applock-state.json");

        assert_eq!(load_state(&path).pin_fail_count, 0);

        let s = AppLockState {
            pin_fail_count: 2,
            pin_locked_until: Some(1_234),
        };
        save_state(&path, &s).unwrap();
        assert_eq!(load_state(&path), s);

        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(
            load_state(&path).pin_fail_count,
            0,
            "corrupt file falls back to defaults"
        );
    }
}
