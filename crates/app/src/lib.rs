mod agent_bridge;
// Desktop: real cpal/rodio native audio (audio.rs). Mobile: a stub with the
// same public API (audio_mobile.rs) — see that file's doc comment for why.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[path = "audio.rs"]
mod audio;
#[cfg(any(target_os = "android", target_os = "ios"))]
#[path = "audio_mobile.rs"]
mod audio;
mod bootstrap;
mod commands;
mod factory_reset;
mod idle;
mod logging;
// OS-native notifications: one `Notifier` backend per kind of platform, picked
// by `notify::select` at the start of `setup()` below rather than by swapping
// files here, because a macOS binary only knows at runtime whether it is
// inside an app bundle (see notify/mod.rs).
mod notify;
// Desktop/iOS: spawn the OS's default opener (opener.rs). Android: bridges
// Intent.ACTION_VIEW instead (opener_android.rs) — see that file's own doc
// comment for why a subprocess spawn doesn't work there at all.
#[cfg(not(target_os = "android"))]
#[path = "opener.rs"]
mod opener;
#[cfg(target_os = "android")]
#[path = "opener_android.rs"]
mod opener;
mod parental;
mod portable;
// Desktop + iOS: the real OS keychain via `keyring` (secret_store.rs).
// Android: a Tauri mobile-plugin bridge to Keystore-backed
// EncryptedSharedPreferences (secret_store_android.rs) — `keyring` has no
// real Android backend. See `openpal_core::crypto::secret_store`.
#[cfg(not(target_os = "android"))]
#[path = "secret_store.rs"]
mod secret_store;
#[cfg(target_os = "android")]
#[path = "secret_store_android.rs"]
mod secret_store;
mod state;
mod sync_loop;
// Android: bridges `WifiManager.MulticastLock` for `crate::remote::client`'s
// mDNS browse (`WifiLockPlugin.kt`). Everywhere else: a no-op stub — see
// wifi_lock.rs's own doc comment.
#[cfg(not(target_os = "android"))]
#[path = "wifi_lock.rs"]
mod wifi_lock;
#[cfg(target_os = "android")]
#[path = "wifi_lock_android.rs"]
mod wifi_lock;

use state::{AppState, BootState};
use std::panic::AssertUnwindSafe;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // If this launch is actually the update helper spawned by
    // `commands::install_update` (Portable SKU), finish applying the update
    // and exit — never falls through to the rest of `run()`. A no-op,
    // returning immediately, on every ordinary launch.
    portable::apply_pending_update();

    // If this launch is actually the relaunch spawned by
    // `commands::factory_reset`, wipe the data directory and the OS
    // keychain entry, then fall through into an ordinary launch (unlike
    // `apply_pending_update` above, this is a real launch, not a throwaway
    // helper — there's no `std::process::exit` here). A no-op on every
    // ordinary launch.
    factory_reset::apply_pending_reset();

    // On the Windows Portable build, point WebView2 at the bundled fixed-version
    // runtime before the webview is created. No-op on a normal build / other OS.
    portable::prepare_webview2();

    // Must happen before anything builds a `reqwest::Client` — the store's
    // 5-second post-boot update check is the first caller in practice.
    // `tauri-plugin-updater`'s `reqwest` major has no per-client fallback like
    // `openpal-core`'s does, so with no process default installed it panics
    // (fatal on Android; silently swallowed on desktop). `let _`: `Err` just
    // means something else already installed one first, which is fine.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // File logging is initialized in `BootState::initialize` (see `setup`
    // below), Phase 1 of the boot split — before the window even exists,
    // using `boot.json`'s last-known log level.
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build());
    // A plugin's own `setup()` runs during `.build()`, before the app-level
    // `.setup(|app| {...})` closure below ever fires — so `secret_store`'s
    // `PluginHandle` is guaranteed ready by the time `bootstrap::mint_or_unlock`
    // needs it there.
    #[cfg(target_os = "android")]
    let builder = builder
        .plugin(secret_store::plugin_init())
        .plugin(wifi_lock::plugin_init())
        .plugin(opener::plugin_init())
        .plugin(notify::android_plugin_init());
    builder
        .setup(|app| {
            // The whole boot sequence runs inside this closure, itself
            // called from `tao`'s `applicationDidFinishLaunching` (macOS) —
            // a callback the platform declares unable to unwind. An
            // uncaught panic anywhere below (e.g. `app_db`'s migration
            // ladder hitting a schema version it doesn't recognize) would
            // otherwise unwind straight through this closure and abort the
            // whole process the instant it reached that boundary, with no
            // window, no dialog, and nothing but a stderr backtrace most
            // users never see. `catch_unwind` stops it here instead, so
            // both a panic and an ordinary `Err` end up at the same
            // `fatal_startup_error` dialog below.
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(
                || -> Result<(), Box<dyn std::error::Error>> {
                    // Before anything the window can call: the notification
                    // commands answer pre-unlock, and `get_app_info` reports
                    // whether this build has a backend at all.
                    app.manage(notify::select(app.handle()));

                    // Phase 1, pre-unlock, no crypto: paths, the instance
                    // lock, file logging (+ panic hook), and the window —
                    // see `BootState`'s doc comment. A failure here has
                    // nowhere to log to yet, same as before this split.
                    let boot = BootState::initialize(app.handle())?;
                    create_main_window(app)?;
                    app.manage(boot);
                    let boot = app.state::<BootState>();

                    // Phase 2, unlock: skipped entirely in portable mode
                    // (never touches the host keychain), same as before
                    // this split.
                    let dek = if boot.paths.portable {
                        None
                    } else {
                        match bootstrap::mint_or_unlock(
                            secret_store::store().as_ref(),
                            &boot.paths.data_dir.join("keys"),
                        )? {
                            bootstrap::KeyStatus::Ready(dek) => Some(dek),
                            bootstrap::KeyStatus::NeedsRecovery => {
                                // Don't build `AppState` yet — the
                                // frontend's `RecoveryUnlock` gate (driven
                                // by `get_app_info`, which only ever needs
                                // `BootState`) takes over from here, and
                                // `commands::attempt_recovery_unlock` calls
                                // `state::finish_unlock` itself on success.
                                boot.needs_recovery
                                    .store(true, std::sync::atomic::Ordering::Relaxed);
                                return Ok(());
                            }
                        }
                    };
                    state::finish_unlock(app.handle(), &boot, dek)?;
                    Ok(())
                },
            ));

            match outcome {
                Ok(Ok(())) => Ok(()),
                Ok(Err(err)) => {
                    tracing::error!("startup failed: {err}");
                    fatal_startup_error();
                }
                Err(_panic) => {
                    // Already logged: `logging::install_panic_hook` (live
                    // from `BootState::initialize` onward) writes the
                    // location and backtrace via `tracing::error!` before
                    // this unwind ever reaches `catch_unwind`.
                    fatal_startup_error();
                }
            }
        })
        // Every command below except `get_app_info`, `generate_recovery_code`,
        // `attempt_recovery_unlock`, `get_app_lock_status`, `verify_app_lock_pin`,
        // `set_app_lock_pin`, `unlock_app_lock_with_recovery_code`,
        // `notification_permission`, `request_notification_permission`,
        // `open_notification_settings` and `take_notification_click` (they only
        // ask the OS, send the user to it, or hand back a click), and
        // `factory_reset` (see their own doc comments) takes
        // `State<'_, AppState>`, unmanaged until `state::finish_unlock`
        // succeeds. That alone is the pre-unlock gate for the other ~94 —
        // deliberately audited once, here, rather than per-command: Tauri's
        // `State<T>` extractor already rejects a command with a catchable
        // error (not a panic) when `T` isn't managed yet, confirmed against
        // the vendored `tauri` source (`State::from_command`). Don't add a
        // manual "is it unlocked" check to a new command on the strength of
        // this comment alone if it does anything portable mode's `dek: None`
        // can't tolerate — check `AppState.dek`'s doc comment first.
        .invoke_handler(tauri::generate_handler![
            commands::get_system_report,
            commands::get_app_info,
            commands::get_models,
            commands::download_model,
            commands::cancel_download,
            commands::probe_disk_speed,
            commands::delete_model,
            commands::list_hf_models,
            commands::add_hf_model,
            commands::backfill_custom_model_metadata,
            commands::search_hf_hub,
            commands::get_hf_model_details,
            commands::get_hf_model_readme,
            commands::get_hf_quant_picker,
            commands::add_local_model,
            commands::pick_attachments,
            commands::start_engine,
            commands::stop_engine,
            commands::get_engine_status,
            commands::notification_permission,
            commands::request_notification_permission,
            commands::open_notification_settings,
            commands::take_notification_click,
            commands::notify_native,
            commands::snooze_auto_unload,
            commands::list_cloud_models,
            commands::chat_stream,
            commands::stop_generation,
            commands::translate_stream,
            commands::correct_stream,
            commands::enhance_prompt,
            commands::save_writing_tool_result,
            commands::classify_message,
            commands::log_triage_action,
            commands::classify_risk,
            commands::speak_text,
            commands::stop_speaking,
            commands::transcribe_speech,
            commands::stop_dictation,
            commands::list_voices,
            commands::download_voice,
            commands::preview_voice,
            commands::list_plugins,
            commands::set_plugin_enabled,
            commands::respond_tool_consent,
            commands::respond_clarifying_question,
            commands::list_tool_grants,
            commands::revoke_tool_grant,
            commands::list_conversations,
            commands::get_conversation,
            commands::save_conversation,
            commands::delete_conversation,
            commands::search_conversations,
            commands::get_artifact,
            commands::download_artifact,
            commands::list_projects,
            commands::save_project,
            commands::delete_project,
            commands::set_conversation_project,
            commands::set_conversation_title,
            commands::set_conversation_pinned,
            commands::list_skills,
            commands::save_skill,
            commands::delete_skill,
            commands::import_skill_file,
            commands::import_skill_folder,
            commands::import_skill_zip,
            commands::export_skill,
            commands::get_settings,
            commands::update_settings,
            commands::set_model_favorite,
            commands::get_parental_status,
            commands::get_parental_settings,
            commands::verify_parental_pin,
            commands::set_parental_controls,
            commands::get_account,
            commands::refresh_account,
            commands::sign_in,
            commands::cancel_sign_in,
            commands::sign_out,
            commands::generate_recovery_code,
            commands::confirm_recovery_code,
            commands::attempt_recovery_unlock,
            commands::get_app_lock_status,
            commands::verify_app_lock_pin,
            commands::set_app_lock_pin,
            commands::unlock_app_lock_with_recovery_code,
            commands::factory_reset,
            commands::check_for_update,
            commands::install_update,
            commands::check_engine_update,
            commands::install_engine_update,
            commands::open_models_folder,
            commands::open_link,
            commands::fetch_favicon,
            commands::copy_text,
            commands::get_recent_logs,
            commands::export_logs,
            commands::clear_logs,
            commands::export_data,
            commands::import_data,
            commands::sync_group_create,
            commands::sync_group_secret,
            commands::sync_pair_listen,
            commands::sync_pair_confirm,
            commands::sync_pair_request,
            commands::sync_pair_finish,
            commands::sync_pair_cancel,
            commands::sync_device_list,
            commands::sync_device_remove,
            commands::frontend_log,
            commands::get_remote_access_status,
            commands::set_remote_access_enabled,
            commands::start_pairing_session,
            commands::list_paired_devices,
            commands::revoke_paired_device,
            commands::rename_paired_device,
            commands::get_wol_status,
            commands::remote_discover_hosts,
            commands::remote_stop_discovery,
            commands::remote_pair,
            commands::remote_client_status,
            commands::remote_forget,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build OpenPal")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                // Spawned children (the engine, plugin services) must never
                // outlive the app.
                if let Some(state) = app_handle.try_state::<AppState>() {
                    tauri::async_runtime::block_on(async {
                        state.engine.stop().await.ok();
                        state.plugins.stop_all().await;
                        state.remote.stop_all().await;
                    });
                }
            }
        });
}

/// Builds the "main" window from its `tauri.conf.json` entry (marked
/// `"create": false` there so this is the only place it gets created),
/// adding a navigation guard the declarative config can't express: block the
/// webview from ever navigating itself to a non-app origin.
///
/// Without this, a real `<a href>` pointing at an external URL — which
/// rendered markdown no longer emits (`ui/src/components/Markdown.tsx` keeps
/// the address out of `href` until the user confirms), but any other markup
/// could — lets the webview's own native "Open Link" context-menu item (or a
/// drag-and-drop) navigate the app's single, addressless window to that URL in place,
/// bypassing `commands::open_link`'s http(s)-only allowlist entirely (that
/// allowlist only ever runs when the link is opened through our `onClick`
/// handler, not through webview-native navigation UI). External links are
/// meant to always open in the system browser via `open_link`, which shells
/// out to the OS opener and never navigates this webview at all.
fn create_main_window(app: &tauri::App) -> tauri::Result<()> {
    let window_config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")
        .expect("tauri.conf.json must declare the \"main\" window")
        .clone();

    tauri::webview::WebviewWindowBuilder::from_config(app.handle(), &window_config)?
        .on_navigation(|url| match url.scheme() {
            "tauri" => true, // production custom protocol (macOS/Linux)
            "http" => {
                url.host_str() == Some("tauri.localhost") // production (Windows)
                    || (cfg!(debug_assertions) && url.host_str() == Some("localhost"))
                // vite dev server
            }
            _ => false,
        })
        .build()?;
    Ok(())
}

/// Last resort for a `setup()` failure (a returned `Err`, or a caught
/// panic — see `run()`'s `setup` hook): show a generic native error dialog
/// and exit, so a broken launch is a message the user actually sees instead
/// of a silent process death. Uses `rfd` directly rather than
/// `tauri_plugin_dialog` — its `blocking_show` can't run on the main thread
/// pre-run-loop, and its non-blocking `show` needs the event loop already
/// pumping, neither of which is true this early in `setup()`; `rfd` talks
/// to the native dialog API (NSAlert / MessageBoxW / GTK) directly,
/// independent of any event loop.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn fatal_startup_error() -> ! {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("OpenPal")
        .set_description(
            "OpenPal ran into a problem and couldn't start.\n\n\
             Please try restarting the app. If the problem continues, check the log files \
             (in the app's logs folder) or reinstall OpenPal.",
        )
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
    std::process::exit(1);
}

/// Mobile has no `rfd` backend (see the `Cargo.toml` dependency comment) and
/// nothing else that can show a native dialog this early in `setup()`, before
/// the event loop pumps — log the failure and exit rather than crash silently.
#[cfg(any(target_os = "android", target_os = "ios"))]
fn fatal_startup_error() -> ! {
    tracing::error!("OpenPal ran into a problem and couldn't start.");
    std::process::exit(1);
}
