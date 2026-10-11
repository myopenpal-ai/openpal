//! Hybrid Logical Clock — per-event timestamps for Phase 3's merge
//! algorithm (per-message append merge for conversations, last-writer-wins
//! for everything else, see `crate::sync::conversations`/
//! `crate::sync::field_versions`). Standard HLC construction (Kulkarni et
//! al., "Logical Physical Clocks"): physical time plus a logical counter,
//! so two events that land in the same millisecond (or arrive with a
//! skewed/behind-clock device) still get a deterministic total order.
//!
//! Reuses the device ids `crate::sync::pairing::device_id_for` already
//! establishes (Phase 2) as the tiebreak component — no new identity
//! concept.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// A single Hybrid Logical Clock timestamp. `Ord`/`PartialOrd` derive in
/// field-declaration order — `(physical_ms, counter, device_id)`
/// lexicographic — which is exactly the total order the merge algorithm
/// needs: physical time first, the logical counter breaks same-millisecond
/// ties, `device_id` breaks any remaining tie deterministically (two
/// different devices can otherwise mint an identical `(physical_ms,
/// counter)` pair independently).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Hash)]
#[serde(rename_all = "camelCase")]
pub struct Hlc {
    pub physical_ms: i64,
    pub counter: u32,
    pub device_id: String,
}

impl Hlc {
    /// A synthetic HLC for seeding a conversation's pre-sync-scope history
    /// (`crate::sync::conversations`' bootstrapping step) — `physical_ms:
    /// 0` puts every synthetic HLC strictly before any real one (real
    /// clocks never read as 0 in practice, and even a clock at the Unix
    /// epoch is not a concern for this app), and `index` orders the
    /// existing messages among themselves in their original array order.
    pub fn synthetic(index: u32, device_id: &str) -> Self {
        Self {
            physical_ms: 0,
            counter: index,
            device_id: device_id.to_string(),
        }
    }

    /// `"{physical_ms}-{counter}-{device_id}"` — used as
    /// `sync_message_ops.op_id` and as the dedup/idempotency key for
    /// applying a received op twice. Device ids are already
    /// hex-alphanumeric (`pairing::device_id_for`), so a plain
    /// hyphen-joined string is an unambiguous encoding — no escaping
    /// needed.
    pub fn op_id(&self) -> String {
        format!("{}-{}-{}", self.physical_ms, self.counter, self.device_id)
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Mints monotonically-nondecreasing `Hlc` values for one device. Not
/// persisted across app restarts — a fresh `HlcClock` at each launch is
/// fine, since `physical_ms` always advances with real wall-clock time and
/// the `counter` only exists to break ties within a single millisecond
/// (there is no cross-restart ordering requirement this would violate:
/// restarting the app takes far longer than one millisecond).
pub struct HlcClock {
    device_id: String,
    last: Mutex<Hlc>,
}

impl HlcClock {
    /// This clock's device id, for callers (e.g.
    /// `crate::sync::conversations::seed`) that need to construct a
    /// synthetic `Hlc` without minting a real one.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn new(device_id: String) -> Self {
        let last = Hlc {
            physical_ms: 0,
            counter: 0,
            device_id: device_id.clone(),
        };
        Self {
            device_id,
            last: Mutex::new(last),
        }
    }

    /// A fresh `Hlc` for a purely local event (a local write not made in
    /// direct response to just having received a remote one — see
    /// [`Self::observe`] for that case).
    pub fn next(&self) -> Hlc {
        let mut last = self.last.lock().unwrap();
        let physical = now_ms().max(last.physical_ms);
        let counter = if physical == last.physical_ms {
            last.counter + 1
        } else {
            0
        };
        let hlc = Hlc {
            physical_ms: physical,
            counter,
            device_id: self.device_id.clone(),
        };
        *last = hlc.clone();
        hlc
    }

    /// Bumps this clock's state after observing `remote` (a received `Hlc`
    /// from another device), per the standard HLC receive-event rule, and
    /// returns a fresh local `Hlc` that is causally after it. Not currently
    /// called by Phase 3's merge logic (applying a received op doesn't
    /// itself mint a new local `Hlc` — it just records the remote one), but
    /// kept as part of this clock's public API for any future local write
    /// that should be ordered after specific remote knowledge (e.g. a user
    /// action taken immediately after seeing newly-synced content).
    pub fn observe(&self, remote: &Hlc) -> Hlc {
        let mut last = self.last.lock().unwrap();
        let physical = now_ms().max(last.physical_ms).max(remote.physical_ms);
        let counter = if physical == last.physical_ms && physical == remote.physical_ms {
            last.counter.max(remote.counter) + 1
        } else if physical == last.physical_ms {
            last.counter + 1
        } else if physical == remote.physical_ms {
            remote.counter + 1
        } else {
            0
        };
        let hlc = Hlc {
            physical_ms: physical,
            counter,
            device_id: self.device_id.clone(),
        };
        *last = hlc.clone();
        hlc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_is_strictly_monotonic() {
        let clock = HlcClock::new("device-a".to_string());
        let a = clock.next();
        let b = clock.next();
        let c = clock.next();
        assert!(a < b, "{a:?} should be < {b:?}");
        assert!(b < c, "{b:?} should be < {c:?}");
    }

    #[test]
    fn ordering_is_physical_then_counter_then_device_id() {
        let earlier = Hlc {
            physical_ms: 100,
            counter: 5,
            device_id: "z".to_string(),
        };
        let later_physical = Hlc {
            physical_ms: 101,
            counter: 0,
            device_id: "a".to_string(),
        };
        assert!(
            earlier < later_physical,
            "lower physical_ms must sort first regardless of counter/device_id"
        );

        let same_physical_lower_counter = Hlc {
            physical_ms: 100,
            counter: 1,
            device_id: "z".to_string(),
        };
        let same_physical_higher_counter = Hlc {
            physical_ms: 100,
            counter: 2,
            device_id: "a".to_string(),
        };
        assert!(
            same_physical_lower_counter < same_physical_higher_counter,
            "same physical_ms: lower counter must sort first regardless of device_id"
        );

        let tie_a = Hlc {
            physical_ms: 100,
            counter: 1,
            device_id: "a".to_string(),
        };
        let tie_b = Hlc {
            physical_ms: 100,
            counter: 1,
            device_id: "b".to_string(),
        };
        assert!(
            tie_a < tie_b,
            "identical physical_ms and counter: device_id breaks the tie"
        );
    }

    #[test]
    fn synthetic_hlcs_sort_before_any_real_one_and_by_index_among_themselves() {
        let s0 = Hlc::synthetic(0, "device-a");
        let s1 = Hlc::synthetic(1, "device-a");
        let real = HlcClock::new("device-a".to_string()).next();
        assert!(s0 < s1);
        assert!(s1 < real);
    }

    #[test]
    fn observe_bumps_past_a_remote_hlc_that_is_ahead() {
        let clock = HlcClock::new("device-a".to_string());
        let far_future_remote = Hlc {
            physical_ms: now_ms() + 1_000_000,
            counter: 3,
            device_id: "device-b".to_string(),
        };
        let observed = clock.observe(&far_future_remote);
        assert!(
            observed > far_future_remote,
            "observing a remote HLC ahead of real time must produce something strictly after it"
        );

        // The clock's own next() call must stay past that point too, not
        // silently fall back to wall-clock time.
        let next = clock.next();
        assert!(next > observed);
    }

    #[test]
    fn observe_with_a_remote_hlc_behind_real_time_just_advances_normally() {
        let clock = HlcClock::new("device-a".to_string());
        let stale_remote = Hlc {
            physical_ms: 1,
            counter: 0,
            device_id: "device-b".to_string(),
        };
        let a = clock.next();
        let observed = clock.observe(&stale_remote);
        assert!(
            observed > a,
            "observing a stale remote HLC must not move this clock backward"
        );
    }

    #[test]
    fn op_id_round_trips_distinct_values_to_distinct_strings() {
        let a = Hlc {
            physical_ms: 100,
            counter: 1,
            device_id: "dev-a".to_string(),
        };
        let b = Hlc {
            physical_ms: 100,
            counter: 2,
            device_id: "dev-a".to_string(),
        };
        assert_ne!(a.op_id(), b.op_id());
    }
}
