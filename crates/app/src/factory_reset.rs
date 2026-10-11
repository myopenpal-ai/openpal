//! Applies a pending factory reset. Like Portable's own update-apply hop
//! (`crates/core/src/update_portable.rs`, wired in at
//! `crates/app/src/portable.rs::apply_pending_update`), a relaunch carries
//! the work across a process boundary — but simpler, since there's no
//! executable to swap, just a wipe to run before the new process's own
//! `setup()` ever touches the data directory. `commands::factory_reset`
//! spawns this same executable with the two env vars below and exits; the
//! fresh process's very first `run()` call (before the `tauri::Builder`
//! chain starts, same spot `apply_pending_update` runs) sees them, wipes,
//! and falls straight through into an ordinary boot — which, finding no
//! `keys/keychain.wrap` and no OS keychain entry, mints a brand-new master
//! key exactly like a first-ever launch.

use std::path::Path;

const ENV_DATA_DIR: &str = "OPENPAL_FACTORY_RESET_DATA_DIR";
const ENV_MODELS_DIR: &str = "OPENPAL_FACTORY_RESET_MODELS_DIR";
const ENV_OLD_PID: &str = "OPENPAL_FACTORY_RESET_OLD_PID";

/// A no-op on an ordinary launch (three env var reads). On the relaunch
/// [`spawn_relaunch_for_reset`] spawns: waits for the still-shutting-down
/// old process to actually exit (it still holds `app.sqlite` and the log
/// files open the instant this one starts — see
/// `openpal_core::boot::factory_reset::wait_for_pid_exit`'s own doc comment
/// for why skipping this isn't just a cosmetic race), then wipes the data
/// directory and the OS keychain entry, then falls through into a
/// completely normal `run()` continuation — unlike the Portable
/// update-apply helper, this process is a real, first-class launch, not a
/// throwaway copy that exits immediately.
pub fn apply_pending_reset() {
    let (Ok(data_dir), Ok(models_dir), Ok(old_pid)) = (
        std::env::var(ENV_DATA_DIR),
        std::env::var(ENV_MODELS_DIR),
        std::env::var(ENV_OLD_PID),
    ) else {
        return;
    };
    if let Ok(old_pid) = old_pid.parse() {
        openpal_core::boot::factory_reset::wait_for_pid_exit(
            old_pid,
            openpal_core::boot::factory_reset::OLD_PROCESS_EXIT_TIMEOUT,
        );
    }
    let _ = openpal_core::boot::factory_reset::wipe(Path::new(&data_dir), Path::new(&models_dir));
    // On Android this runs before the Tauri plugin registry exists (this
    // whole function is called before `tauri::Builder::default()`, same as
    // `apply_pending_update`), so `secret_store::store()`'s plugin handle
    // isn't ready yet and this delete is a silent no-op there — matches
    // `spawn_relaunch_for_reset`'s re-exec relaunch, which doesn't work on
    // Android either; factory reset on Android needs its own design, not
    // covered here.
    let _ = openpal_core::crypto::keychain::delete(crate::secret_store::store().as_ref());
}

/// Spawns this same executable with the env vars [`apply_pending_reset`]
/// looks for, including this (the still-running) process's own pid so the
/// relaunch knows to wait for it. The caller (`commands::factory_reset`)
/// exits the current process right after via `app.exit(0)`, the same shape
/// as the Portable update-apply helper's own spawn-then-exit.
pub fn spawn_relaunch_for_reset(data_dir: &Path, models_dir: &Path) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe)
        .env(ENV_DATA_DIR, data_dir)
        .env(ENV_MODELS_DIR, models_dir)
        .env(ENV_OLD_PID, std::process::id().to_string())
        .spawn()?;
    Ok(())
}
