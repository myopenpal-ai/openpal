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
use tauri::{AppHandle, Manager};

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
        maybe_unload(&state).await;
    }
}

async fn maybe_unload(state: &AppState) {
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
        if let Err(e) = state.engine.stop_if_current(info.generation).await {
            tracing::warn!("auto-unload stop failed: {e}");
        }
    }
}
