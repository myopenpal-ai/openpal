//! Background loop that unloads the local engine after inactivity
//! ("auto-unload"), per `Settings.auto_unload_*`. Spawned once for the app's
//! lifetime from `lib.rs::run()`.
//!
//! Backend-authoritative by design: a pure JS timer in the webview can be
//! throttled while the window is minimized/backgrounded, which would defeat
//! the point of reclaiming RAM/VRAM while the user is away from the app. This
//! loop runs regardless of whether the webview is visible.
//!
//! This loop only ever sees the resulting `idle_for` — it has no notion of
//! *why* the clock is fresh. Besides a finished chat turn, the frontend also
//! resets it while the user is typing (see `commands::snooze_auto_unload`),
//! which is what makes the idle countdown appear to "pause" while composing.

use crate::state::AppState;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Tell the webview the engine's status may have changed behind its back.
/// Mirrored by hand in `ui/src/lib/api.ts` (`onEngineStatusChanged`).
///
/// A payload-less nudge, not the new status: the UI re-reads
/// `get_engine_status` and diffs against what it last saw, which is also what
/// its 2-second poll does, so a nudge and a poll that both observe the same
/// unload still produce one alert, and a nudge for a stop that turned out to
/// be a no-op (see `maybe_unload`) produces none. The poll alone is a JS timer
/// the OS can throttle while the window is minimized, which is exactly when an
/// auto-unload happens; the event wakes the page up promptly instead.
pub const ENGINE_STATUS_CHANGED_EVENT: &str = "engine-status-changed";

/// How often to re-check the idle clock. Coarser than the frontend's 1s/10s
/// display cadence (`ui/src/views/Sidebar.tsx`'s countdown badge computes its
/// own display locally from `EngineStatus::Running.autoUnloadAt`) — this loop
/// only needs to pull the trigger reasonably close to when that countdown
/// reaches zero, not tick a display itself.
const CHECK_INTERVAL: Duration = Duration::from_secs(5);

pub async fn run(app: AppHandle) {
    let mut ticker = tokio::time::interval(CHECK_INTERVAL);
    loop {
        ticker.tick().await;
        let Some(state) = app.try_state::<AppState>() else {
            continue;
        };
        maybe_unload(&app, &state).await;
    }
}

async fn maybe_unload(app: &AppHandle, state: &AppState) {
    let Some(info) = state.engine.activity().await else {
        return; // not Running
    };
    if info.generating {
        return;
    }
    let (enabled, include_disk_slow, minutes) = {
        let settings = state.settings.lock().await;
        (
            settings.auto_unload_enabled,
            settings.auto_unload_include_disk_slow,
            settings.auto_unload_minutes,
        )
    };
    if !enabled || (info.disk_slow && !include_disk_slow) {
        return;
    }
    let limit = Duration::from_secs(u64::from(minutes) * 60);
    if info.idle_for >= limit {
        tracing::info!(
            idle_secs = info.idle_for.as_secs(),
            "auto-unload: idle timeout reached"
        );
        match state.engine.stop_if_current(info.generation).await {
            // `Ok` also covers "the generation had moved on, nothing was
            // stopped" (`stop_if_current` does not say which). Emitting then
            // is harmless: the nudge carries no status, so the UI re-reads
            // the real one and shows nothing new.
            Ok(()) => emit_engine_status_changed(app),
            Err(e) => tracing::warn!("auto-unload stop failed: {e}"),
        }
    }
}

fn emit_engine_status_changed<R: Runtime>(app: &AppHandle<R>) {
    if let Err(e) = app.emit(ENGINE_STATUS_CHANGED_EVENT, ()) {
        tracing::warn!("could not emit {ENGINE_STATUS_CHANGED_EVENT}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use tauri::Listener;

    #[test]
    fn the_event_name_matches_what_the_ui_listens_for() {
        // `ui/src/lib/api.ts` listens under this literal; no test crosses the
        // Rust/TypeScript seam, so pin the wire name here.
        assert_eq!(ENGINE_STATUS_CHANGED_EVENT, "engine-status-changed");
    }

    #[test]
    fn emitting_the_nudge_reaches_a_listener_with_no_payload() {
        let app = tauri::test::mock_app();
        let (tx, rx) = mpsc::channel();
        app.listen(ENGINE_STATUS_CHANGED_EVENT, move |event| {
            let _ = tx.send(event.payload().to_owned());
        });

        emit_engine_status_changed(app.handle());

        let payload = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("the nudge should reach a listener");
        // `()` serializes to JSON `null`: nothing for the UI to trust, so it
        // has to re-read the status.
        assert_eq!(payload, "null");
    }
}
