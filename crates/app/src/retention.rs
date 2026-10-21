//! Background loop for "Delete old conversations"
//! (`Settings.auto_delete_after_days`). Spawned from `state::finish_unlock`,
//! so its first tick — immediate, `tokio::time::interval`'s first tick never
//! waits — runs on every unlock, then again every `CHECK_INTERVAL` while the
//! app stays open (well inside the once-a-day minimum). Which conversations
//! go is `openpal_core::retention::expired`'s decision; each one is removed
//! through `commands::remove_conversation`, the manual delete's own path, so
//! with sync on the deletion reaches every paired device and with sync off
//! only the local copy goes.
//!
//! Backend-side for the same reason as `crate::idle`: a webview timer can be
//! throttled while the window is hidden, and nothing here needs the UI.

use crate::state::AppState;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Tells the webview which conversations the cleanup just deleted, so the
/// sidebar drops them (and an open one closes) without a reload. Mirrored by
/// hand in `ui/src/lib/api.ts` (`onConversationsPruned`).
pub const CONVERSATIONS_PRUNED_EVENT: &str = "conversations-pruned";

const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

pub async fn run(app: AppHandle) {
    let mut ticker = tokio::time::interval(CHECK_INTERVAL);
    loop {
        ticker.tick().await;
        let Some(state) = app.try_state::<AppState>() else {
            continue;
        };
        let deleted = prune(&state).await;
        if !deleted.is_empty() {
            emit_conversations_pruned(&app, &deleted);
        }
    }
}

/// Deletes every conversation the saved period selects, and returns their
/// ids. A no-op while the setting is Never. One failed delete is logged and
/// skipped, never a reason to stop the rest.
pub async fn prune(state: &AppState) -> Vec<String> {
    let after_days = state.settings.lock().await.auto_delete_after_days;
    if after_days.is_none() {
        return Vec::new();
    }
    let metas = openpal_core::store::list(&state.app_db.lock().unwrap());
    let mut deleted = Vec::new();
    for id in openpal_core::retention::expired(&metas, now_ms(), after_days) {
        match crate::commands::remove_conversation(state, &id).await {
            Ok(()) => deleted.push(id),
            Err(e) => tracing::warn!("couldn't delete an old conversation: {e}"),
        }
    }
    if !deleted.is_empty() {
        tracing::info!(count = deleted.len(), "deleted old conversations");
    }
    deleted
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn emit_conversations_pruned<R: Runtime>(app: &AppHandle<R>, ids: &[String]) {
    if let Err(e) = app.emit(CONVERSATIONS_PRUNED_EVENT, ids) {
        tracing::warn!("could not emit {CONVERSATIONS_PRUNED_EVENT}: {e}");
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
        assert_eq!(CONVERSATIONS_PRUNED_EVENT, "conversations-pruned");
    }

    #[test]
    fn the_event_carries_the_deleted_ids() {
        let app = tauri::test::mock_app();
        let (tx, rx) = mpsc::channel();
        app.listen(CONVERSATIONS_PRUNED_EVENT, move |event| {
            let _ = tx.send(event.payload().to_owned());
        });

        emit_conversations_pruned(app.handle(), &["a".to_string(), "b".to_string()]);

        let payload = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("the event should reach a listener");
        assert_eq!(payload, r#"["a","b"]"#);
    }
}
