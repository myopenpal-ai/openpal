//! Background loop that writes Settings → Your data's automatic backup
//! (`openpal_core::backup`) whenever one is due. Spawned once per unlock
//! from `state::finish_unlock`, like `crate::idle`.
//!
//! The first check runs shortly after unlock rather than waiting a whole
//! interval, so a backup that fell due while the app was closed is written
//! soon after the next launch. Turning backup on, or choosing a new folder,
//! runs one straight away too (`commands::set_backup_enabled`/
//! `choose_backup_folder`).
//!
//! The database is read under `app_db`'s lock in one short go; building,
//! encrypting and writing the file happens on a blocking thread with the
//! lock released, so chatting and settings never wait on a backup.

use crate::state::AppState;
use openpal_core::backup::{self, BackupState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

/// Tells the webview a backup just finished (or failed), so Settings → Your
/// data re-reads `get_backup_state`. Mirrored by hand in `ui/src/lib/api.ts`
/// (`onBackupStateChanged`).
pub const BACKUP_STATE_CHANGED_EVENT: &str = "backup-state-changed";

/// Long enough to stay out of the way of startup, short enough to count as
/// "soon after the next unlock".
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(60);

/// Backups are daily at most, so a coarse tick is plenty.
const CHECK_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// One backup at a time: a tick and a "turned it on" can otherwise race.
static RUNNING: AtomicBool = AtomicBool::new(false);

pub async fn run(app: AppHandle) {
    tokio::time::sleep(FIRST_CHECK_DELAY).await;
    let mut ticker = tokio::time::interval(CHECK_INTERVAL);
    loop {
        ticker.tick().await;
        run_if_due(&app).await;
    }
}

/// Write a backup now if one is due; otherwise do nothing.
pub async fn run_if_due(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if state.paths.portable {
        return;
    }
    let current = backup::load_state(&state.app_db.lock().unwrap());
    if !backup::is_due(&current, now_ms()) {
        return;
    }
    let Some(folder) = current.folder else {
        return;
    };
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let result = write_one(app, &state, folder.into()).await;
    RUNNING.store(false, Ordering::SeqCst);

    let finished = now_ms();
    let recorded = backup::update_state(&state.app_db.lock().unwrap(), |s| {
        s.last_attempt_ms = Some(finished);
        match &result {
            Ok(()) => {
                s.last_success_ms = Some(finished);
                s.last_error = None;
            }
            Err(reason) => s.last_error = Some(reason.clone()),
        }
    });
    if let Err(e) = recorded {
        tracing::warn!("backup: couldn't record the result: {e}");
    }
    let _ = app.emit(BACKUP_STATE_CHANGED_EVENT, ());
}

async fn write_one(
    app: &AppHandle,
    state: &AppState,
    folder: std::path::PathBuf,
) -> Result<(), String> {
    let snapshot =
        backup::read_snapshot(&state.app_db.lock().unwrap()).map_err(crate::commands::friendly)?;
    let wrap_path = state.paths.data_dir.join("keys").join("recovery.wrap");
    let app = app.clone();
    let written = tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let dek = state.dek.as_ref().ok_or_else(no_recovery_code)?;
        let wrap = std::fs::read(&wrap_path).map_err(|_| no_recovery_code())?;
        backup::write_backup(&snapshot, &wrap, dek, &folder, now_ms())
            .map_err(crate::commands::friendly)
    })
    .await
    .map_err(|e| {
        tracing::error!("backup: the backup task failed: {e}");
        "OpenPal couldn't save the backup file.".to_string()
    })??;
    tracing::info!(file = ?written.file_name(), "backup: written");
    Ok(())
}

fn no_recovery_code() -> String {
    "Set up your recovery code first.".to_string()
}

/// The state Settings → Your data shows, read under a short lock.
pub fn current(state: &AppState) -> BackupState {
    backup::load_state(&state.app_db.lock().unwrap())
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
