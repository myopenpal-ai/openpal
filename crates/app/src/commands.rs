//! Tauri commands — a thin, friendly layer over `openpal-core`.
//!
//! Rules for this file:
//! - No business logic here; that lives in core. Commands validate, call core,
//!   and translate errors into sentences a non-technical user can act on.
//! - Long-running commands (downloads, chat) stream progress through
//!   `tauri::ipc::Channel` and stay cancellable via shared `AtomicBool`s held
//!   in [`AppState`].

use crate::agent_bridge;
use crate::logging;
use crate::notify::{Notification, NotificationClick, NotificationPermission, Notifications};
use crate::parental;
use crate::state::{AppState, BootState};
use openpal_core::account::{Account, AccountEvent};
use openpal_core::agent::consent::ConsentDecision;
use openpal_core::agent::ToolGrant;
use openpal_core::cloud::CloudModelInfo;
use openpal_core::engine::{self, STARTUP_TIMEOUT};
use openpal_core::plugin::{PluginEvent, PluginInfo, PluginManager};
use openpal_core::remote::client::{Browser, DiscoveredHost, RemotePairingRecord};
use openpal_core::remote::{PairedDeviceInfo, PairingEvent, RemoteAccessStatusInfo, WolStatus};
use openpal_core::types::{
    AppLockPinCheckResult, AppLockState, AppLockStatusView, Artifact, Attachment, ChatBackend,
    ChatEvent, ChatMessage, Conversation, ConversationMeta, ConversationSearchResult,
    DiskSpeedInfo, DownloadEvent, EngineConfig, EngineStatus, HfListing, HfModelDetails,
    HfQuantOption, HfQuantPicker, HfSearchPage, HfSearchQuery, ModelInfo, ModelSource, ModelSpec,
    ModelStatus, ParentalConfigInput, ParentalSettingsView, ParentalStatus, PinCheckResult,
    Project, RiskSignal, SamplingParams, Settings, Skill, SystemReport, TaskType, VoiceInfo,
};
use openpal_core::{
    custom, disk_speed, download, huggingface, projects, prompt_enhancer, selection, skills, store,
    writing_tools, Error as CoreError,
};
use rand::rngs::OsRng;
use rand::RngCore;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::ipc::Channel;
use tauri::Manager;
use tauri::State;

const GIB: u64 = 1024 * 1024 * 1024;

fn friendly(err: CoreError) -> String {
    match err {
        CoreError::Cancelled => "Cancelled.".to_string(),
        CoreError::Http(e) => {
            tracing::warn!("http error: {e}");
            "Couldn't reach the internet. Check your connection and try again.".to_string()
        }
        CoreError::Io(e) => {
            tracing::warn!("io error: {e}");
            "Something went wrong while saving files on your Mac. Check that you have free \
             disk space and try again."
                .to_string()
        }
        CoreError::Json(e) => {
            tracing::warn!("json error: {e}");
            "Something went wrong inside OpenPal. Please try again.".to_string()
        }
        CoreError::Db(e) => {
            tracing::warn!("database error: {e}");
            "Something went wrong with your local data. Check that you have free disk space \
             and try again."
                .to_string()
        }
        // Engine/Download/Update/AuthRequired/NotEntitled/Cloud/Import/Export/
        // DataImport/Rejected messages are written to be safe to show as-is. NotFound is
        // the one exception left on this catch-all path — its "not found: {0}"
        // Display prefix means it's only ever meant for slug-lookup misses
        // that should be unreachable from the UI (an invalid/unknown slug),
        // never a message meant to read naturally on its own; see
        // `Error::Import` for a variant that doesn't add that prefix.
        other => other.to_string(),
    }
}

/// For failures the user can't act on (e.g. a background task panicking):
/// log the technical details, show a generic sentence.
fn internal(err: impl std::fmt::Display) -> String {
    tracing::error!("internal error: {err}");
    "Something went wrong inside OpenPal. Please try again.".to_string()
}

/// UX guard for a premium command: `Ok(())` if the cached account
/// entitlement includes `capability`, else a friendly `Err` explaining what's
/// missing. This is enforcement-in-depth for UX only — it prevents a doomed
/// round trip, it does **not** secure anything, since this client (and this
/// very check) is open source. The real gate is the backend refusing the
/// request server-side (see `openpal_core::cloud`, `openpal_core::account`).
async fn require_entitlement(state: &AppState, capability: &str) -> Result<(), String> {
    if state.account.has(capability).await {
        return Ok(());
    }
    Err(if state.account.cached().await.is_some() {
        "Your plan doesn't include cloud models yet.".to_string()
    } else {
        "Sign in to use cloud models.".to_string()
    })
}

/// This device's sync key + the current data mailbox id — everything a
/// `openpal_core::sync::capture::*_synced` call needs beyond `state.
/// sync_clock`, resolved fresh on every call (mirrors `PairingManager`'s own
/// "re-fetch from the keychain on demand, never cache" discipline). `None`
/// when this device hasn't created/joined a sync group yet — a caller in
/// that state falls back to the plain, unsynced save/delete, same as when
/// `Settings::sync_enabled` is off. This can legitimately happen even with
/// `sync_enabled: true` (e.g. a stale/manually-edited settings blob), so
/// it's handled as a graceful fallback rather than a command error.
struct SyncDeps {
    sync_key: openpal_core::crypto::sync_key::SyncKey,
    mailbox_id: String,
}

fn sync_deps(state: &AppState) -> Option<SyncDeps> {
    let sync_key = state.pairing.load_sync_key().ok()?;
    let group_secret = state.pairing.load_group_secret().ok()?;
    let gen_n = openpal_core::sync::pairing::next_gen_n(&state.app_db.lock().unwrap());
    let mailbox_id = openpal_core::sync::pairing::data_mailbox_id(&group_secret, gen_n);
    Some(SyncDeps {
        sync_key,
        mailbox_id,
    })
}

/// `custom::save`, routed through the sync-capture path when sync is
/// enabled and a sync group already exists, otherwise the plain save —
/// every "Advanced settings: add your own model" / adopt-into-custom call
/// site in this file shares this one branch. `async` (unlike
/// [`save_settings`] below) because `CustomModels` carries no
/// `sync_enabled` flag of its own, so this has to read it off
/// `state.settings` itself; every call site below calls this either before
/// ever touching `state.settings` or after that lock has already been
/// dropped, so this never re-enters an already-held lock.
async fn save_custom_models(state: &AppState, custom: &custom::CustomModels) -> Result<(), String> {
    if state.settings.lock().await.sync_enabled {
        if let Some(deps) = sync_deps(state) {
            let mut conn = state.app_db.lock().unwrap();
            return openpal_core::sync::capture::save_custom_models_synced(
                &mut conn,
                custom,
                &state.sync_clock,
                &deps.sync_key,
                &deps.mailbox_id,
            )
            .map_err(friendly);
        }
    }
    custom::save(&state.app_db.lock().unwrap(), custom).map_err(friendly)
}

/// `openpal_core::settings::save`, routed through the sync-capture path
/// when `settings.sync_enabled` and a sync group already exists, otherwise
/// the plain save. Deliberately reads `sync_enabled` straight off `settings`
/// itself rather than re-locking `state.settings` — every call site already
/// holds that lock as `settings` (a `tokio::sync::MutexGuard`, which derefs
/// to `&Settings` just fine here), and `tokio::sync::Mutex` isn't reentrant.
fn save_settings(state: &AppState, settings: &Settings) -> Result<(), String> {
    if settings.sync_enabled {
        if let Some(deps) = sync_deps(state) {
            let mut conn = state.app_db.lock().unwrap();
            return openpal_core::sync::capture::save_settings_synced(
                &mut conn,
                settings,
                &state.sync_clock,
                &deps.sync_key,
                &deps.mailbox_id,
            )
            .map_err(friendly);
        }
    }
    openpal_core::settings::save(&state.app_db.lock().unwrap(), settings).map_err(friendly)
}

// ---------------------------------------------------------------------------
// System
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_system_report(state: State<'_, AppState>) -> Result<SystemReport, String> {
    let models_dir = state.paths.models_dir.clone();
    // Fresh detection: free disk space changes while the app runs. The GPU
    // doesn't — carry over the startup snapshot's engine-probed GPU fields,
    // which a plain re-detect can't fill (it has no engine binary to ask).
    let mut hardware =
        tokio::task::spawn_blocking(move || openpal_core::hardware::detect(&models_dir))
            .await
            .map_err(internal)?;
    hardware.gpu_name = state.hardware.gpu_name.clone();
    hardware.gpu_vram_bytes = state.hardware.gpu_vram_bytes;
    let disk_offload = state.settings.lock().await.disk_offload;
    let recommendation = selection::recommend(&state.catalog, &hardware, disk_offload);
    Ok(SystemReport {
        hardware,
        recommendation,
    })
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/// A model the user can act on: a curated catalog entry or one they added
/// themselves in Advanced settings.
async fn resolve_spec(state: &AppState, model_id: &str) -> Result<ModelSpec, String> {
    if let Some(spec) = state.catalog.get(model_id) {
        return Ok(spec.clone());
    }
    state
        .custom
        .lock()
        .await
        .get(model_id)
        .cloned()
        .ok_or_else(|| "That model isn't in your library.".to_string())
}

/// Where the model's weights live (or will live once downloaded): local-file
/// models run from wherever the user keeps them; everything else from the
/// writable models directory, falling back to the read-only `models/` folder a
/// "Portable + Model" build ships beside the app (so a pre-staged GGUF runs even
/// on read-only media). When the file is nowhere yet, this is the *download
/// target* — always the writable models dir, never the read-only bundle.
///
/// For a split GGUF (`spec.extra_shards` non-empty), this is shard 1's path —
/// exactly the `-m` argument llama-server needs to auto-open the rest, as
/// long as `download_model` put every shard in this same directory (see
/// [`required_model_paths`] for the full file list a split model needs).
fn model_file_path(state: &AppState, spec: &ModelSpec) -> std::path::PathBuf {
    if let Some(local) = &spec.local_path {
        return local.clone();
    }
    let primary = state.paths.models_dir.join(&spec.file_name);
    if primary.exists() {
        return primary;
    }
    if let Some(bundled) = bundled_model_path(state, &spec.file_name) {
        return bundled;
    }
    primary
}

/// A read-only pre-staged file (`<bundle>/models/<name>`) if a "Portable +
/// Model" build shipped one and it exists. `None` for normal installs.
fn bundled_model_path(state: &AppState, name: &str) -> Option<std::path::PathBuf> {
    let path = state.paths.bundled_models_dir.as_ref()?.join(name);
    path.exists().then_some(path)
}

/// Where a vision model's multimodal projector lives once present — checking the
/// writable models dir first, then the read-only pre-staged bundle. `None` for
/// text-only models, and for a vision model whose projector isn't present yet
/// (so it still starts, just text-only, rather than failing to load).
fn mmproj_path(state: &AppState, spec: &ModelSpec) -> Option<std::path::PathBuf> {
    let name = spec.mmproj_file_name.as_ref()?;
    let path = state.paths.models_dir.join(name);
    if path.exists() {
        return Some(path);
    }
    bundled_model_path(state, name)
}

/// Every file `spec` needs to actually run: shard 1 (from [`model_file_path`])
/// plus every `extra_shards` entry, each resolved the same way (writable
/// models dir, falling back to the read-only Portable bundle). The vision
/// projector is deliberately excluded — a model missing it still runs,
/// text-only, so it must never gate "Installed" the way a missing shard does.
fn required_model_paths(state: &AppState, spec: &ModelSpec) -> Vec<std::path::PathBuf> {
    let mut paths = vec![model_file_path(state, spec)];
    for shard in &spec.extra_shards {
        let primary = state.paths.models_dir.join(&shard.file_name);
        paths.push(match bundled_model_path(state, &shard.file_name) {
            Some(bundled) if !primary.exists() => bundled,
            _ => primary,
        });
    }
    paths
}

/// "Installed" only when every required file exists — a model missing shard 2
/// can't actually run llama-server's auto-shard-discovery, so it must not
/// read as ready. Otherwise sums whatever bytes exist across every file
/// (complete shards' real sizes, plus one in-progress `.part`'s partial
/// bytes) so the "resume" UI reflects real progress across the whole model,
/// not just shard 1.
fn model_status(paths: &[std::path::PathBuf]) -> ModelStatus {
    let mut total_size = 0u64;
    let mut received = 0u64;
    let mut all_installed = true;
    for path in paths {
        match std::fs::metadata(path) {
            Ok(meta) => {
                total_size += meta.len();
                received += meta.len();
            }
            Err(_) => {
                all_installed = false;
                received += download::partial_bytes(path);
            }
        }
    }
    if all_installed {
        ModelStatus::Installed {
            path: paths[0].clone(),
            size_bytes: total_size,
        }
    } else if received > 0 {
        ModelStatus::PartiallyDownloaded {
            received_bytes: received,
        }
    } else {
        ModelStatus::NotInstalled
    }
}

/// Deterministic custom-model id for a catalog model adopted by [`get_models`]
/// below — lets a later call recognize "already adopted" without a separate
/// persisted marker (catalog ids never start with "adopted-").
fn adopted_id(catalog_id: &str) -> String {
    format!("adopted-{catalog_id}")
}

#[tauri::command]
pub async fn get_models(state: State<'_, AppState>) -> Result<Vec<ModelInfo>, String> {
    let models_dir = state.paths.models_dir.clone();
    let hardware = tokio::task::spawn_blocking({
        let models_dir = models_dir.clone();
        move || openpal_core::hardware::detect(&models_dir)
    })
    .await
    .map_err(internal)?;

    let settings_guard = state.settings.lock().await;
    let disk_offload = settings_guard.disk_offload;
    let favorites: std::collections::HashSet<String> =
        settings_guard.favorite_model_ids.iter().cloned().collect();
    drop(settings_guard);
    let recommendation = selection::recommend(&state.catalog, &hardware, disk_offload);

    let mut custom = state.custom.lock().await;
    let mut newly_adopted = false;
    let mut infos: Vec<ModelInfo> = Vec::new();

    for spec in state.catalog.visible() {
        if !spec.curated {
            if custom.get(&adopted_id(&spec.id)).is_some() {
                continue; // superseded by its adopted entry, listed below
            }
            let status = model_status(&required_model_paths(&state, spec));
            if matches!(
                status,
                ModelStatus::Installed { .. } | ModelStatus::PartiallyDownloaded { .. }
            ) {
                // Downloaded before this model was decurated (or through an
                // older build's full catalog grid): adopt it into "your own
                // models" so it stays reachable now that only curated
                // catalog picks list here directly. Keeps the exact same
                // url/fileName/sha256/extraShards, so the already-downloaded
                // (or in-progress) file is found at the same path — nothing
                // is re-downloaded or moved.
                let mut adopted = spec.clone();
                adopted.id = adopted_id(&spec.id);
                adopted.source = ModelSource::HuggingFace;
                custom.add(adopted);
                newly_adopted = true;
                continue;
            }
        }

        let (fit, why) = selection::fit_for(spec, &hardware, disk_offload);
        infos.push(ModelInfo {
            status: model_status(&required_model_paths(&state, spec)),
            fit,
            why,
            is_recommended: recommendation.primary_model_id.as_deref() == Some(&spec.id),
            favorite: favorites.contains(&spec.id),
            spec: spec.clone(),
        });
    }

    if newly_adopted {
        save_custom_models(&state, &custom).await?;
    }

    // The user's own additions (Discover, Advanced settings, and any catalog
    // model adopted above) list after the catalog and are never "our pick"
    // — we can't vouch for them the same way.
    for spec in custom.all() {
        let (fit, why) = selection::fit_for(spec, &hardware, disk_offload);
        infos.push(ModelInfo {
            status: model_status(&required_model_paths(&state, spec)),
            fit,
            why,
            is_recommended: false,
            favorite: favorites.contains(&spec.id),
            spec: spec.clone(),
        });
    }
    Ok(infos)
}

/// One file to download as part of installing a model: shard 1, one of
/// `extra_shards`, or the trailing vision projector.
struct DownloadPart {
    url: String,
    dest: std::path::PathBuf,
    expected_bytes: Option<u64>,
    expected_sha256: Option<String>,
    /// Every shard is required — a model missing one can't run at all, so its
    /// failure hard-fails the whole download. The trailing projector (if any)
    /// is optional: the model still runs, text-only, so its failure
    /// soft-degrades instead (see the loop below).
    required: bool,
}

/// Every file `spec` needs downloaded, in order: shard 1, then every extra
/// shard, then (for vision models) the projector. Pure and filesystem-free
/// so it's directly unit-testable without a full `AppState`.
fn build_download_parts(models_dir: &std::path::Path, spec: &ModelSpec) -> Vec<DownloadPart> {
    let mut parts = vec![DownloadPart {
        url: spec.url.clone(),
        dest: models_dir.join(&spec.file_name),
        expected_bytes: Some(spec.size_bytes),
        expected_sha256: spec.sha256.clone(),
        required: true,
    }];
    for shard in &spec.extra_shards {
        parts.push(DownloadPart {
            url: shard.url.clone(),
            dest: models_dir.join(&shard.file_name),
            expected_bytes: Some(shard.size_bytes),
            expected_sha256: shard.sha256.clone(),
            required: true,
        });
    }
    if let (Some(url), Some(name)) = (&spec.mmproj_url, &spec.mmproj_file_name) {
        parts.push(DownloadPart {
            url: url.clone(),
            dest: models_dir.join(name),
            expected_bytes: spec.mmproj_size_bytes,
            expected_sha256: spec.mmproj_sha256.clone(),
            required: false,
        });
    }
    parts
}

#[tauri::command]
pub async fn download_model(
    state: State<'_, AppState>,
    model_id: String,
    on_event: Channel<DownloadEvent>,
) -> Result<(), String> {
    let spec = resolve_spec(&state, &model_id).await?;
    if spec.source == ModelSource::LocalFile {
        return Err(
            "That model is already on your Mac, so there's nothing to download.".to_string(),
        );
    }

    // Shard 1, then every extra shard, then (for vision models) the
    // projector — downloaded in that order, in one combined progress bar.
    let parts = build_download_parts(&state.paths.models_dir, &spec);

    // Disk space check across every part, with a message that says exactly
    // what to do. A file already complete on disk needs nothing — re-running
    // to fetch only a missing projector (or a missing shard) mustn't demand
    // room for the whole model again.
    let still_needed: u64 = parts
        .iter()
        .filter(|p| !p.dest.exists())
        .map(|p| {
            p.expected_bytes
                .unwrap_or(0)
                .saturating_sub(download::partial_bytes(&p.dest))
        })
        .sum();
    let models_dir = state.paths.models_dir.clone();
    let hardware = tokio::task::spawn_blocking(move || openpal_core::hardware::detect(&models_dir))
        .await
        .map_err(internal)?;
    if hardware.free_disk_bytes < still_needed + GIB {
        let need_gb = (still_needed + GIB).div_ceil(GIB);
        let msg = format!("Not enough free disk space. Free up about {need_gb} GB and try again.");
        let _ = on_event.send(DownloadEvent::Failed {
            message: msg.clone(),
        });
        return Err(msg);
    }

    // One download per model at a time.
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut downloads = state.downloads.lock().await;
        if downloads.contains_key(&model_id) {
            return Err("This model is already downloading.".to_string());
        }
        downloads.insert(model_id.clone(), cancel.clone());
    }

    // Combined progress: report a single bar over the total of every part.
    let combined_total: u64 = parts.iter().map(|p| p.expected_bytes.unwrap_or(0)).sum();

    let mut offset = 0u64;
    let mut result: std::result::Result<(), CoreError> = Ok(());
    for (i, part) in parts.iter().enumerate() {
        let is_first = i == 0;
        let is_last = i == parts.len() - 1;
        let part_offset = offset;
        let part_result = download::download_file(
            &state.http,
            &part.url,
            &part.dest,
            part.expected_bytes,
            part.expected_sha256.as_deref(),
            cancel.clone(),
            |event| {
                let event = match event {
                    // The combined `Started` only goes out once, for the
                    // first part — every later part's own `Started` is
                    // redundant (the UI already has a total and a start).
                    DownloadEvent::Started {
                        resumed_from_bytes, ..
                    } => {
                        if !is_first {
                            return;
                        }
                        DownloadEvent::Started {
                            total_bytes: combined_total,
                            resumed_from_bytes,
                        }
                    }
                    DownloadEvent::Progress {
                        received_bytes,
                        bytes_per_sec,
                        eta_seconds,
                        ..
                    } => DownloadEvent::Progress {
                        received_bytes: part_offset + received_bytes,
                        total_bytes: combined_total,
                        bytes_per_sec,
                        eta_seconds,
                    },
                    // Only the very last part's `Finished` reaches the UI —
                    // every earlier part just means "on to the next file".
                    DownloadEvent::Finished => {
                        if !is_last {
                            return;
                        }
                        DownloadEvent::Finished
                    }
                    // An optional part's failure soft-degrades (handled after
                    // the call returns) rather than surfacing as a hard error.
                    DownloadEvent::Failed { .. } if !part.required => return,
                    other => other, // Failed (required) / Cancelled pass through
                };
                let _ = on_event.send(event);
            },
        )
        .await;

        offset += part.expected_bytes.unwrap_or(0);

        match part_result {
            Ok(()) => continue,
            // A user-cancelled download stays a cancellation regardless of
            // which part it interrupted — never soft-degraded into "done".
            Err(CoreError::Cancelled) => {
                result = Err(CoreError::Cancelled);
                break;
            }
            // The projector failed but every required shard is installed:
            // the model still runs (text-only), so report success rather
            // than leaving an installed model looking like a failed
            // download. Vision is restored by downloading again, which
            // fast-skips the already-present shards.
            Err(e) if !part.required => {
                tracing::warn!("projector download failed; model is usable text-only: {e}");
                if is_last {
                    let _ = on_event.send(DownloadEvent::Finished);
                }
                break;
            }
            Err(e) => {
                result = Err(e);
                break;
            }
        }
    }

    state.downloads.lock().await.remove(&model_id);

    match result {
        Ok(()) => {
            // First model the user installs becomes the selected one.
            let mut settings = state.settings.lock().await;
            if settings.selected_model_id.is_none() {
                settings.selected_model_id = Some(model_id);
                // The download itself succeeded (Finished was already sent);
                // a failed settings save must not make it look like it didn't.
                if let Err(e) = save_settings(&state, &settings) {
                    tracing::warn!("model installed, but saving settings failed: {e}");
                }
            }
            Ok(())
        }
        Err(CoreError::Cancelled) => Ok(()),
        Err(err) => Err(friendly(err)),
    }
}

#[tauri::command]
pub async fn cancel_download(state: State<'_, AppState>, model_id: String) -> Result<(), String> {
    if let Some(cancel) = state.downloads.lock().await.get(&model_id) {
        cancel.store(true, Ordering::SeqCst);
    }
    Ok(())
}

/// The models volume's write speed, probed once per run and reused after
/// that: `state.paths.models_dir` is fixed for the whole life of the process
/// (set once, in `state::finish_unlock`), so re-measuring it on every download
/// confirmation and every disk-slow engine start was pure waste — and worse,
/// let two probes race on the same disk I/O and corrupt each other's
/// measurement. Holding the lock across the blocking probe itself serializes
/// any concurrent callers instead: the second caller simply waits for the
/// first probe's result rather than starting a competing one. A failed probe
/// is *not* cached, so a transient error (e.g. disk momentarily full) doesn't
/// wedge every future call — it just tries again next time.
async fn cached_volume_speed(state: &AppState) -> Result<DiskSpeedInfo, String> {
    let mut cache = state.disk_speed_cache.lock().await;
    if let Some(info) = &*cache {
        return Ok(info.clone());
    }
    let dir = state.paths.models_dir.clone();
    let info = tokio::task::spawn_blocking(move || disk_speed::probe_volume_speed(&dir))
        .await
        .map_err(internal)?
        .map_err(friendly)?;
    *cache = Some(info.clone());
    Ok(info)
}

/// Measure the model install volume's speed and estimate this model's tok/s
/// if it streamed from disk — shown in the "run from disk" download
/// confirmation dialog (`docs/design/DISK_OFFLOAD.md` Phase 3) so the user decides
/// with an honest number instead of just "it'll be slow".
#[tauri::command]
pub async fn probe_disk_speed(
    state: State<'_, AppState>,
    model_id: String,
) -> Result<DiskSpeedInfo, String> {
    let spec = resolve_spec(&state, &model_id).await?;
    let mut info = cached_volume_speed(&state).await?;
    if let Some((estimate, calibrated)) = disk_speed::estimated_tokens_per_sec(
        info.bytes_per_sec,
        &spec,
        state.hardware.total_ram_bytes,
    ) {
        info.estimated_tokens_per_sec = Some(estimate);
        info.calibrated = calibrated;
    }
    Ok(info)
}

#[tauri::command]
pub async fn delete_model(state: State<'_, AppState>, model_id: String) -> Result<(), String> {
    let spec = resolve_spec(&state, &model_id).await?;

    // A model pre-staged in the read-only bundle (Portable + Model) lives outside
    // the writable models dir, so there's nothing we can remove — say so instead
    // of silently doing nothing and leaving it shown as Installed.
    if spec.source != ModelSource::LocalFile
        && !state.paths.models_dir.join(&spec.file_name).exists()
        && bundled_model_path(&state, &spec.file_name).is_some()
    {
        return Err("This model ships with the app and can't be removed.".to_string());
    }

    // Never delete a model out from under a running (or starting) engine.
    match state.engine.status().await {
        EngineStatus::Running {
            model_id: active, ..
        }
        | EngineStatus::Starting {
            model_id: active, ..
        } if active == model_id => {
            state.engine.stop().await.map_err(friendly)?;
        }
        _ => {}
    }

    // Local-file models are the user's own files, used in place — removing
    // one only takes it off the list. Everything we downloaded, we delete.
    if spec.source != ModelSource::LocalFile {
        // Shard 1, every extra shard, plus (for vision models) the projector
        // — and any of their leftover `.part` from an interrupted download.
        let mut files = vec![state.paths.models_dir.join(&spec.file_name)];
        for shard in &spec.extra_shards {
            files.push(state.paths.models_dir.join(&shard.file_name));
        }
        if let Some(name) = &spec.mmproj_file_name {
            files.push(state.paths.models_dir.join(name));
        }
        for file in files {
            for p in [file.clone(), file.with_extension("gguf.part")] {
                if p.exists() {
                    std::fs::remove_file(&p).map_err(|e| {
                        tracing::error!("deleting {} failed: {e}", p.display());
                        "Couldn't remove that model from your Mac. Please try again.".to_string()
                    })?;
                }
            }
        }
    }

    if spec.source != ModelSource::Catalog {
        let mut custom = state.custom.lock().await;
        custom.remove(&model_id);
        save_custom_models(&state, &custom).await?;
    }

    let mut settings = state.settings.lock().await;
    let mut settings_changed = false;
    if settings.selected_model_id.as_deref() == Some(model_id.as_str()) {
        settings.selected_model_id = None;
        settings_changed = true;
    }
    if settings.default_model_id.as_deref() == Some(model_id.as_str()) {
        settings.default_model_id = None;
        settings_changed = true;
    }
    if settings_changed {
        save_settings(&state, &settings)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Custom models (Advanced settings: add your own)
// ---------------------------------------------------------------------------

/// List the single-file GGUFs behind whatever the user pasted (a Hugging Face
/// link or "owner/repo" id). The webview has no network access, so this is
/// the only road to Hugging Face.
#[tauri::command]
pub async fn list_hf_models(
    state: State<'_, AppState>,
    query: String,
) -> Result<HfListing, String> {
    let hf_ref = huggingface::parse_input(&query).map_err(friendly)?;
    cached_hf_listing(&state, &hf_ref).await
}

/// How long a cached Hugging Face repo listing/details response stays fresh
/// before a call re-fetches it — matches HF's 5-minute anonymous rate-limit
/// window, so a user idly browsing back and forth stays well clear of it.
const HF_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// Cached wrapper around `huggingface::list_gguf_files`, shared by every
/// display-only listing path (`list_hf_models`, `get_hf_quant_picker`).
/// `add_hf_model` deliberately bypasses this and calls `list_gguf_files`
/// directly — it must re-verify size/sha256 against Hugging Face right
/// before a download starts, never a cache that could be minutes stale.
///
/// Holding the cache's lock across the fetch (same trick as
/// `cached_volume_speed`) makes concurrent misses single-flight: whichever
/// caller reaches the lock first does the real request, and anyone else
/// waiting behind it gets the fresh cache entry instead of firing a second
/// identical request at Hugging Face. A failed fetch is never cached, so a
/// transient error doesn't wedge future calls.
async fn cached_hf_listing(
    state: &AppState,
    hf_ref: &huggingface::HfRef,
) -> Result<HfListing, String> {
    let key = (hf_ref.repo_id.clone(), hf_ref.revision.clone());
    let mut cache = state.hf_listing_cache.lock().await;
    let cached = match cache.get(&key) {
        Some((fetched_at, listing)) if fetched_at.elapsed() < HF_CACHE_TTL => listing.clone(),
        _ => {
            let listing = huggingface::list_gguf_files(&state.http, hf_ref)
                .await
                .map_err(friendly)?;
            cache.insert(key, (std::time::Instant::now(), listing.clone()));
            listing
        }
    };
    // `preselect_path` depends on *this* call's requested file (from the
    // pasted link), not just the repo/revision the cache is keyed on — an
    // earlier caller (e.g. the quant picker, which never wants a
    // preselection) may have cached a listing built from a different
    // `file_path`. Recompute it against the current `hf_ref` rather than
    // trusting whatever the cached struct happens to carry.
    let preselect_path = hf_ref.file_path.clone().filter(|wanted| {
        cached
            .files
            .iter()
            .any(|f| &f.path == wanted || f.extra_shards.iter().any(|s| &s.path == wanted))
    });
    Ok(HfListing {
        preselect_path,
        ..cached
    })
}

/// Cached wrapper around `huggingface::model_details`; see `cached_hf_listing`
/// for the single-flight/TTL rationale. Shared by `get_hf_model_details` and
/// `get_hf_quant_picker`, which both need the same repo's metadata for every
/// Discover model-page open — without this, opening one page sent two
/// identical `/api/models/{repo}` requests.
async fn cached_hf_details(state: &AppState, repo_id: &str) -> Result<HfModelDetails, String> {
    let mut cache = state.hf_details_cache.lock().await;
    if let Some((fetched_at, details)) = cache.get(repo_id) {
        if fetched_at.elapsed() < HF_CACHE_TTL {
            return Ok(details.clone());
        }
    }
    let details = huggingface::model_details(&state.http, repo_id)
        .await
        .map_err(friendly)?;
    cache.insert(
        repo_id.to_string(),
        (std::time::Instant::now(), details.clone()),
    );
    Ok(details)
}

/// Add one Hugging Face GGUF to the user's library. Returns the new model id.
/// Size and checksum are re-fetched from Hugging Face here — never trusted
/// from the UI.
#[tauri::command]
pub async fn add_hf_model(
    state: State<'_, AppState>,
    repo_id: String,
    revision: String,
    file_path: String,
) -> Result<String, String> {
    let hf_ref = huggingface::HfRef {
        repo_id,
        revision,
        file_path: Some(file_path.clone()),
    };
    let listing = huggingface::list_gguf_files(&state.http, &hf_ref)
        .await
        .map_err(friendly)?;
    let file = listing
        .files
        .iter()
        .find(|f| f.path == file_path)
        .ok_or_else(|| {
            "That file isn't part of this Hugging Face model anymore. Search again to see \
             what it offers now."
                .to_string()
        })?;

    let url = huggingface::resolve_url(&listing.repo_id, &listing.revision, &file.path);
    let mut custom = state.custom.lock().await;
    // Adding the same file twice just returns the existing entry.
    if let Some(existing) = custom.all().iter().find(|m| m.url == url) {
        return Ok(existing.id.clone());
    }

    // Real MoE-ness / exact parameter counts, independently re-derived here
    // rather than trusted from whatever the quant picker cached client-side
    // — same "never trust the UI" posture as the size/sha256 re-fetch above.
    // Best-effort: on any failure `hf_spec` falls back to its usual estimate.
    let shape = huggingface::fetch_gguf_header_meta(
        &state.http,
        &listing.repo_id,
        &listing.revision,
        &file.path,
    )
    .await
    .ok()
    .map(|meta| meta.model_shape());

    // Real license + release date, best-effort — same cached fetch the
    // Discover model page already made for this repo (`get_hf_model_details`),
    // so adding a model rarely fires a fresh request. A failure here must
    // never block adding the model; `hf_spec` falls back to its usual
    // placeholder/unknown copy when this is `None`.
    let details = cached_hf_details(&state, &listing.repo_id).await.ok();

    let taken: Vec<String> = state
        .catalog
        .all()
        .iter()
        .chain(custom.all())
        .map(|m| m.id.clone())
        .collect();
    let taken: Vec<&str> = taken.iter().map(String::as_str).collect();
    let id = custom.add(custom::hf_spec(
        &listing.repo_id,
        &listing.revision,
        file,
        shape,
        &taken,
        details.as_ref(),
    ));
    save_custom_models(&state, &custom).await?;
    Ok(id)
}

/// One-time, best-effort backfill for Hugging Face custom models added
/// before OpenPal tracked `groupId`/a real `license`/`releaseDate`
/// (`add_hf_model` above always fills these in for anything added since) —
/// re-fetches each one's model-card metadata and updates it in place.
///
/// A model qualifies when its `groupId` is unset — the same signal
/// `add_hf_model` always sets going forward, so this is naturally
/// idempotent (a second call is a no-op) without any schema-version
/// tracking. Catalog models "adopted" into custom (see `adopted_id` in
/// [`get_models`]) are excluded: they already carry the catalog's own
/// verified license/date and must not be overwritten by a re-fetch.
/// Per-model failures (parsing the stored URL, a network hiccup) just skip
/// that one model rather than failing the whole batch — same posture as
/// `add_hf_model`'s own best-effort detail fetch. Returns how many models
/// were actually updated.
#[tauri::command]
pub async fn backfill_custom_model_metadata(state: State<'_, AppState>) -> Result<u32, String> {
    let mut custom = state.custom.lock().await;
    let mut updated = 0u32;

    for spec in custom.all_mut() {
        if spec.source != ModelSource::HuggingFace
            || spec.group_id.is_some()
            || spec.id.starts_with("adopted-")
        {
            continue;
        }
        let Ok(hf_ref) = huggingface::parse_input(&spec.url) else {
            continue;
        };
        let Ok(details) = huggingface::model_details(&state.http, &hf_ref.repo_id).await else {
            continue;
        };
        spec.group_id = Some(hf_ref.repo_id);
        if let Some(license) = details.license {
            spec.license = license;
        }
        spec.release_date = details.created_at;
        updated += 1;
    }

    if updated > 0 {
        save_custom_models(&state, &custom).await?;
        tracing::info!(updated, "custom model metadata backfill complete");
    }
    Ok(updated)
}

// ---------------------------------------------------------------------------
// Hugging Face hub browser (search / trending / model card)
//
// Gated in the UI by the python-less "hf-browser" plugin's `model-browser`
// capability (see plugins/hf-browser/plugin.json) — but, like every other HF
// command, these are read-only proxies to public Hugging Face endpoints, so
// they aren't plugin-gated here too; see docs/design/HF_BROWSER.md design decision 2.
// ---------------------------------------------------------------------------

/// Search (or, with an empty query, browse trending) GGUF repos on the hub.
/// Each result is decorated with a rough "will this fit my machine" hint
/// (see `decorate_hf_summary`) before the user has opened a single repo.
#[tauri::command]
pub async fn search_hf_hub(
    state: State<'_, AppState>,
    query: HfSearchQuery,
) -> Result<HfSearchPage, String> {
    let mut page = huggingface::search_models(&state.http, &query)
        .await
        .map_err(friendly)?;
    let disk_offload = state.settings.lock().await.disk_offload;
    // The startup `state.hardware` snapshot, not a fresh detect: `fit_for`'s
    // RAM math doesn't consult free disk space, and this runs on every
    // keystroke/scroll, so re-detecting per call would be wasted work (see
    // `get_hf_quant_picker`'s doc comment for where a fresh detect *is*
    // needed instead).
    for model in &mut page.models {
        decorate_hf_summary(model, &state.hardware, disk_offload);
    }
    Ok(page)
}

/// Best-effort "will this fit my machine" hint for a search-result card,
/// filled in only when the hub could parse the repo's parameter count
/// (`gguf.total`). Reuses `decorate_hf_file` against a synthetic file sized
/// for a representative mid-range quant — the real per-file sizes only
/// appear once the user opens the repo (`get_hf_quant_picker`) — so the copy
/// is explicit about being a rough estimate. See docs/design/HF_BROWSER.md
/// decision 6.
const TYPICAL_QUANT: &str = "Q4_K_M";

fn decorate_hf_summary(
    model: &mut openpal_core::types::HfModelSummary,
    hardware: &openpal_core::types::HardwareInfo,
    disk_offload: bool,
) {
    let Some(total_params) = model.gguf.as_ref().and_then(|g| g.total_params) else {
        return;
    };
    let params_b = total_params as f32 / 1e9;
    let synthetic_file = openpal_core::types::HfFile {
        path: format!("{}.gguf", model.repo_id),
        size_bytes: huggingface::estimate_size_bytes(params_b, TYPICAL_QUANT),
        quant: TYPICAL_QUANT.to_string(),
        sha256: None,
        extra_shards: Vec::new(),
    };
    let decorated = decorate_hf_file(
        &model.repo_id,
        "main",
        &synthetic_file,
        Some(params_b),
        None,
        hardware,
        disk_offload,
    );
    model.fit = Some(decorated.fit);
    model.fit_why = Some(format!(
        "{} Rough estimate for a typical file size. Open the model to see exact options.",
        decorated.why
    ));
}

/// One repo's model-card metadata (downloads, likes, license, GGUF
/// architecture/context length when the hub could parse it).
#[tauri::command]
pub async fn get_hf_model_details(
    state: State<'_, AppState>,
    repo_id: String,
) -> Result<HfModelDetails, String> {
    cached_hf_details(&state, &repo_id).await
}

/// A repo's README (model card body), frontmatter stripped.
#[tauri::command]
pub async fn get_hf_model_readme(
    state: State<'_, AppState>,
    repo_id: String,
    revision: String,
) -> Result<String, String> {
    huggingface::fetch_readme(&state.http, &repo_id, &revision)
        .await
        .map_err(friendly)
}

/// The repo's selectable files/quants, each decorated with a "will it fit
/// your machine" verdict. Reuses `custom::hf_spec` to build a transient
/// (never persisted) spec per file — same construction `add_hf_model` uses —
/// preferring the hub's own parameter count (`gguf.total`, when the detail
/// lookup succeeds) over the file-size heuristic `hf_spec` falls back to on
/// its own. If a repo has no files OpenPal can use (gated, no GGUF, an
/// incomplete split-shard set), this fails with the same friendly error
/// `list_hf_models` would.
#[tauri::command]
pub async fn get_hf_quant_picker(
    state: State<'_, AppState>,
    repo_id: String,
    revision: String,
) -> Result<HfQuantPicker, String> {
    let hf_ref = huggingface::HfRef {
        repo_id: repo_id.clone(),
        revision: revision.clone(),
        file_path: None,
    };
    let listing = cached_hf_listing(&state, &hf_ref).await?;

    // Best-effort: a real parameter count beats the size-based estimate, but
    // the picker must still work if this secondary lookup hiccups. Cached —
    // the Discover model page also calls `get_hf_model_details` for this same
    // repo_id at the same time, so this reuses that fetch instead of firing
    // a second identical request at Hugging Face.
    let known_params_b = cached_hf_details(&state, &repo_id)
        .await
        .ok()
        .and_then(|d| d.gguf)
        .and_then(|g| g.total_params)
        .map(|p| p as f32 / 1e9);

    // Real MoE-ness / exact parameter counts, from parsing the smallest
    // file's own GGUF header — shared across every quant in the repo, so one
    // fetch decorates every option below. Best-effort: a network hiccup or
    // an unusual file just leaves every option at the estimate above,
    // exactly like before this detection existed.
    let shape = match listing.files.iter().min_by_key(|f| f.total_size_bytes()) {
        Some(smallest) => {
            cached_gguf_meta(&state, &listing.repo_id, &listing.revision, &smallest.path)
                .await
                .map(|meta| meta.model_shape())
        }
        None => None,
    };

    // Fresh detection, not the startup `state.hardware` snapshot: free disk
    // space changes while the app runs and feeds fit_for (get_models does the
    // same for the exact same reason).
    let models_dir = state.paths.models_dir.clone();
    let hardware = tokio::task::spawn_blocking(move || openpal_core::hardware::detect(&models_dir))
        .await
        .map_err(internal)?;
    let disk_offload = state.settings.lock().await.disk_offload;

    let options = listing
        .files
        .iter()
        .map(|file| {
            decorate_hf_file(
                &listing.repo_id,
                &listing.revision,
                file,
                known_params_b,
                shape,
                &hardware,
                disk_offload,
            )
        })
        .collect();

    Ok(HfQuantPicker {
        repo_id: listing.repo_id,
        revision: listing.revision,
        options,
        preselect_path: listing.preselect_path,
    })
}

/// Cached wrapper around `huggingface::fetch_gguf_header_meta`; see
/// `cached_hf_listing` for the single-flight/TTL rationale. Returns `None` on
/// any failure (offline, malformed, still truncated at the largest range
/// tried) — this is always a best-effort enhancement layered over an
/// estimate that already works, never something a caller can't proceed
/// without.
async fn cached_gguf_meta(
    state: &AppState,
    repo_id: &str,
    revision: &str,
    file_path: &str,
) -> Option<openpal_core::gguf_meta::GgufMeta> {
    let key = (
        repo_id.to_string(),
        revision.to_string(),
        file_path.to_string(),
    );
    let mut cache = state.gguf_meta_cache.lock().await;
    if let Some((fetched_at, meta)) = cache.get(&key) {
        if fetched_at.elapsed() < HF_CACHE_TTL {
            return Some(meta.clone());
        }
    }
    let meta = huggingface::fetch_gguf_header_meta(&state.http, repo_id, revision, file_path)
        .await
        .ok()?;
    cache.insert(key, (std::time::Instant::now(), meta.clone()));
    Some(meta)
}

/// Decorate one Hugging Face file with a fit verdict against `hardware`.
/// `known_params_b` (the hub's own `gguf.total` parameter count) and `shape`
/// (real moe/active-params from parsing the repo's own GGUF header — see
/// `cached_gguf_meta`) are both best-effort; `shape` wins when both are
/// present since it's strictly more precise. Pure — the actual command above
/// supplies the network- and state-derived inputs.
fn decorate_hf_file(
    repo_id: &str,
    revision: &str,
    file: &openpal_core::types::HfFile,
    known_params_b: Option<f32>,
    shape: Option<(f32, bool, Option<f32>)>,
    hardware: &openpal_core::types::HardwareInfo,
    disk_offload: bool,
) -> HfQuantOption {
    let precise = shape.is_some();
    let hf_spec_shape = shape.or_else(|| known_params_b.map(|params_b| (params_b, false, None)));
    // Transient, never persisted — just needs a fit verdict, so no model-card
    // details lookup here (the picker doesn't show license/release date).
    let spec = custom::hf_spec(repo_id, revision, file, hf_spec_shape, &[], None);
    let (fit, why) = selection::fit_for(&spec, hardware, disk_offload);
    HfQuantOption {
        file: file.clone(),
        fit,
        why,
        params_b: spec.params_b,
        moe: spec.moe,
        active_params_b: spec.active_params_b,
        precise,
    }
}

/// Let the user pick a .gguf already on their disk and add it to the library,
/// used in place (never copied). Returns the new model id, or None if they
/// closed the picker without choosing.
#[tauri::command]
pub async fn add_local_model(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Choose a GGUF model file")
        .add_filter("GGUF model", &["gguf"])
        .pick_file(move |file| {
            let _ = tx.send(file);
        });
    let Some(picked) = rx.await.map_err(internal)? else {
        return Ok(None); // user closed the picker
    };
    let path = picked.into_path().map_err(internal)?;

    let size_bytes = tokio::task::spawn_blocking({
        let path = path.clone();
        move || custom::validate_gguf(&path)
    })
    .await
    .map_err(internal)?
    .map_err(friendly)?;

    let mut custom_models = state.custom.lock().await;
    // Picking the same file twice just returns the existing entry.
    if let Some(existing) = custom_models
        .all()
        .iter()
        .find(|m| m.local_path.as_deref() == Some(path.as_path()))
    {
        return Ok(Some(existing.id.clone()));
    }

    let taken: Vec<String> = state
        .catalog
        .all()
        .iter()
        .chain(custom_models.all())
        .map(|m| m.id.clone())
        .collect();
    let taken: Vec<&str> = taken.iter().map(String::as_str).collect();
    let id = custom_models.add(custom::local_spec(&path, size_bytes, &taken));
    save_custom_models(&state, &custom_models).await?;
    Ok(Some(id))
}

/// Open a native picker and turn the chosen files into chat attachments:
/// images become `data:` URIs (only offered when `allow_images` — i.e. the
/// current model is a vision model) and documents (PDF / text / code) become
/// extracted text the engine folds into the prompt. Returns the attachments
/// that could be read; `Ok(vec![])` means the user closed the picker. Returns
/// an error only when files were chosen but none could be read, so the UI can
/// say why.
#[tauri::command]
pub async fn pick_attachments(
    app: tauri::AppHandle,
    allow_images: bool,
) -> Result<Vec<Attachment>, String> {
    use tauri_plugin_dialog::DialogExt;

    // Text/code documents work with any model; images are added only for
    // vision models (`allow_images`).
    const DOC_EXTS: &[&str] = &[
        "pdf", "txt", "md", "markdown", "csv", "tsv", "json", "log", "rtf", "rs", "py", "js", "ts",
        "tsx", "jsx", "html", "css", "scss", "yaml", "yml", "toml", "xml", "sh",
    ];
    let mut exts: Vec<&str> = DOC_EXTS.to_vec();
    if allow_images {
        exts.extend_from_slice(openpal_core::attachments::IMAGE_EXTS);
    }

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title(if allow_images {
            "Add images or documents"
        } else {
            "Add documents"
        })
        .add_filter("Supported files", &exts)
        .pick_files(move |files| {
            let _ = tx.send(files);
        });
    let Some(picked) = rx.await.map_err(internal)? else {
        return Ok(Vec::new()); // user closed the picker
    };

    let paths: Vec<std::path::PathBuf> = picked
        .into_iter()
        .filter_map(|f| f.into_path().ok())
        .collect();
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    // Reading files and extracting PDF text is blocking work — keep it off the
    // async runtime.
    let (attachments, first_error) = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let mut first_error: Option<String> = None;
        for path in paths {
            // The PDF parser can panic on some malformed files — isolate each
            // file so one bad document can't take down the whole pick.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                openpal_core::attachments::from_path(&path)
            }));
            match result {
                Ok(Ok(att)) => out.push(att),
                Ok(Err(e)) => {
                    first_error.get_or_insert(e);
                }
                Err(_) => {
                    tracing::error!("reading attachment {} panicked", path.display());
                    first_error.get_or_insert("That file couldn't be read.".to_string());
                }
            }
        }
        (out, first_error)
    })
    .await
    .map_err(internal)?;

    if attachments.is_empty() {
        return Err(first_error
            .unwrap_or_else(|| "Those files couldn't be added. Please try again.".to_string()));
    }
    Ok(attachments)
}

#[tauri::command]
pub fn open_models_folder(state: State<'_, AppState>) -> Result<(), String> {
    let dir = &state.paths.models_dir;
    let _ = std::fs::create_dir_all(dir);
    crate::opener::reveal_folder(dir).map_err(|e| {
        tracing::warn!("opening models folder failed: {e}");
        "Couldn't open that folder. Please try again.".to_string()
    })
}

/// Whether this is the no-install Portable build, and where its data lives. The
/// UI hides the built-in Plugins tab when `portable` (the Python plugins don't
/// ship in portable) and shows `dataDir` in Settings.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    portable: bool,
    data_dir: String,
    version: String,
    /// Whether the recovery-code ceremony (`generate_recovery_code` +
    /// `confirm_recovery_code`) still needs to run: never true in portable
    /// (no host keychain entry to ever lose) and never true once
    /// `recovery.wrap` exists. Drives whether the UI nags the user to set
    /// up recovery.
    needs_recovery_setup: bool,
    /// Whether the OS keychain just failed to produce the master key this
    /// launch and a recovery code needs to be typed back in
    /// (`attempt_recovery_unlock`) — distinct from `needs_recovery_setup`,
    /// which means no recovery code has ever been generated yet. See
    /// `BootState::needs_recovery` / `bootstrap::KeyStatus`.
    needs_recovery: bool,
    /// Whether the app-lock PIN (`openpal_core::applock`) is turned on.
    /// Recomputed fresh from `applock.json` on every call, same as
    /// `needs_recovery_setup` — never cached, since this must stay correct
    /// even when `AppState` doesn't exist yet.
    needs_app_unlock: bool,
    /// Compile-time platform fact, not a runtime detection — gates the
    /// Settings "Remote pairing" section (`remote_pair`/etc. only make
    /// sense where `state::finish_unlock` actually boxes `RemoteBackend`
    /// into `engine`). No `@tauri-apps/plugin-os` dependency needed for
    /// just this one flag.
    is_android: bool,
    /// Whether this build can post OS notifications at all (false on Android
    /// and iOS, which have no backend yet). Gates the "Desktop notifications"
    /// switch, its onboarding step and native delivery; what the OS currently
    /// allows is `notification_permission`'s answer, read later.
    native_notifications: bool,
}

/// Works before `AppState` exists — every field comes from `BootState` and
/// `Notifications`, both managed from the very start of `setup()`. This is
/// what lets the frontend's boot sequence check lock state before ever
/// calling a command that needs a store.
#[tauri::command]
pub fn get_app_info(
    state: State<'_, BootState>,
    notifications: State<'_, Notifications>,
) -> AppInfo {
    AppInfo {
        portable: state.paths.portable,
        data_dir: state.paths.data_dir.display().to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        needs_recovery_setup: !state.paths.portable
            && !state
                .paths
                .data_dir
                .join("keys")
                .join("recovery.wrap")
                .exists(),
        needs_recovery: state.needs_recovery.load(Ordering::Relaxed),
        needs_app_unlock: openpal_core::applock::load_config(
            &state.paths.data_dir.join("applock.json"),
        )
        .enabled,
        is_android: cfg!(target_os = "android"),
        native_notifications: notifications.supported(),
    }
}

#[tauri::command]
pub fn open_link(url: String) -> Result<(), String> {
    // Only web links — never hand arbitrary strings to the system opener.
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("That link can't be opened.".to_string());
    }
    crate::opener::open(&url).map_err(|e| {
        tracing::warn!("opening link failed: {e}");
        "Couldn't open that link in your browser. Please try again.".to_string()
    })
}

/// The favicon for a web-search source, as a `data:` URI the UI can drop
/// straight into an `<img>` — the webview can't load remote images itself
/// (CSP `img-src 'self' data:`). Best-effort: `None` means the site had no
/// usable icon, which the UI shows as a neutral fallback. Only fetches what the
/// page itself points at (never a third-party favicon service) and only over
/// public hosts — never internal/loopback addresses — keeping with OpenPal
/// being local and private.
#[tauri::command]
pub async fn fetch_favicon(url: String) -> Result<Option<String>, String> {
    Ok(openpal_core::favicon::fetch(&url).await)
}

/// Copy text to the OS clipboard. Done in Rust rather than the webview's own
/// clipboard API — that's unreliable in WebView2 (Windows) and WebKitGTK
/// (Linux), whereas this behaves the same everywhere.
#[tauri::command]
pub fn copy_text(app: tauri::AppHandle, text: String) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    app.clipboard().write_text(text).map_err(|e| {
        tracing::warn!("clipboard write failed: {e}");
        "Couldn't copy that to your clipboard. Please try again.".to_string()
    })
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// llama-server's GPU-layers flag (and its long-form alias) — must never
/// survive into a DiskSlow start's `extra_args`. `engine::spawn_server`
/// appends `extra_args` *after* `-ngl <cfg.gpu_layers>` on the command line,
/// so a per-model catalog entry that ever set its own `-ngl` would silently
/// override the `-ngl 0` safety guarantee `start_engine` relies on to avoid
/// wiring a bigger-than-RAM model into Metal (llama.cpp issue #24510). No
/// current catalog entry does this — this is defense in depth, not a fix for
/// an observed bug.
fn strip_gpu_layers_flag(args: &mut Vec<String>) {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-ngl" || args[i] == "--n-gpu-layers" {
            args.drain(i..(i + 2).min(args.len()));
        } else {
            i += 1;
        }
    }
}

/// Minimum reported GPU memory before the Vulkan expert-offload recipe is
/// worth applying: a MoE model's non-expert tensors plus its (capped) KV
/// cache come to a few GB, and llama.cpp hard-fails the load if the GPU
/// allocation doesn't fit — so err against a failed engine start. Integrated
/// GPUs reporting shared RAM can pass this bar; that's acceptable (Vulkan
/// still *copies* rather than wires, so it's never a safety problem, just
/// possibly not faster than CPU).
const MIN_VULKAN_VRAM_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// How `start_engine` should use the GPU for this start.
struct GpuPlan {
    gpu_layers: i32,
    /// Append `--cpu-moe`: hot tensors (attention, norms, shared experts,
    /// KV) to the GPU, routed experts stay CPU-side.
    cpu_moe: bool,
}

/// Decide GPU offload for an engine start. Verified ground rules
/// (docs/design/DISK_OFFLOAD.md, "Speed-up levers"):
/// - Metal (Apple Silicon) *wires* mmapped weights into a residency set, so
///   a bigger-than-RAM model must never get GPU layers there (llama.cpp
///   issue #24510) — full offload otherwise.
/// - Vulkan (Windows/Linux) *copies* GPU-assigned tensors into VRAM
///   (`buffer_from_host_ptr=false`), leaving CPU-side tensors file-backed
///   and evictable — so `-ngl 99 --cpu-moe` is safe even for a
///   bigger-than-RAM MoE model: the few-GB hot path lives in VRAM while
///   experts stream from RAM/disk. This is the single biggest speed lever
///   off-Mac (attention + KV on GPU, several-fold prompt-processing gains).
/// - Dense models on Vulkan stay CPU-only for now: offloading them safely
///   needs real does-it-fit-VRAM math, and a failed GPU allocation is a
///   failed engine start — a regression from today's working CPU path.
fn gpu_plan(
    hw: &openpal_core::types::HardwareInfo,
    spec: &ModelSpec,
    force_cpu: bool,
    disk_slow: bool,
) -> GpuPlan {
    if force_cpu {
        return GpuPlan {
            gpu_layers: 0,
            cpu_moe: false,
        };
    }
    if hw.has_metal {
        return GpuPlan {
            gpu_layers: if disk_slow { 0 } else { 99 },
            cpu_moe: false,
        };
    }
    let vulkan_moe = spec.moe
        && hw
            .gpu_vram_bytes
            .is_some_and(|vram| vram >= MIN_VULKAN_VRAM_BYTES);
    GpuPlan {
        gpu_layers: if vulkan_moe { 99 } else { 0 },
        cpu_moe: vulkan_moe,
    }
}

#[tauri::command]
pub async fn start_engine(
    state: State<'_, AppState>,
    model_id: String,
    context_length: u32,
) -> Result<(), String> {
    let spec = resolve_spec(&state, &model_id).await?;

    let binary = state.paths.engine_binary().ok_or_else(|| {
        "Part of OpenPal is missing. Reinstalling the app fixes this.".to_string()
    })?;

    let model_path = model_file_path(&state, &spec);
    if !model_path.exists() {
        return Err(match spec.source {
            ModelSource::LocalFile => {
                "That model's file has moved or was deleted. Remove it from your library, \
                 then add it again from its new home."
                    .to_string()
            }
            _ => "That model isn't downloaded yet.".to_string(),
        });
    }

    let settings = state.settings.lock().await.clone();
    // Deliberately NOT `fit_for(..., settings.disk_offload) == DiskSlow`: the
    // safety behavior below (forcing `-ngl 0`, the whole reason this exists)
    // must apply to any model whose full weights don't fit in RAM, even if
    // the user downloaded it while the toggle was on and later turned it
    // off, or it's a dense/custom model that could never be classified
    // DiskSlow to begin with. See `selection::exceeds_ram`'s doc comment.
    let disk_slow = selection::exceeds_ram(&spec, &state.hardware);

    let mut extra_args = spec.extra_args.clone();
    // When the user has turned "Show model thinking" off, suppress reasoning at
    // the source so no thinking is generated (saving the tokens). When on, we
    // leave the model's default behavior — reasoning models then stream their
    // thinking, which the UI shows as a collapsible trace. The flag is appended
    // last so it overrides any per-model default the catalog set.
    if !settings.show_thinking {
        extra_args.push("--reasoning-budget".to_string());
        extra_args.push("0".to_string());
    }
    // A model running from disk skips warmup (near-instant readiness on
    // macOS/Windows; llama.cpp's own warmup decode would otherwise page in
    // extra weights we don't need resident) and never gets GPU layers on
    // Apple Silicon: Metal wires mmapped weights into a residency set, which
    // for a bigger-than-RAM model means wiring the *entire* file and OOMing
    // (verified against llama.cpp issue #24510) — `-ngl 0` keeps every page
    // file-backed and evictable, the only combination that actually streams.
    // (On a Vulkan GPU, `gpu_plan` may still offload the hot path — that
    // backend copies instead of wiring, so it composes safely with disk
    // streaming.)
    if disk_slow {
        strip_gpu_layers_flag(&mut extra_args);
        extra_args.push("--no-warmup".to_string());
        // Both measured on the dev 16 GB M4 with glm-4.5-air (docs/
        // DISK_OFFLOAD.md, bench round 2): a 2048 logical/physical batch
        // doubles prompt-processing throughput (10.7 vs 5.0 tok/s on a
        // 2354-token prompt — each expert paged in from disk serves 4x the
        // tokens per pass), and q8_0 KV quant (which requires flash-attn)
        // halves the KV cache's claim on the page-cache headroom the
        // streamed weights live off, with no generation-speed regression.
        // Appended after the spec's own extra_args, so they win if a
        // catalog entry ever sets its own batch/KV flags.
        for arg in [
            "-b", "2048", "-ub", "2048", "-fa", "on", "-ctk", "q8_0", "-ctv", "q8_0",
        ] {
            extra_args.push(arg.to_string());
        }
    }
    let plan = gpu_plan(&state.hardware, &spec, settings.force_cpu, disk_slow);
    if plan.cpu_moe {
        extra_args.push("--cpu-moe".to_string());
    }
    // Disk-streamed models pay for every byte of KV cache out of the same
    // tight memory budget their weights are already straining; keep it
    // modest regardless of the conversation's usual context setting.
    let context_length = context_length.min(spec.context_max);
    let context_length = if disk_slow {
        context_length.min(8192)
    } else {
        context_length
    };

    let cfg = EngineConfig {
        context_length,
        gpu_layers: plan.gpu_layers,
        threads: None,
        extra_args,
        // Load the vision projector when this model has one on disk.
        mmproj_path: mmproj_path(&state, &spec),
    };

    let startup_timeout = if disk_slow {
        let measured = match cached_volume_speed(&state).await {
            Ok(info) => Some(info.bytes_per_sec.round() as u64),
            Err(e) => {
                tracing::warn!(
                    "disk speed probe failed, using the conservative default timeout: {e}"
                );
                None
            }
        };
        engine::disk_slow_startup_timeout(spec.total_size_bytes(), measured)
    } else {
        STARTUP_TIMEOUT
    };
    state
        .engine
        .start(
            &binary,
            &model_id,
            &model_path,
            &cfg,
            disk_slow,
            startup_timeout,
        )
        .await
        .map_err(friendly)?;

    // The model could have been deleted while your AI was loading it; never
    // persist a selection that points at a file that's gone.
    if !model_path.exists() {
        if let Err(e) = state.engine.stop().await {
            tracing::warn!("stopping engine after model disappeared failed: {e}");
        }
        return Err(
            "That model was removed while your AI was starting. Pick a model and try again."
                .to_string(),
        );
    }

    let mut settings = state.settings.lock().await;
    settings.selected_model_id = Some(model_id);
    // Starting a local model always means "chat locally from now on" — reset
    // out of Cloud mode here rather than relying on every call site (Library,
    // onboarding, auto-resume on boot) to remember to.
    settings.chat_backend = ChatBackend::Local;
    // Your AI did start; a failed settings save must not make it look like it didn't.
    if let Err(e) = save_settings(&state, &settings) {
        tracing::warn!("engine started, but saving settings failed: {e}");
    }
    Ok(())
}

#[tauri::command]
pub async fn stop_engine(state: State<'_, AppState>) -> Result<(), String> {
    state.engine.stop().await.map_err(friendly)
}

#[tauri::command]
pub async fn get_engine_status(state: State<'_, AppState>) -> Result<EngineStatus, String> {
    let mut status = state.engine.status().await;
    if let EngineStatus::Running { auto_unload_at, .. } = &mut status {
        if let Some(info) = state.engine.activity().await {
            let settings = state.settings.lock().await;
            *auto_unload_at = auto_unload_at_ms(&settings, &info);
        }
    }
    Ok(status)
}

/// Epoch-ms the running engine will be auto-unloaded at, or `None` if the
/// feature doesn't apply right now (disabled, disk-slow and not opted in, or
/// a reply is generating). Pure function of `Settings` + a fresh
/// `engine::ActivityInfo` snapshot.
fn auto_unload_at_ms(settings: &Settings, info: &engine::ActivityInfo) -> Option<i64> {
    if !settings.auto_unload_enabled || info.generating {
        return None;
    }
    if info.disk_slow && !settings.auto_unload_include_disk_slow {
        return None;
    }
    let limit = Duration::from_secs(u64::from(settings.auto_unload_minutes) * 60);
    let remaining = limit.saturating_sub(info.idle_for);
    let target = SystemTime::now() + remaining;
    let ms = target.duration_since(UNIX_EPOCH).ok()?.as_millis();
    i64::try_from(ms).ok()
}

/// Reset the auto-unload idle clock to now — same effect as any other
/// activity. Called from the frontend both explicitly (the pre-unload
/// warning popover's "Cancel" button) and passively (the composer
/// treating typing as activity while a model is loaded — see
/// `ui/src/views/ChatView.tsx`'s `Composer`).
#[tauri::command]
pub async fn snooze_auto_unload(state: State<'_, AppState>) -> Result<(), String> {
    state.engine.touch_activity().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Native notifications
// ---------------------------------------------------------------------------

/// Whether the OS currently lets OpenPal show notifications, without ever
/// prompting. Settings -> General shows a "blocked" line on `denied`.
///
/// Takes no `State<AppState>`, so it answers before the app is unlocked (the
/// onboarding and Settings screens ask while the store is still booting).
/// Best-effort: see `NotificationPermission::Granted`.
#[tauri::command]
pub async fn notification_permission(
    notifications: State<'_, Notifications>,
) -> Result<NotificationPermission, String> {
    Ok(notifications.permission().await)
}

/// Ask the OS to let OpenPal show notifications, and report the result. On
/// macOS this puts the system's one-time "Allow notifications?" prompt in front
/// of the user and returns once they have answered; it is called when the user
/// turns the Desktop notifications switch on. Elsewhere there is nothing to ask
/// and it reads like [`notification_permission`]. Same pre-unlock rules as
/// that command.
#[tauri::command]
pub async fn request_notification_permission(
    notifications: State<'_, Notifications>,
) -> Result<NotificationPermission, String> {
    Ok(notifications.request_permission().await)
}

/// Open the OS page where the user allows notifications for OpenPal: what the
/// "blocked" line's button in Settings -> General does, since the OS will not
/// ask a second time. Takes no `State<AppState>`, like the two commands above.
#[tauri::command]
pub async fn open_notification_settings(
    notifications: State<'_, Notifications>,
) -> Result<(), String> {
    notifications.open_settings().await.map_err(|e| {
        tracing::warn!("opening notification settings failed: {e}");
        "Couldn't open your system settings.".to_string()
    })
}

/// The notification click the UI has not acted on yet, once: what it reads
/// after the `notification-clicked` nudge, and once more when it has finished
/// booting, for a click that launched the app. Takes no `State<AppState>`, like
/// the commands above; the UI only acts on the answer once it is unlocked.
#[tauri::command]
pub fn take_notification_click(
    notifications: State<'_, Notifications>,
) -> Option<NotificationClick> {
    notifications.take_click()
}

/// The decision half of [`notify_native`], pure so it is unit-testable.
fn should_notify_native(enabled: bool, window_focused: bool) -> bool {
    enabled && !window_focused
}

/// Mirror an in-app alert as an OS notification. Best-effort: resolves `Ok`
/// even when it posts nothing (switch off, window focused, or an OS that drops
/// the notification, which we cannot see anyway); a delivery error is only
/// logged. Re-checks the switch and the window's focus here as a backstop to
/// the UI's own check, so a stale webview can never pop a notification while
/// the user is looking at OpenPal or after they turned the switch off.
///
/// Unlike the in-app alert, this ignores which conversation is open: the
/// caller decides what to send, this decides only whether the user is away.
/// Title and body are length-capped, and the body escaped where the OS reads
/// markup, in `Notifications::show`. `conversation_id` is what a click on the
/// notification opens (see `take_notification_click`).
///
/// On Android the window's focus cannot be read (tao reports it as never
/// focused), so there the UI's own check is the only one.
#[tauri::command]
pub async fn notify_native(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    notifications: State<'_, Notifications>,
    title: String,
    body: String,
    conversation_id: Option<String>,
) -> Result<(), String> {
    let enabled = state.settings.lock().await.desktop_notifications;
    let focused = app
        .get_webview_window("main")
        .and_then(|w| w.is_focused().ok())
        .unwrap_or(false);
    if should_notify_native(enabled, focused) {
        notifications.show(Notification {
            title,
            body,
            conversation_id,
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Cloud models
// ---------------------------------------------------------------------------

/// The cloud model catalog — shown to everyone, signed in or not (see
/// `openpal_core::cloud::list_models`), so a signed-out user sees what
/// they'd unlock. `chat_stream` (via `require_entitlement`) is what actually
/// gates chatting with one.
#[tauri::command]
pub async fn list_cloud_models(state: State<'_, AppState>) -> Result<Vec<CloudModelInfo>, String> {
    openpal_core::cloud::list_models(&state.http).await
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

/// One chat turn. Routing, in order:
/// 1. `settings.chat_backend == Cloud` → a signed-in account's cloud model
///    (`cloud_chat_turn`). Takes over the whole turn; `web_search` is ignored
///    (there's no local engine endpoint to search/fetch against in this mode
///    — the composer hides web search while a cloud model is selected).
/// 2. Otherwise, when `web_search` is set and the experimental `web-search`
///    add-on is on: a `run_agent_turn` loop scoped to just
///    `web_search`/`fetch_url` (`web_search_chat_turn`), starting its SearXNG
///    service if it isn't running (see `agent_bridge::build_web_search_deps`).
/// 3. Otherwise — including `web_search` on with the add-on off or its
///    service failing to start — the plain local engine. `web_search` is the
///    composer's per-conversation toggle; the add-on's own switch in Plugins
///    is the global gate, enforced here too.
///
/// Every path shares the `chat_cancel` flag, so `stop_generation` interrupts
/// whichever one is running.
#[tauri::command]
pub async fn chat_stream(
    state: State<'_, AppState>,
    conversation_id: String,
    messages: Vec<ChatMessage>,
    web_search: bool,
    agent_tools: bool,
    sampling: SamplingParams,
    on_event: Channel<ChatEvent>,
) -> Result<(), String> {
    parental::enforce_send(&state).await?;

    let settings = state.settings.lock().await.clone();

    if settings.chat_backend == ChatBackend::Cloud {
        return cloud_chat_turn(&state, &settings, messages, &sampling, on_event).await;
    }

    if agent_tools && settings.agent_mode_enabled {
        return agent_chat_turn(
            &state,
            &settings,
            &conversation_id,
            messages,
            &sampling,
            on_event,
        )
        .await;
    }

    // The composer hides its globe while the add-on is off, so this only
    // catches a race with turning it off. `web_search_chat_turn` starts the
    // search service and falls back to a plain answer itself if that fails.
    if web_search && state.plugins.capability_enabled("search").await {
        return web_search_chat_turn(
            &state,
            &settings,
            &conversation_id,
            messages,
            &sampling,
            on_event,
        )
        .await;
    }

    plain_chat(&state, &settings, messages, &sampling, on_event).await
}

/// Classify `text` (the composer's in-progress draft) into a [`TaskType`]
/// for the request-triage feature's silent sampling auto-pick and/or the
/// opt-in `smart-model-router` plugin's model-switch auto-pick — never shown in
/// the chat UI. Returns `Ok(None)` — never an error — whenever there's
/// nothing to suggest: neither `auto_pick_sampling` nor the `smart-model-router`
/// plugin is on (nothing would use the result; the plugin is experimental,
/// so it also counts as off while experimental features are), the chat
/// backend is Cloud (no local engine to ask), or the engine itself has
/// nothing useful to say right now (not running, or mid-generation — see
/// [`engine::EngineManager::classify`]). The frontend treats `None` as "no
/// suggestion this time", not an error to surface.
#[tauri::command]
pub async fn classify_message(
    state: State<'_, AppState>,
    text: String,
) -> Result<Option<TaskType>, String> {
    let settings = state.settings.lock().await.clone();
    if settings.chat_backend == ChatBackend::Cloud {
        return Ok(None);
    }
    let smart_model_router_on = state.plugins.capability_enabled("smart-model-router").await;
    if !settings.auto_pick_sampling && !smart_model_router_on {
        return Ok(None);
    }
    state.engine.classify(&text).await.map_err(friendly)
}

/// Write one line to the app's log file (`tracing::info!`) — the
/// request-triage feature's only visible trace of an auto-applied sampling
/// preset. There's intentionally no chat-UI indicator; this is it.
#[tauri::command]
pub fn log_triage_action(message: String) -> Result<(), String> {
    tracing::info!("triage: {message}");
    Ok(())
}

/// Classify whether `text` — the user's message from a just-settled chat
/// turn — expresses signs of self-harm or a severe mental-health crisis, so
/// the frontend can show the in-chat safety banner. Returns `Ok(None)` —
/// never an error — whenever there's nothing to check: `risk_detection_enabled`
/// is off, the chat backend is `Cloud` (no local engine to ask — the same
/// trade-off `classify_message` makes for request-triage), or the engine
/// itself has nothing useful to say right now (see
/// [`engine::EngineManager::classify_risk`]). The frontend treats both
/// `None` and `Some(RiskSignal::Safe)` as "no banner".
#[tauri::command]
pub async fn classify_risk(
    state: State<'_, AppState>,
    text: String,
) -> Result<Option<RiskSignal>, String> {
    let settings = state.settings.lock().await.clone();
    if !settings.risk_detection_enabled || settings.chat_backend == ChatBackend::Cloud {
        return Ok(None);
    }
    state.engine.classify_risk(&text).await.map_err(friendly)
}

/// Drive one agent-mode turn: build the per-turn `AgentDeps` from a
/// system-managed, per-conversation workspace folder + the long-lived
/// consent/audit state in `AppState`, then hand off to
/// `agent_bridge::run_turn`. Mirrors `plugin_chat_turn`'s shape (engine
/// endpoint, shared `chat_cancel`, begin/end_generation bracketing the
/// auto-unload idle timer).
///
/// The workspace is never user-configurable: it's always
/// `<data_dir>/agent-workspaces/<conversation_id>`, created on demand. Each
/// conversation gets its own folder so one agent call's files are never
/// visible to another conversation's tool calls.
async fn agent_chat_turn(
    state: &AppState,
    settings: &Settings,
    conversation_id: &str,
    mut messages: Vec<ChatMessage>,
    sampling: &SamplingParams,
    on_event: Channel<ChatEvent>,
) -> Result<(), String> {
    let safe_id: String = conversation_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if safe_id.is_empty() {
        return Err("Invalid conversation id.".to_string());
    }
    let workspace_dir = state.paths.data_dir.join("agent-workspaces").join(safe_id);
    std::fs::create_dir_all(&workspace_dir).map_err(|e| {
        let _ = on_event.send(ChatEvent::Error {
            message: format!("Couldn't prepare the agent workspace: {e}"),
        });
        format!("Couldn't prepare the agent workspace: {e}")
    })?;
    let endpoint = state
        .engine
        .endpoint()
        .await
        .ok_or_else(|| "Your AI isn't ready yet. Pick a model, then try again.".to_string())?;

    match messages.iter_mut().find(|m| m.role == "system") {
        Some(system) => system.content.push_str(agent_bridge::ASK_USER_FRAMING),
        None => messages.insert(
            0,
            ChatMessage {
                id: openpal_core::types::new_message_id(),
                role: "system".to_string(),
                content: format!(
                    "{}{}",
                    settings.system_prompt,
                    agent_bridge::ASK_USER_FRAMING
                ),
                created_at_ms: None,
                model: None,
                citations: None,
                thinking: None,
                steps: None,
                attachments: None,
                artifacts: None,
                stats: None,
                display_content: None,
            },
        ),
    }

    let deps = agent_bridge::build_deps(
        &workspace_dir,
        &state.paths.data_dir,
        state.app_db.clone(),
        conversation_id,
        state.http.clone(),
        state.agent_consent.clone(),
        state.agent_audit.clone(),
        &state.plugins,
        endpoint.clone(),
        *sampling,
    )
    .await
    .inspect_err(|e| {
        let _ = on_event.send(ChatEvent::Error { message: e.clone() });
    })?;

    let cancel = try_claim_generation_slot(state).await?;
    state.engine.begin_generation().await;

    let event_sink = on_event.clone();
    let result =
        agent_bridge::run_turn(&deps, endpoint, &messages, sampling, cancel, move |event| {
            let _ = event_sink.send(event);
        })
        .await;

    state.engine.end_generation().await;
    *state.chat_cancel.lock().await = None;
    result
}

#[tauri::command]
pub async fn respond_tool_consent(
    state: State<'_, AppState>,
    request_id: String,
    decision: ConsentDecision,
    note: Option<String>,
) -> Result<(), String> {
    state
        .agent_consent
        .resolve(&request_id, decision, note)
        .await;
    Ok(())
}

#[tauri::command]
pub async fn respond_clarifying_question(
    state: State<'_, AppState>,
    request_id: String,
    answer: String,
) -> Result<(), String> {
    state
        .agent_consent
        .resolve_clarification(&request_id, answer)
        .await;
    Ok(())
}

#[tauri::command]
pub async fn list_tool_grants(state: State<'_, AppState>) -> Result<Vec<ToolGrant>, String> {
    Ok(state.agent_consent.list_grants().await)
}

#[tauri::command]
pub async fn revoke_tool_grant(
    state: State<'_, AppState>,
    grant_key: String,
) -> Result<(), String> {
    state.agent_consent.revoke_grant(&grant_key).await;
    Ok(())
}

/// Resolve the model id + access token needed for a cloud chat-completion
/// request: the `require_entitlement` UX guard, then the cached account
/// token (refreshing once if this run hasn't picked up a live one yet).
/// Pulled out of `cloud_chat_turn`'s own logic so `run_one_shot_completion`
/// can reuse it without duplicating the retry behavior — no behavior change
/// to `cloud_chat_turn` itself.
async fn resolve_cloud_access(
    state: &AppState,
    settings: &Settings,
) -> Result<(String, String), String> {
    require_entitlement(state, "cloud-models").await?;

    let model_id = settings
        .selected_cloud_model_id
        .clone()
        .ok_or_else(|| "Pick a cloud model in your model library first.".to_string())?;

    let access_token = match state.account.access_token().await {
        Some(token) => token,
        None => {
            // The cached entitlement said yes, but this run has no live
            // token yet (e.g. right after a fresh launch, before the
            // boot-time refresh completed) — try once more before giving up.
            state.account.refresh().await;
            state
                .account
                .access_token()
                .await
                .ok_or_else(|| "Sign in to use cloud models.".to_string())?
        }
    };

    Ok((model_id, access_token))
}

/// Atomically claim the app's single foreground-generation slot, or return a
/// friendly busy error if something else already holds it. Every turn-runner
/// (chat, agent, web-search, Writing Tools, and the Message improver) goes
/// through this instead of assigning `state.chat_cancel` directly, so a
/// Translate/Correct/Improve request and an ordinary chat turn started
/// concurrently can never silently clobber each other's cancel handle — the
/// loser gets a friendly error up front instead of a stuck Stop button or a
/// leaked idle-timer bracket.
async fn try_claim_generation_slot(state: &AppState) -> Result<Arc<AtomicBool>, String> {
    claim_generation_slot(&state.chat_cancel).await
}

/// The actual check-and-claim, factored out of [`try_claim_generation_slot`]
/// so it's unit-testable against a bare `Mutex` — `AppState` itself needs a
/// live Tauri app to construct, which nothing else in this file's test
/// module does either.
async fn claim_generation_slot(
    slot: &tokio::sync::Mutex<Option<Arc<AtomicBool>>>,
) -> Result<Arc<AtomicBool>, String> {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut guard = slot.lock().await;
    if guard.is_some() {
        return Err(
            "OpenPal is busy replying to a chat message right now. Try again in a moment."
                .to_string(),
        );
    }
    *guard = Some(cancel.clone());
    Ok(cancel)
}

/// Drive one chat turn against a signed-in account's cloud model. Shares
/// `chat_cancel` with the other chat paths via `try_claim_generation_slot`.
async fn cloud_chat_turn(
    state: &AppState,
    settings: &Settings,
    mut messages: Vec<ChatMessage>,
    sampling: &SamplingParams,
    on_event: Channel<ChatEvent>,
) -> Result<(), String> {
    let (model_id, access_token) = resolve_cloud_access(state, settings).await?;

    if !messages.iter().any(|m| m.role == "system") {
        messages.insert(
            0,
            ChatMessage {
                id: openpal_core::types::new_message_id(),
                role: "system".to_string(),
                content: settings.system_prompt.clone(),
                created_at_ms: None,
                model: None,
                citations: None,
                thinking: None,
                steps: None,
                attachments: None,
                artifacts: None,
                stats: None,
                display_content: None,
            },
        );
    }

    let cancel = try_claim_generation_slot(state).await?;

    let result = openpal_core::cloud::chat_stream(
        &state.http,
        &access_token,
        &model_id,
        &messages,
        sampling,
        cancel,
        |event| {
            let _ = on_event.send(event);
        },
    )
    .await;

    *state.chat_cancel.lock().await = None;
    result.map_err(friendly)
}

/// Drive one plain-chat turn with web search on: a `run_agent_turn` loop
/// scoped to just `web_search`/`fetch_url` (built by
/// `agent_bridge::build_web_search_deps`), replacing the old Python-sidecar
/// path (`plugin_chat_turn`/`plugin_chat.rs`, kept for now but no longer
/// called). Mirrors `agent_chat_turn`'s shape (per-conversation workspace
/// dir, cancel flag, begin/end_generation bracketing).
async fn web_search_chat_turn(
    state: &AppState,
    settings: &Settings,
    conversation_id: &str,
    mut messages: Vec<ChatMessage>,
    sampling: &SamplingParams,
    on_event: Channel<ChatEvent>,
) -> Result<(), String> {
    let safe_id: String = conversation_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if safe_id.is_empty() {
        return Err("Invalid conversation id.".to_string());
    }
    let workspace_dir = state.paths.data_dir.join("agent-workspaces").join(safe_id);
    std::fs::create_dir_all(&workspace_dir)
        .map_err(|e| format!("Couldn't prepare the web-search workspace: {e}"))?;

    let progress_sink = on_event.clone();
    let on_progress = move |event: ChatEvent| {
        let _ = progress_sink.send(event);
    };
    let deps = agent_bridge::build_web_search_deps(
        &workspace_dir,
        &state.paths.data_dir,
        state.http.clone(),
        state.agent_consent.clone(),
        state.agent_audit.clone(),
        &state.plugins,
        &on_progress,
    )
    .await
    .inspect_err(|e| {
        let _ = on_event.send(ChatEvent::Error { message: e.clone() });
    })?;

    let Some(deps) = deps else {
        // The web-search service couldn't be had (the add-on is still
        // setting up, or its service won't start) — same graceful fallback
        // `chat_stream` already falls into for any other reason web search
        // isn't available this turn.
        return plain_chat(state, settings, messages, sampling, on_event).await;
    };

    let endpoint = state
        .engine
        .endpoint()
        .await
        .ok_or_else(|| "Your AI isn't ready yet. Pick a model, then try again.".to_string())?;

    match messages.iter_mut().find(|m| m.role == "system") {
        Some(system) => {
            system.content.push_str(agent_bridge::WEB_SEARCH_FRAMING);
            system.content.push_str(agent_bridge::ASK_USER_FRAMING);
        }
        None => messages.insert(
            0,
            ChatMessage {
                id: openpal_core::types::new_message_id(),
                role: "system".to_string(),
                content: format!(
                    "{}{}{}",
                    settings.system_prompt,
                    agent_bridge::WEB_SEARCH_FRAMING,
                    agent_bridge::ASK_USER_FRAMING
                ),
                created_at_ms: None,
                model: None,
                citations: None,
                thinking: None,
                steps: None,
                attachments: None,
                artifacts: None,
                stats: None,
                display_content: None,
            },
        ),
    }

    let cancel = try_claim_generation_slot(state).await?;
    state.engine.begin_generation().await;

    let event_sink = on_event.clone();
    let result =
        agent_bridge::run_turn(&deps, endpoint, &messages, sampling, cancel, move |event| {
            let _ = event_sink.send(event);
        })
        .await;

    state.engine.end_generation().await;
    *state.chat_cancel.lock().await = None;
    result
}

/// Plain engine completion (no plugin).
async fn plain_chat(
    state: &AppState,
    settings: &Settings,
    mut messages: Vec<ChatMessage>,
    sampling: &SamplingParams,
    on_event: Channel<ChatEvent>,
) -> Result<(), String> {
    if !messages.iter().any(|m| m.role == "system") {
        messages.insert(
            0,
            ChatMessage {
                id: openpal_core::types::new_message_id(),
                role: "system".to_string(),
                content: settings.system_prompt.clone(),
                created_at_ms: None,
                model: None,
                citations: None,
                thinking: None,
                steps: None,
                attachments: None,
                artifacts: None,
                stats: None,
                display_content: None,
            },
        );
    }

    let cancel = try_claim_generation_slot(state).await?;
    state.engine.begin_generation().await;

    let result = state
        .engine
        .chat_stream(&messages, sampling, cancel, |event| {
            let _ = on_event.send(event);
        })
        .await;

    state.engine.end_generation().await;
    *state.chat_cancel.lock().await = None;
    result.map_err(friendly)
}

#[tauri::command]
pub async fn stop_generation(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(cancel) = state.chat_cancel.lock().await.as_ref() {
        cancel.store(true, Ordering::SeqCst);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// One-shot utility completions (Writing Tools Translate/Correct, Message
// improver)
// ---------------------------------------------------------------------------
//
// One-shot, non-conversational completions against whichever backend chat
// already uses (never a second model). No Library/conversation persistence,
// no `Settings.system_prompt` — see `openpal_core::writing_tools` (Translate/
// Correct) and `openpal_core::prompt_enhancer` (Message improver) for the
// fixed system prompts and sampling these build requests from.

fn system_message(content: String) -> ChatMessage {
    ChatMessage {
        id: openpal_core::types::new_message_id(),
        role: "system".to_string(),
        content,
        created_at_ms: None,
        model: None,
        citations: None,
        thinking: None,
        steps: None,
        attachments: None,
        artifacts: None,
        stats: None,
        display_content: None,
    }
}

fn user_message(content: String) -> ChatMessage {
    ChatMessage {
        id: openpal_core::types::new_message_id(),
        role: "user".to_string(),
        content,
        created_at_ms: None,
        model: None,
        citations: None,
        thinking: None,
        steps: None,
        attachments: None,
        artifacts: None,
        stats: None,
        display_content: None,
    }
}

/// Whether a one-shot completion counts toward the parental-controls daily
/// message cap. Translate/Correct always pass [`Self::CountMessage`]
/// (unchanged behaviour). The Message improver picks between the two at
/// request time based on whether parental controls are even on — see
/// `enhance_prompt`.
#[derive(Clone, Copy)]
enum ParentalGate {
    CountMessage,
    CheckOnly,
}

/// Shared one-shot completion path for every non-conversational,
/// user-initiated utility completion — Writing Tools' Translate/Correct and
/// the composer's Message improver. Never references `settings.system_prompt`
/// — callers pass a complete, already-built `messages` array of exactly a
/// system + user message. `gate` selects `parental::enforce_send` (counts as
/// one message) or `parental::check_send` (lock-only) — callers must
/// validate input first either way, same reasoning as before. Rejects up
/// front via `try_claim_generation_slot` if the app's single
/// foreground-generation slot is already claimed by a chat turn (or vice
/// versa — every turn-runner claims through the same helper), brackets the
/// local engine with begin/end_generation so auto-unload can't fire
/// mid-request, and clears `chat_cancel` on exit so `stop_generation`
/// cancels it like any turn. Returns `Ok(true)` when the request was
/// cancelled mid-stream (`sse::drain_sse` never turns a cancel into an
/// `Error`), so callers can tell "no usable output" apart from "the user
/// pressed Stop".
async fn run_one_shot_completion(
    state: &AppState,
    settings: &Settings,
    gate: ParentalGate,
    messages: Vec<ChatMessage>,
    sampling: &SamplingParams,
    on_event: impl FnMut(ChatEvent) + Send,
) -> Result<bool, String> {
    match gate {
        ParentalGate::CountMessage => parental::enforce_send(state).await?,
        ParentalGate::CheckOnly => parental::check_send(state).await?,
    }

    let cloud_access = if settings.chat_backend == ChatBackend::Cloud {
        Some(resolve_cloud_access(state, settings).await?)
    } else {
        None
    };

    let cancel = try_claim_generation_slot(state).await?;
    let cancel_flag = cancel.clone();

    let result = if let Some((model_id, access_token)) = cloud_access {
        openpal_core::cloud::chat_stream(
            &state.http,
            &access_token,
            &model_id,
            &messages,
            sampling,
            cancel,
            on_event,
        )
        .await
    } else {
        state.engine.begin_generation().await;
        let r = state
            .engine
            .chat_stream(&messages, sampling, cancel, on_event)
            .await;
        state.engine.end_generation().await;
        r
    };

    *state.chat_cancel.lock().await = None;
    result.map_err(friendly)?;
    Ok(cancel_flag.load(Ordering::SeqCst))
}

/// Reject a draft longer than the Message improver will offer to improve.
/// Factored out for the same reason as [`validate_writing_tool_text`] —
/// unit-testable without a live `AppState`.
fn validate_draft_length(draft: &str) -> Result<(), String> {
    if prompt_enhancer::draft_too_long(draft) {
        return Err(
            "That message is too long to improve. Send it as it is, or trim it first.".to_string(),
        );
    }
    Ok(())
}

/// Trim `text` and reject it if empty. Factored out of the one-shot utility
/// commands below so it's unit-testable without a live `AppState` (same
/// reasoning as [`claim_generation_slot`]).
fn validate_writing_tool_text(text: &str, empty_message: &str) -> Result<String, String> {
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return Err(empty_message.to_string());
    }
    Ok(trimmed)
}

/// Validate Translate mode's language selection against the curated
/// [`writing_tools::LANGUAGES`] table: an empty/absent `source_lang` means
/// "detect" (never an error), but a non-empty one that isn't in the table
/// is rejected, and `target_lang` is always required. Returns the
/// normalized (empty-filtered) source language on success.
fn validate_translate_languages<'a>(
    source_lang: Option<&'a str>,
    target_lang: &str,
) -> Result<Option<&'a str>, String> {
    let source_lang = source_lang.filter(|s| !s.is_empty());
    if let Some(id) = source_lang {
        if writing_tools::language_name(id).is_none() {
            return Err(
                "That source language isn't on OpenPal's list. Leave it on Detect, or pick one \
                 from the menu."
                    .to_string(),
            );
        }
    }
    if writing_tools::language_name(target_lang).is_none() {
        return Err("Choose a language to translate into.".to_string());
    }
    Ok(source_lang)
}

/// Stream a translation of `text` into `target_lang`, optionally naming
/// `source_lang` ("detect" when `None`/empty). Validates input before
/// `parental::enforce_send` (a blank submission or a bad language pick must
/// never burn a parental-controls message-quota slot for a request that
/// never reaches the model).
#[tauri::command]
pub async fn translate_stream(
    state: State<'_, AppState>,
    text: String,
    source_lang: Option<String>,
    target_lang: String,
    on_event: Channel<ChatEvent>,
) -> Result<(), String> {
    let text = validate_writing_tool_text(&text, "Type or paste some text to translate.")?;
    let source_lang = validate_translate_languages(source_lang.as_deref(), &target_lang)?;

    let settings = state.settings.lock().await.clone();
    let system = writing_tools::translate_system_prompt(source_lang, &target_lang);
    let messages = vec![system_message(system), user_message(text)];
    run_one_shot_completion(
        &state,
        &settings,
        ParentalGate::CountMessage,
        messages,
        &writing_tools::fixed_sampling(),
        move |event| {
            let _ = on_event.send(event);
        },
    )
    .await
    .map(|_cancelled| ())
}

/// Stream a grammar/style correction of `text`. Validates input before
/// `parental::enforce_send`, same reasoning as [`translate_stream`].
#[tauri::command]
pub async fn correct_stream(
    state: State<'_, AppState>,
    text: String,
    on_event: Channel<ChatEvent>,
) -> Result<(), String> {
    let text = validate_writing_tool_text(&text, "Type or paste some text to correct.")?;

    let settings = state.settings.lock().await.clone();
    let messages = vec![
        system_message(writing_tools::CORRECT_SYSTEM_PROMPT.to_string()),
        user_message(text),
    ];
    run_one_shot_completion(
        &state,
        &settings,
        ParentalGate::CountMessage,
        messages,
        &writing_tools::fixed_sampling(),
        move |event| {
            let _ = on_event.send(event);
        },
    )
    .await
    .map(|_cancelled| ())
}

/// Rewrite the composer draft so the model understands it better (the
/// "Improve my message" action, part of the `writing-tools` plugin
/// alongside Translate/Correct). Non-streaming on purpose: the result is
/// cleaned once in `prompt_enhancer::clean_enhanced` and either replaces the
/// draft atomically or is kept out entirely — the composer never shows a
/// half-written rewrite. Validates input before any parental check (same
/// rule as `translate_stream`); like `translate_stream`/`correct_stream`,
/// deliberately not gated by `PluginManager::capability_enabled` here — this
/// is a net-new command with no other call site, so the UI's own
/// `writing-tools`-capability check is trusted (see `plugins/writing-tools/
/// README.md`). Whether it counts toward the daily message cap depends on
/// whether parental controls are even on: refused-but-uncounted when they
/// are off (the common case — no double-charge against a cap nobody set),
/// counted like every other generation when they are on (so a capped child
/// can't use this as an unlimited, friction-free channel to the model once
/// the visible chat cap is reached).
#[tauri::command]
pub async fn enhance_prompt(
    state: State<'_, AppState>,
    text: String,
) -> Result<prompt_enhancer::EnhanceOutcome, String> {
    use prompt_enhancer::{EnhanceOutcome, KeepReason};

    let draft = validate_writing_tool_text(&text, "Type a message first.")?;
    validate_draft_length(&draft)?;

    let settings = state.settings.lock().await.clone();

    let messages = vec![
        system_message(prompt_enhancer::SYSTEM_PROMPT.to_string()),
        user_message(prompt_enhancer::user_message_text(&draft)),
    ];
    let sampling = prompt_enhancer::fixed_sampling(&draft, settings.show_thinking);

    let parental_enabled = state.parental_config.lock().await.enabled;
    let gate = if parental_enabled {
        ParentalGate::CountMessage
    } else {
        ParentalGate::CheckOnly
    };

    let mut answer = String::new();
    let mut reasoning_seen = false;
    let cancelled = run_one_shot_completion(
        &state,
        &settings,
        gate,
        messages,
        &sampling,
        |event| match event {
            ChatEvent::Token { text } => answer.push_str(&text),
            ChatEvent::Reasoning { .. } => reasoning_seen = true,
            _ => {}
        },
    )
    .await?;

    let outcome = if cancelled {
        EnhanceOutcome::Kept {
            reason: KeepReason::Cancelled,
        }
    } else {
        prompt_enhancer::clean_enhanced(&answer, &draft, reasoning_seen)
    };

    tracing::info!(
        "message improver: {} ({} -> {} chars)",
        outcome.label(),
        draft.chars().count(),
        answer.chars().count()
    );

    Ok(outcome)
}

/// Save a translated/corrected document to disk: opens a native save dialog
/// defaulting to `suggested_file_name`, then writes `text` to the chosen
/// path. Mirrors [`download_artifact`]'s dialog pattern (`Ok(false)` if the
/// dialog is closed without a choice, not an error), but wraps the write in
/// `spawn_blocking` like [`export_logs`] since a Writing Tools result can be
/// a fairly large document.
#[tauri::command]
pub async fn save_writing_tool_result(
    app: tauri::AppHandle,
    text: String,
    suggested_file_name: String,
) -> Result<bool, String> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_file_name(&suggested_file_name)
        .add_filter("Text file", &["txt"])
        .save_file(move |path| {
            let _ = tx.send(path);
        });
    let Some(picked) = rx.await.map_err(internal)? else {
        return Ok(false); // user closed the dialog
    };
    let path = picked.into_path().map_err(internal)?;
    tokio::task::spawn_blocking(move || std::fs::write(&path, text))
        .await
        .map_err(internal)?
        .map(|()| true)
        .map_err(|e| {
            tracing::warn!("writing tools result export write failed: {e}");
            "Couldn't save the file. Please try again.".to_string()
        })
}

// ---------------------------------------------------------------------------
// Speech (the Voice plugin: read aloud + voice typing)
// ---------------------------------------------------------------------------
//
// These bridge the webview to the Voice plugin's loopback service through Rust,
// because the webview's CSP can't reach the service and the microphone is opened
// natively (see `crate::audio`). Each finds the enabled, set-up plugin
// advertising the capability, makes sure its service is running, then calls it.

/// Read `text` aloud with the enabled Voice (`tts`) plugin. Resolves when
/// playback finishes; `stop_speaking` cuts it short (resolving early).
#[tauri::command]
pub async fn speak_text(state: State<'_, AppState>, text: String) -> Result<(), String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Ok(());
    }
    let Some(id) = state.plugins.active_service_plugin("tts").await else {
        return Err("Turn on Voice in Add-ons to have messages read aloud.".to_string());
    };
    let port =
        state.plugins.ensure_service(&id).await?.ok_or_else(|| {
            "Voice isn't ready yet. Open Add-ons to check it's set up.".to_string()
        })?;

    // Translate the user's friendly read-aloud Settings into Piper's synthesis
    // parameters. Speed is the inverse of Piper's `length_scale` (faster speech
    // = shorter phonemes); expressiveness scales Piper's two noise controls
    // around their defaults (0.667 and 0.8). Clamped here so a hand-edited
    // settings file can't push Piper somewhere odd — the service clamps again.
    let (voice_id, length_scale, noise_scale, noise_w, sentence_silence) = {
        let settings = state.settings.lock().await;
        let speed = settings.voice_speed.clamp(0.25, 4.0);
        let expr = settings.voice_expressiveness.clamp(0.0, 3.0);
        (
            settings.voice_id.clone(),
            1.0 / speed,
            0.667 * expr,
            0.8 * expr,
            settings.voice_sentence_pause.clamp(0.0, 5.0),
        )
    };

    tracing::info!(
        voice = %voice_id,
        chars = text.chars().count(),
        "tts: synthesizing speech"
    );

    let resp = state
        .http
        .post(format!("http://127.0.0.1:{port}/speak"))
        .json(&serde_json::json!({
            "text": text,
            "voice": voice_id,
            "length_scale": length_scale,
            "noise_scale": noise_scale,
            "noise_w": noise_w,
            "sentence_silence": sentence_silence,
        }))
        .send()
        .await
        .map_err(|e| {
            tracing::warn!("tts request failed: {e}");
            "Couldn't reach the read-aloud service. Try turning Voice off and on in Add-ons."
                .to_string()
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!("tts service returned {status}: {body}");
        // The selected voice may have been deleted (or be a stale id). Point the
        // user at Settings to re-pick instead of looping on a generic "retry".
        if body.contains("voice model not found") {
            return Err(
                "That voice isn't installed. Pick or download a voice in Settings.".to_string(),
            );
        }
        return Err("Reading that aloud didn't work. Please try again.".to_string());
    }
    let bytes = resp.bytes().await.map_err(internal)?.to_vec();

    // Play natively, and wait for it to finish (or be stopped) off the runtime.
    let done = state.audio.play(bytes);
    let _ = tokio::task::spawn_blocking(move || {
        let _ = done.recv();
    })
    .await;
    Ok(())
}

#[tauri::command]
pub async fn stop_speaking(state: State<'_, AppState>) -> Result<(), String> {
    tracing::debug!("tts: stop requested");
    state.audio.stop();
    Ok(())
}

/// Record from the microphone until `stop_dictation` is called (or a 60s safety
/// cap), transcribe it with the enabled Voice (`stt`) plugin, and return the
/// text for the composer to insert.
#[tauri::command]
pub async fn transcribe_speech(state: State<'_, AppState>) -> Result<String, String> {
    let Some(id) = state.plugins.active_service_plugin("stt").await else {
        return Err("Turn on Voice in Add-ons to type by talking.".to_string());
    };
    let port =
        state.plugins.ensure_service(&id).await?.ok_or_else(|| {
            "Voice isn't ready yet. Open Add-ons to check it's set up.".to_string()
        })?;

    tracing::info!(plugin = %id, "stt: recording from microphone");

    // Record (cancellable via stop_dictation flipping this flag).
    let stop = Arc::new(AtomicBool::new(false));
    *state.dictation_stop.lock().await = Some(stop.clone());
    let recorded = tokio::task::spawn_blocking(move || crate::audio::record_wav(stop, 60))
        .await
        .map_err(internal)?;
    *state.dictation_stop.lock().await = None;
    let wav = recorded?;

    // A bare WAV header (~44 bytes) means nothing was captured — nothing to say.
    if wav.len() <= 64 {
        tracing::debug!("stt: no speech captured");
        return Ok(String::new());
    }

    let resp = state
        .http
        .post(format!("http://127.0.0.1:{port}/transcribe"))
        .header("content-type", "audio/wav")
        .body(wav)
        .send()
        .await
        .map_err(|e| {
            tracing::warn!("stt request failed: {e}");
            "Couldn't reach the voice-typing service. Try turning Voice off and on in Add-ons."
                .to_string()
        })?;
    if !resp.status().is_success() {
        tracing::warn!("stt service returned {}", resp.status());
        return Err("Couldn't turn your speech into text. Please try again.".to_string());
    }

    #[derive(serde::Deserialize)]
    struct Transcript {
        text: String,
    }
    let t: Transcript = resp.json().await.map_err(internal)?;
    let text = t.text.trim().to_string();
    tracing::info!(chars = text.chars().count(), "stt: transcription complete");
    Ok(text)
}

#[tauri::command]
pub async fn stop_dictation(state: State<'_, AppState>) -> Result<(), String> {
    tracing::debug!("stt: stop requested");
    if let Some(stop) = state.dictation_stop.lock().await.as_ref() {
        stop.store(true, Ordering::SeqCst);
    }
    Ok(())
}

/// The running Voice (`tts`) service's port, or a friendly error when Voice
/// isn't enabled/ready. Shared by the voice-management commands below.
async fn tts_service_port(plugins: &PluginManager) -> Result<u16, String> {
    let Some(id) = plugins.active_service_plugin("tts").await else {
        return Err("Turn on Voice in Add-ons to choose a voice.".to_string());
    };
    plugins
        .ensure_service(&id)
        .await?
        .ok_or_else(|| "Voice isn't ready yet. Open Add-ons to check it's set up.".to_string())
}

#[derive(serde::Deserialize)]
struct VoicesResp {
    voices: Vec<VoiceInfo>,
}

/// List the voices the user can read with — the curated catalog plus which are
/// installed — from the Voice plugin's `/voices` endpoint.
#[tauri::command]
pub async fn list_voices(state: State<'_, AppState>) -> Result<Vec<VoiceInfo>, String> {
    let port = tts_service_port(&state.plugins).await?;
    tracing::debug!("voice: listing catalog");
    let resp = state
        .http
        .get(format!("http://127.0.0.1:{port}/voices"))
        .send()
        .await
        .map_err(|e| {
            tracing::warn!("voices request failed: {e}");
            "Couldn't reach the Voice service. Try turning Voice off and on in Add-ons.".to_string()
        })?;
    if !resp.status().is_success() {
        tracing::warn!("voices service returned {}", resp.status());
        return Err("Couldn't load the list of voices. Please try again.".to_string());
    }
    Ok(resp.json::<VoicesResp>().await.map_err(internal)?.voices)
}

/// Download a voice by id (a no-op if it's already installed), then return the
/// refreshed list. Voice models are tens of MB, so this uses a one-off client
/// without the short read-timeout the shared one carries — a slow download must
/// not be aborted mid-stream.
#[tauri::command]
pub async fn download_voice(
    state: State<'_, AppState>,
    voice_id: String,
) -> Result<Vec<VoiceInfo>, String> {
    let port = tts_service_port(&state.plugins).await?;
    tracing::info!(voice = %voice_id, "voice: downloading");
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(internal)?;
    let resp = client
        .post(format!("http://127.0.0.1:{port}/voices/download"))
        .json(&serde_json::json!({ "voice": voice_id }))
        .send()
        .await
        .map_err(|e| {
            tracing::warn!("voice download failed: {e}");
            "Couldn't download that voice. Check your internet connection and try again."
                .to_string()
        })?;
    if !resp.status().is_success() {
        tracing::warn!("voice download returned {}", resp.status());
        return Err("Couldn't download that voice. Please try again.".to_string());
    }
    tracing::info!("voice: download complete");
    Ok(resp.json::<VoicesResp>().await.map_err(internal)?.voices)
}

/// Hosts a voice preview may be fetched from: Hugging Face and its own download
/// CDNs (a `huggingface.co` `/resolve/` URL 302s to e.g. `cas-bridge.xethub.hf.co`).
/// Used to re-validate every redirect hop so a preview fetch can't be bounced to
/// an arbitrary (e.g. internal/loopback) host.
fn is_hf_host(host: &str) -> bool {
    host == "huggingface.co"
        || host == "hf.co"
        || host.ends_with(".huggingface.co")
        || host.ends_with(".hf.co")
}

/// Play a short remote sample of a voice so the user can preview it before
/// selecting or downloading. The `url` comes from the Voice plugin's `/voices`
/// listing (`previewUrl`) and points at an MP3 in the pinned piper-voices repo.
/// Playback is native (rodio), so this works for not-yet-installed voices and
/// doesn't touch the speech service. The webview hands us the URL, so it must
/// start at Hugging Face and we re-check the host on *every* redirect hop (HF
/// redirects downloads to its own CDN) — this can't be turned into a generic
/// fetcher. Resolves when the sample finishes (or `stop_speaking` cuts it).
#[tauri::command]
pub async fn preview_voice(state: State<'_, AppState>, url: String) -> Result<(), String> {
    let parsed = reqwest::Url::parse(&url)
        .map_err(|_| "No preview is available for this voice.".to_string())?;
    if parsed.scheme() != "https" || !parsed.host_str().is_some_and(is_hf_host) {
        return Err("No preview is available for this voice.".to_string());
    }

    // A dedicated client (the shared one follows redirects blindly): re-validate
    // the host on every redirect hop and bound the whole fetch in time. A hop to
    // a non-HF host — or too many hops — stops with the 3xx, which then fails the
    // is_success check below.
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let ok = attempt.url().scheme() == "https"
                && attempt.url().host_str().is_some_and(is_hf_host);
            if !ok || attempt.previous().len() >= 8 {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(internal)?;

    tracing::debug!("voice: previewing remote sample");
    let mut resp = client.get(parsed).send().await.map_err(|e| {
        tracing::warn!("voice preview fetch failed: {e}");
        "Couldn't play a preview. Check your internet connection and try again.".to_string()
    })?;
    if !resp.status().is_success() {
        tracing::warn!("voice preview returned {}", resp.status());
        return Err("Couldn't play a preview for this voice.".to_string());
    }

    // Stream with a running cap so an unexpectedly huge body is never fully
    // buffered (samples are ~100 KB); reject up front if Content-Length already
    // exceeds it.
    const CAP: usize = 16 * 1024 * 1024;
    if resp.content_length().is_some_and(|n| n > CAP as u64) {
        return Err("That preview was too large to play.".to_string());
    }
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(internal)? {
        if bytes.len() + chunk.len() > CAP {
            return Err("That preview was too large to play.".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }

    // Play natively and wait for it to finish (or be stopped). Reuses the same
    // sink as read-aloud, so starting a preview replaces whatever was playing.
    let done = state.audio.play(bytes);
    let _ = tokio::task::spawn_blocking(move || {
        let _ = done.recv();
    })
    .await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Plugins (add-ons)
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_plugins(state: State<'_, AppState>) -> Result<Vec<PluginInfo>, String> {
    Ok(state.plugins.list().await)
}

/// Enable or disable a plugin. Enabling streams setup progress (and starts the
/// plugin's service); re-enabling a failed/half-installed plugin retries.
#[tauri::command]
pub async fn set_plugin_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
    on_event: Channel<PluginEvent>,
) -> Result<(), String> {
    state
        .plugins
        .set_enabled(&id, enabled, |event| {
            let _ = on_event.send(event);
        })
        .await
}

// ---------------------------------------------------------------------------
// Conversations
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_conversations(state: State<'_, AppState>) -> Result<Vec<ConversationMeta>, String> {
    Ok(store::list(&state.app_db.lock().unwrap()))
}

#[tauri::command]
pub fn get_conversation(state: State<'_, AppState>, id: String) -> Result<Conversation, String> {
    store::get(&state.app_db.lock().unwrap(), &id).map_err(friendly)
}

#[tauri::command]
pub async fn save_conversation(
    state: State<'_, AppState>,
    conversation: Conversation,
) -> Result<(), String> {
    let sync_enabled = state.settings.lock().await.sync_enabled;
    if sync_enabled {
        if let Some(deps) = sync_deps(&state) {
            let mut conn = state.app_db.lock().unwrap();
            return openpal_core::sync::capture::save_conversation_synced(
                &mut conn,
                &conversation,
                &state.sync_clock,
                &deps.sync_key,
                &deps.mailbox_id,
            )
            .map_err(friendly);
        }
    }
    store::save(&state.app_db.lock().unwrap(), &conversation).map_err(friendly)
}

#[tauri::command]
pub async fn delete_conversation(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let sync_enabled = state.settings.lock().await.sync_enabled;
    if sync_enabled {
        if let Some(deps) = sync_deps(&state) {
            let mut conn = state.app_db.lock().unwrap();
            return openpal_core::sync::capture::delete_conversation_synced(
                &mut conn,
                &id,
                &state.sync_clock,
                &deps.sync_key,
                &deps.mailbox_id,
            )
            .map_err(friendly);
        }
    }
    store::delete(&state.app_db.lock().unwrap(), &id).map_err(friendly)
}

/// Case-insensitive search over every conversation's title and message
/// content, for the sidebar's search box. See `store::search`.
#[tauri::command]
pub fn search_conversations(
    state: State<'_, AppState>,
    query: String,
) -> Result<Vec<ConversationSearchResult>, String> {
    Ok(store::search(&state.app_db.lock().unwrap(), &query))
}

// ---------------------------------------------------------------------------
// Artifacts
// ---------------------------------------------------------------------------

/// Fetch a `create_artifact` tool call's full content, for the transcript's
/// preview panel. See `openpal_core::artifacts::get`.
#[tauri::command]
pub fn get_artifact(
    state: State<'_, AppState>,
    conversation_id: String,
    artifact_id: String,
) -> Result<Artifact, String> {
    openpal_core::artifacts::get(
        &state.app_db.lock().unwrap(),
        &conversation_id,
        &artifact_id,
    )
    .map_err(friendly)
}

/// Let the user save an artifact to disk via the native save dialog,
/// defaulting the file name to the artifact's own. `Ok(false)` means the
/// dialog was closed without a choice (not an error).
#[tauri::command]
pub async fn download_artifact(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    conversation_id: String,
    artifact_id: String,
) -> Result<bool, String> {
    use tauri_plugin_dialog::DialogExt;

    let artifact = openpal_core::artifacts::get(
        &state.app_db.lock().unwrap(),
        &conversation_id,
        &artifact_id,
    )
    .map_err(friendly)?;

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_file_name(&artifact.filename)
        .save_file(move |path| {
            let _ = tx.send(path);
        });
    let Some(picked) = rx.await.map_err(internal)? else {
        return Ok(false); // user closed the dialog
    };
    let path = picked.into_path().map_err(internal)?;
    std::fs::write(&path, artifact.content.as_bytes()).map_err(internal)?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_projects(state: State<'_, AppState>) -> Result<Vec<Project>, String> {
    Ok(projects::list(&state.app_db.lock().unwrap()))
}

/// Create (new id from the frontend, same convention as
/// `save_conversation`) or rename (existing id) a project.
#[tauri::command]
pub async fn save_project(state: State<'_, AppState>, project: Project) -> Result<(), String> {
    let sync_enabled = state.settings.lock().await.sync_enabled;
    if sync_enabled {
        if let Some(deps) = sync_deps(&state) {
            let mut conn = state.app_db.lock().unwrap();
            return openpal_core::sync::capture::save_project_synced(
                &mut conn,
                &project,
                &state.sync_clock,
                &deps.sync_key,
                &deps.mailbox_id,
            )
            .map_err(friendly);
        }
    }
    projects::save(&state.app_db.lock().unwrap(), &project).map_err(friendly)
}

/// Delete a project and unassign every conversation that was in it — the
/// conversations themselves are untouched, only ungrouped.
#[tauri::command]
pub async fn delete_project(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let sync_enabled = state.settings.lock().await.sync_enabled;
    if sync_enabled {
        if let Some(deps) = sync_deps(&state) {
            let mut conn = state.app_db.lock().unwrap();
            return openpal_core::sync::capture::delete_project_synced(
                &mut conn,
                &id,
                &state.sync_clock,
                &deps.sync_key,
                &deps.mailbox_id,
            )
            .map_err(friendly);
        }
    }
    projects::delete(&state.app_db.lock().unwrap(), &id).map_err(friendly)
}

/// Move a conversation into `project_id`, or out of whatever project it's in
/// when `project_id` is `None`. A read-modify-write on just this one field —
/// deliberately not a round trip through `save_conversation`, since the
/// sidebar only ever holds `ConversationMeta` (no message history) for
/// conversations other than the active one. When sync is enabled this still
/// routes through the same conversation sync-capture path as
/// `save_conversation` (a project-id-only change is exactly the
/// `project_update`/independent-LWW case `sync::conversations::apply_local`
/// already handles), not a bespoke project-id envelope.
#[tauri::command]
pub async fn set_conversation_project(
    state: State<'_, AppState>,
    conversation_id: String,
    project_id: Option<String>,
) -> Result<(), String> {
    let sync_enabled = state.settings.lock().await.sync_enabled;
    if sync_enabled {
        if let Some(deps) = sync_deps(&state) {
            let mut conn = state.app_db.lock().unwrap();
            let mut conversation = store::get(&conn, &conversation_id).map_err(friendly)?;
            conversation.project_id = project_id;
            return openpal_core::sync::capture::save_conversation_synced(
                &mut conn,
                &conversation,
                &state.sync_clock,
                &deps.sync_key,
                &deps.mailbox_id,
            )
            .map_err(friendly);
        }
    }
    let conn = state.app_db.lock().unwrap();
    let mut conversation = store::get(&conn, &conversation_id).map_err(friendly)?;
    conversation.project_id = project_id;
    store::save(&conn, &conversation).map_err(friendly)
}

/// Rename a conversation from the sidebar's row menu. A read-modify-write on
/// just the title — same reasoning as `set_conversation_project` (the
/// sidebar only holds `ConversationMeta`, not the full conversation with its
/// messages). Unlike `pinned` below, `title` already has full multi-device
/// sync support as an independent LWW register (`sync::conversations`'s
/// `conversation_title` scope), so this routes through
/// `sync::capture::save_conversation_synced` when sync is enabled, same as
/// `set_conversation_project` does for `project_id`.
#[tauri::command]
pub async fn set_conversation_title(
    state: State<'_, AppState>,
    conversation_id: String,
    title: String,
) -> Result<(), String> {
    let sync_enabled = state.settings.lock().await.sync_enabled;
    if sync_enabled {
        if let Some(deps) = sync_deps(&state) {
            let mut conn = state.app_db.lock().unwrap();
            let mut conversation = store::get(&conn, &conversation_id).map_err(friendly)?;
            conversation.title = title;
            return openpal_core::sync::capture::save_conversation_synced(
                &mut conn,
                &conversation,
                &state.sync_clock,
                &deps.sync_key,
                &deps.mailbox_id,
            )
            .map_err(friendly);
        }
    }
    let conn = state.app_db.lock().unwrap();
    let mut conversation = store::get(&conn, &conversation_id).map_err(friendly)?;
    conversation.title = title;
    store::save(&conn, &conversation).map_err(friendly)
}

/// Pin/unpin a conversation from the sidebar's row menu. `pinned` isn't a
/// synced field (see `app_db`'s `SCHEMA_V10` doc comment), so unlike
/// `set_conversation_title`/`set_conversation_project` this never routes
/// through `sync::capture` — a plain local read-modify-write regardless of
/// whether sync is on. Plain `fn`, not `async` — nothing here awaits.
#[tauri::command]
pub fn set_conversation_pinned(
    state: State<'_, AppState>,
    conversation_id: String,
    pinned: bool,
) -> Result<(), String> {
    let conn = state.app_db.lock().unwrap();
    let mut conversation = store::get(&conn, &conversation_id).map_err(friendly)?;
    conversation.pinned = pinned;
    store::save(&conn, &conversation).map_err(friendly)
}

// ---------------------------------------------------------------------------
// Skills
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_skills(state: State<'_, AppState>) -> Result<Vec<Skill>, String> {
    Ok(skills::list(&state.app_db.lock().unwrap()))
}

/// Create (empty `skill.slug`) or update (existing slug) a skill, returning
/// it with the slug filled in — a fresh save needs it back so the frontend's
/// local copy stays in sync with the generated trigger. Plain `fn`, not
/// `async` — the only thing that used to make this `async` was the now-gone
/// `skills_lock.lock().await`; slug-uniqueness is decided inside one
/// transaction under `app_db`'s own `Mutex`, which every other command
/// touching `app_db` already blocks on, so no separate lock is needed (see
/// `skills::insert_new_skill`'s doc comment).
#[tauri::command]
pub fn save_skill(state: State<'_, AppState>, skill: Skill) -> Result<Skill, String> {
    skills::save(&mut state.app_db.lock().unwrap(), skill).map_err(friendly)
}

#[tauri::command]
pub fn delete_skill(state: State<'_, AppState>, slug: String) -> Result<(), String> {
    skills::delete(&mut state.app_db.lock().unwrap(), &slug).map_err(friendly)
}

/// Let the user pick a `.md` file on disk and add it as a new skill (see
/// `skills::import`). `Ok(None)` means the picker was closed without a
/// choice. Stays `async` — unlike `save_skill`/`delete_skill`, this still
/// needs to `.await` the native picker's oneshot channel; only the DB write
/// after it is synchronous.
#[tauri::command]
pub async fn import_skill_file(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<Skill>, String> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Choose a skill file")
        .add_filter("Markdown", &["md"])
        .pick_file(move |file| {
            let _ = tx.send(file);
        });
    let Some(picked) = rx.await.map_err(internal)? else {
        return Ok(None); // user closed the picker
    };
    let path = picked.into_path().map_err(internal)?;

    skills::import(&mut state.app_db.lock().unwrap(), &path)
        .map(Some)
        .map_err(friendly)
}

/// Same as [`import_skill_file`], but for a folder containing a `SKILL.md`
/// plus any supporting Markdown files. Desktop only — `tauri-plugin-dialog`
/// has no `pick_folder` on Android/iOS (a hard compile error, not just a
/// missing feature at runtime); mobile keeps `import_skill_file`/
/// `import_skill_zip`.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tauri::command]
pub async fn import_skill_folder(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<Skill>, String> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Choose a skill folder")
        .pick_folder(move |folder| {
            let _ = tx.send(folder);
        });
    let Some(picked) = rx.await.map_err(internal)? else {
        return Ok(None); // user closed the picker
    };
    let path = picked.into_path().map_err(internal)?;

    skills::import(&mut state.app_db.lock().unwrap(), &path)
        .map(Some)
        .map_err(friendly)
}

#[cfg(any(target_os = "android", target_os = "ios"))]
#[tauri::command]
pub async fn import_skill_folder(
    _app: tauri::AppHandle,
    _state: State<'_, AppState>,
) -> Result<Option<Skill>, String> {
    Err("Importing a skill folder isn't available on mobile yet. Try a .zip instead.".to_string())
}

/// Same as [`import_skill_file`], but for a `.zip` archive of a skill folder
/// — e.g. downloaded from somewhere else. Every entry is read straight from
/// the archive into memory (see `skills::import_zip`, which caps both entry
/// count and total uncompressed size against a zip bomb) — never extracted
/// to disk, so untrusted zip content never transits an unencrypted OS temp
/// directory the way it used to.
#[tauri::command]
pub async fn import_skill_zip(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<Skill>, String> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Choose a skill .zip archive")
        .add_filter("Zip archive", &["zip"])
        .pick_file(move |file| {
            let _ = tx.send(file);
        });
    let Some(picked) = rx.await.map_err(internal)? else {
        return Ok(None); // user closed the picker
    };
    let path = picked.into_path().map_err(internal)?;

    skills::import_zip(&mut state.app_db.lock().unwrap(), &path)
        .map(Some)
        .map_err(friendly)
}

/// Save `slug` to a location the user chooses, via the native save dialog —
/// a single `.md` for a flat skill, a `.zip` (shaped exactly like what
/// `import_skill_zip` reads back) for one with supporting files. This, not
/// in-place disk editing, is the encrypted-at-rest era's round trip: export,
/// edit, import back. `Ok(false)` means the dialog was closed without a
/// choice (not an error) — mirrors [`export_data`]/[`export_logs`]'s `bool`
/// return, for the same reason (`Option<()>` and `None` both serialize to
/// JSON `null`).
#[tauri::command]
pub async fn export_skill(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    slug: String,
) -> Result<bool, String> {
    use tauri_plugin_dialog::DialogExt;

    let skill = skills::get(&state.app_db.lock().unwrap(), &slug).map_err(friendly)?;
    let (file_name, bytes) = if skill.files.is_empty() {
        (
            format!("{}.md", skill.slug),
            skills::export_markdown(&skill).into_bytes(),
        )
    } else {
        (
            format!("{}.zip", skill.slug),
            skills::export_zip_bytes(&skill).map_err(friendly)?,
        )
    };

    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file().set_file_name(&file_name);
    dialog = if skill.files.is_empty() {
        dialog.add_filter("Markdown", &["md"])
    } else {
        dialog.add_filter("Zip archive", &["zip"])
    };
    dialog.save_file(move |path| {
        let _ = tx.send(path);
    });
    let Some(picked) = rx.await.map_err(internal)? else {
        return Ok(false); // user closed the dialog
    };
    let dest = picked.into_path().map_err(internal)?;
    tokio::task::spawn_blocking(move || std::fs::write(&dest, bytes))
        .await
        .map_err(internal)?
        .map(|()| true)
        .map_err(|e| {
            tracing::warn!("skill export write failed: {e}");
            "Couldn't export that skill. Please try again.".to_string()
        })
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    Ok(state.settings.lock().await.clone())
}

#[tauri::command]
pub async fn update_settings(state: State<'_, AppState>, settings: Settings) -> Result<(), String> {
    // Reject a non-loopback sync relay URL that doesn't use https:// before
    // it's ever persisted — a plain-HTTP relay is only acceptable for
    // 127.0.0.1/localhost, see `validate_relay_url`'s own doc comment. This
    // is an Advanced-only, self-hoster-facing field: a save-time rejection
    // beats a silent warning nobody reads.
    if let Some(url) = &settings.sync_relay_url {
        openpal_core::sync::mailbox_client::validate_relay_url(url)?;
    }

    let mut current = state.settings.lock().await;
    let relay_url_changed = current.sync_relay_url != settings.sync_relay_url;
    let experimental_changed = current.experimental_features != settings.experimental_features;
    *current = settings;
    save_settings(&state, &current)?;

    if relay_url_changed {
        // Repoints the SAME `MailboxClient` the pairing flows and
        // `sync_loop`'s tick already use, live — previously this setting
        // was captured but never actually consulted (see `sync_loop.rs`'s
        // former "Known limitation" note, now resolved).
        let url = current
            .sync_relay_url
            .clone()
            .unwrap_or_else(openpal_core::sync::mailbox_client::relay_base_url);
        state.pairing.transport().set_base_url(url);
    }

    if experimental_changed {
        let experimental = current.experimental_features;
        // Don't hold the settings lock while plugin services shut down.
        drop(current);
        state.plugins.set_experimental_features(experimental);
        if !experimental {
            state.plugins.stop_experimental_services().await;
        }
    }

    Ok(())
}

/// Star/unstar a model as a favorite, from the Library page or the
/// composer's model quick-switcher. Device-local, like
/// `set_conversation_pinned` — a plain read-modify-write of
/// `Settings.favorite_model_ids` rather than a full `update_settings`
/// round-trip, so the caller only ever sends the one thing that changed.
#[tauri::command]
pub async fn set_model_favorite(
    state: State<'_, AppState>,
    model_id: String,
    favorite: bool,
) -> Result<(), String> {
    let mut settings = state.settings.lock().await;
    if favorite {
        if !settings.favorite_model_ids.contains(&model_id) {
            settings.favorite_model_ids.push(model_id);
        }
    } else {
        settings.favorite_model_ids.retain(|id| *id != model_id);
    }
    save_settings(&state, &settings)
}

// ---------------------------------------------------------------------------
// Parental controls
// ---------------------------------------------------------------------------

/// Everything the chat UI needs to show (or hide) the soft-lock banner. No
/// PIN required — this never reveals the PIN or lets anything be changed.
#[tauri::command]
pub async fn get_parental_status(state: State<'_, AppState>) -> Result<ParentalStatus, String> {
    let config = state.parental_config.lock().await.clone();
    let (today, minute) = openpal_core::parental::now_local_parts();
    let mut ps = state.parental_state.lock().await;
    if openpal_core::parental::roll_over(&mut ps, &today) {
        let _ = openpal_core::parental::save_state(&state.parental_state_path, &ps);
    }
    Ok(openpal_core::parental::status(&config, &ps, minute))
}

/// The current configuration, minus the PIN itself. No PIN required to
/// view — only [`set_parental_controls`] (changing anything) needs it.
#[tauri::command]
pub async fn get_parental_settings(
    state: State<'_, AppState>,
) -> Result<ParentalSettingsView, String> {
    Ok(ParentalSettingsView::from(
        &*state.parental_config.lock().await,
    ))
}

/// Check a PIN without changing anything — lets the Settings UI unlock the
/// edit form before the user has actually filled in new values. Rate-limited
/// via `openpal_core::parental::attempt_pin`, shared with
/// [`set_parental_controls`] so a caller can't dodge the lockout by trying
/// one command instead of the other.
#[tauri::command]
pub async fn verify_parental_pin(
    state: State<'_, AppState>,
    pin: String,
) -> Result<PinCheckResult, String> {
    let config = state.parental_config.lock().await;
    let mut ps = state.parental_state.lock().await;
    let now = openpal_core::parental::now_epoch_seconds();
    let result = openpal_core::parental::attempt_pin(&config, &mut ps, &pin, now);
    openpal_core::parental::save_state(&state.parental_state_path, &ps).map_err(friendly)?;
    Ok(result)
}

/// Create, edit, or turn off parental controls. `current_pin` must match
/// the stored PIN once one is set — including to turn the feature back off,
/// so a kid who finds the toggle can't just switch it off unaided.
/// `config.new_pin`, when given, replaces the stored PIN (required the
/// first time `config.enabled` becomes true).
#[tauri::command]
pub async fn set_parental_controls(
    state: State<'_, AppState>,
    current_pin: Option<String>,
    config: ParentalConfigInput,
) -> Result<(), String> {
    let mut current = state.parental_config.lock().await;

    if current.pin_hash.is_some() {
        let mut ps = state.parental_state.lock().await;
        let now = openpal_core::parental::now_epoch_seconds();
        let result = openpal_core::parental::attempt_pin(
            &current,
            &mut ps,
            current_pin.as_deref().unwrap_or(""),
            now,
        );
        openpal_core::parental::save_state(&state.parental_state_path, &ps).map_err(friendly)?;
        if !result.ok {
            return Err(if result.locked_for_seconds > 0 {
                format!(
                    "Too many wrong attempts. Try again in {}s.",
                    result.locked_for_seconds
                )
            } else {
                "That PIN isn't right.".to_string()
            });
        }
    }

    let (pin_hash, pin_salt) = match config.new_pin.filter(|p| !p.is_empty()) {
        Some(pin) => {
            let salt = openpal_core::parental::new_salt();
            let hash = openpal_core::parental::hash_pin(&pin, &salt);
            (Some(hash), Some(salt))
        }
        None => (current.pin_hash.clone(), current.pin_salt.clone()),
    };
    if config.enabled && pin_hash.is_none() {
        return Err("Set a PIN before turning parental controls on.".to_string());
    }

    *current = openpal_core::types::ParentalConfig {
        enabled: config.enabled,
        schedule_enabled: config.schedule_enabled,
        allowed_start: config.allowed_start,
        allowed_end: config.allowed_end,
        usage_cap_enabled: config.usage_cap_enabled,
        usage_cap_minutes: config.usage_cap_minutes,
        message_cap_enabled: config.message_cap_enabled,
        message_cap_per_day: config.message_cap_per_day,
        pin_hash,
        pin_salt,
    };
    openpal_core::parental::save_config(&state.parental_config_path, &current).map_err(friendly)
}

// ---------------------------------------------------------------------------
// Account
// ---------------------------------------------------------------------------

/// The signed-in account, if any — the local cache only, no network call.
/// `App` calls this once at boot, the same way it loads `appInfo`.
#[tauri::command]
pub async fn get_account(state: State<'_, AppState>) -> Result<Option<Account>, String> {
    Ok(state.account.cached().await)
}

/// Refresh the cached account from a stored sign-in session, if any, and
/// return the refreshed snapshot. Silent on failure (offline, no stored
/// session, expired) — returns whatever was already cached. Called a few
/// seconds after boot, the same fire-and-forget shape as `check_for_update`'s
/// auto-check.
#[tauri::command]
pub async fn refresh_account(state: State<'_, AppState>) -> Result<Option<Account>, String> {
    state.account.refresh().await;
    Ok(state.account.cached().await)
}

/// Start signing in. Streams `AccountEvent`s: `AwaitingBrowser` once a device
/// session is ready — the frontend opens `verification_url` with the existing
/// `open_link` command — then `Complete`/`Expired`/`Failed` once the user
/// finishes (or doesn't) there. `cancel_sign_in` stops an in-flight call.
#[tauri::command]
pub async fn sign_in(
    state: State<'_, AppState>,
    on_event: Channel<AccountEvent>,
) -> Result<(), String> {
    state
        .account
        .sign_in(|event| {
            let _ = on_event.send(event);
        })
        .await
}

#[tauri::command]
pub async fn cancel_sign_in(state: State<'_, AppState>) -> Result<(), String> {
    state.account.cancel_sign_in().await;
    Ok(())
}

#[tauri::command]
pub async fn sign_out(state: State<'_, AppState>) -> Result<(), String> {
    state.account.sign_out().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Recovery
//
// The second wrap-file slot — see `openpal_core::crypto::recovery`'s module
// doc. `generate_recovery_code` and `confirm_recovery_code` are meant to be
// called back-to-back as one ceremony: generate, show the code once, and
// only once the user confirms they saved it does `confirm_recovery_code`
// actually seal anything under it.
// ---------------------------------------------------------------------------

/// A fresh recovery code for the user to save. Pure generation — nothing is
/// sealed under it until the user confirms they've saved it and this same
/// code comes back through [`confirm_recovery_code`].
#[tauri::command]
pub fn generate_recovery_code() -> String {
    openpal_core::crypto::recovery::generate()
}

/// Seals a second copy of the data-encryption key under `code` — the
/// recovery code the user just confirmed they saved — writing
/// `<data>/keys/recovery.wrap` alongside the existing keychain-sealed slot.
/// Unlike that slot, this one runs a real KDF (`openpal_core::crypto::kdf`,
/// at its recommended parameters): a recovery code, unlike the OS keychain,
/// has no access control of its own.
#[tauri::command]
pub fn confirm_recovery_code(state: State<'_, AppState>, code: String) -> Result<(), String> {
    use openpal_core::crypto::{kdf, recovery, wrap};

    // The DEK is already sitting on `AppState` — the app unlocked via the
    // keychain earlier this same launch (see `state::finish_unlock`) and
    // this command only ever runs post-unlock, so there's no need to go
    // back to the keychain and re-derive it a second time.
    let dek = state.dek.as_ref().ok_or_else(|| {
        tracing::error!(
            "confirm_recovery_code called with no DEK on AppState — should be unreachable \
             post-unlock"
        );
        "Something went wrong reading your master key. Please try again.".to_string()
    })?;

    let instance_dir = state.paths.data_dir.join("keys");

    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);

    let normalized = recovery::normalize(&code);
    let recovery_kek = kdf::derive(normalized.as_bytes(), &salt, kdf::M_COST_KIB, kdf::T_COST)
        .map_err(|e| {
            tracing::error!("deriving the recovery key failed: {e}");
            "Something went wrong saving your recovery code. Please try again.".to_string()
        })?;

    let header = wrap::WrapHeader::new(salt);
    wrap::write_wrap(
        &instance_dir.join("recovery.wrap"),
        dek,
        &recovery_kek,
        &header,
    )
    .map_err(|e| {
        tracing::error!("writing the recovery wrap file failed: {e}");
        "Something went wrong saving your recovery code. Please try again.".to_string()
    })
}

/// Attempts to unlock using a recovery code after the OS keychain failed to
/// produce the master key (`BootState::needs_recovery`) — re-derives the
/// recovery slot's KEK from `code` using the SAME salt/KDF parameters that
/// slot's own wrap-file header records (read back, never assumed), unseals
/// the DEK, and — on success — re-mints the keychain slot under a fresh
/// random KEK sealed with the same DEK, so future launches go back to
/// unlocking silently instead of asking again every time. Takes
/// `State<'_, BootState>`, not `AppState` — this only ever runs during the
/// `NeedsRecovery` window, before `AppState` exists, and finishes by
/// calling the same `state::finish_unlock` the happy path uses to build it.
///
/// Idempotent: if `AppState` is already managed (a prior call already
/// finished, or two overlapping calls raced — Tauri dispatches each
/// `invoke()` as its own async task, so nothing on the frontend actually
/// prevents two in-flight calls), this returns immediately rather than
/// re-deriving the DEK, re-sealing the keychain a second time, and — the
/// concrete failure this guards against — calling `finish_unlock` again,
/// which would spawn a second, permanent `idle::run`/`parental::run` ticker
/// pair (`Manager::manage`'s second call silently drops the redundant
/// `AppState` but does nothing to stop the tickers `finish_unlock` spawns
/// unconditionally after it).
/// Re-derives the recovery slot's KEK from `code` using the SAME salt/KDF
/// parameters recorded in the wrap file at `path` (read back, never
/// assumed), and unseals the `DataKey` it protects. Shared by
/// [`attempt_recovery_unlock`] (which goes on to rebuild `AppState`) and
/// [`unlock_app_lock_with_recovery_code`] (which only needs to confirm the
/// code is right — the app-lock PIN never actually locked the DEK, so that
/// caller drops the returned key immediately rather than doing anything
/// with it).
fn verify_recovery_code_against_wrap(
    path: &Path,
    code: &str,
) -> Result<openpal_core::crypto::key::DataKey, String> {
    use openpal_core::crypto::{kdf, recovery, wrap};

    let header = wrap::read_header(path).map_err(|e| {
        tracing::error!("couldn't read the recovery wrap file's header: {e}");
        "Something went wrong reading your recovery information. Please try again.".to_string()
    })?;

    let normalized = recovery::normalize(code);
    let candidate_kek = kdf::derive(
        normalized.as_bytes(),
        &header.salt,
        header.m_cost_kib,
        header.t_cost,
    )
    .map_err(|e| {
        tracing::error!("deriving the recovery key failed: {e}");
        "Something went wrong checking your recovery code. Please try again.".to_string()
    })?;

    wrap::read_wrap(path, &candidate_kek).map_err(|_| {
        // Deliberately don't log the wrong code itself, and don't distinguish
        // "wrong code" from other AEAD failures in the user-facing message —
        // see `WrapError`'s own variants for what's available if the
        // technical reason ever needs logging without exposing it.
        "That recovery code doesn't match. Check for typos and try again.".to_string()
    })
}

#[tauri::command]
pub fn attempt_recovery_unlock(
    app: tauri::AppHandle,
    state: State<'_, BootState>,
    code: String,
) -> Result<(), String> {
    use openpal_core::crypto::key::DataKey;
    use openpal_core::crypto::{keychain, wrap};

    if app.try_state::<AppState>().is_some() {
        return Ok(());
    }

    let keys_dir = state.paths.data_dir.join("keys");
    let recovery_path = keys_dir.join("recovery.wrap");
    let dek = verify_recovery_code_against_wrap(&recovery_path, &code)?;

    // Self-heal: re-mint the keychain slot under a fresh KEK, sealed with
    // the DEK we just recovered, so future launches unlock silently again.
    let new_kek = DataKey::random();
    keychain::store(crate::secret_store::store().as_ref(), &new_kek).map_err(|e| {
        tracing::error!(
            "recovered the master key, but couldn't re-save it to the OS keychain: {e}"
        );
        "Your recovery code worked, but OpenPal couldn't save a new key to your OS keychain. \
         You may need to enter your recovery code again next launch."
            .to_string()
    })?;
    let keychain_header = wrap::WrapHeader {
        m_cost_kib: 0,
        t_cost: 0,
        ..wrap::WrapHeader::new([0u8; 16])
    };
    wrap::write_wrap(
        &keys_dir.join("keychain.wrap"),
        &dek,
        &new_kek,
        &keychain_header,
    )
    .map_err(|e| {
        tracing::error!("recovered the master key, but couldn't reseal the keychain slot: {e}");
        "Your recovery code worked, but OpenPal couldn't finish resetting your saved key. \
         You may need to enter your recovery code again next launch."
            .to_string()
    })?;

    tracing::info!(
        "recovered the master key via the recovery code and re-sealed the keychain slot"
    );

    // Cleared only once `finish_unlock` has actually succeeded — before this
    // diff nothing between the old `Ok(())` and this store could fail, so
    // the doc comment's "cleared ... on success" held trivially; now that
    // `finish_unlock` is a genuinely fallible step (its `reqwest::Client`
    // build can error), clearing the flag has to wait for it, or a failure
    // here would leave `needs_recovery` false while `AppState` was never
    // built.
    crate::state::finish_unlock(&app, &state, Some(dek)).map_err(|e| {
        tracing::error!("recovered the master key, but couldn't finish booting: {e}");
        "Your recovery code worked, but OpenPal couldn't finish starting up. Please restart \
         OpenPal."
            .to_string()
    })?;
    state.needs_recovery.store(false, Ordering::Relaxed);
    Ok(())
}

// ---------------------------------------------------------------------------
// App lock
//
// A soft, non-cryptographic PIN gate on opening the app at all — see
// `openpal_core::applock`'s module doc for why it never touches the DEK.
// All five commands here take `State<'_, BootState>`, never `AppState`:
// they must work even when `AppState` was never built (a fully locked-out
// user, or before Settings/onboarding gates resolve), so `applock.json` /
// `applock-state.json` are read fresh from disk on every call rather than
// cached anywhere.
// ---------------------------------------------------------------------------

/// Whether the app-lock PIN is turned on. Safe to call with no PIN entered
/// — this is what the boot sequence uses to decide whether to show the
/// lock screen at all.
#[tauri::command]
pub fn get_app_lock_status(state: State<'_, BootState>) -> AppLockStatusView {
    let config = openpal_core::applock::load_config(&state.paths.data_dir.join("applock.json"));
    AppLockStatusView {
        enabled: config.enabled,
    }
}

/// Check the app-lock PIN without changing anything. Rate-limited via
/// `openpal_core::applock::attempt_pin`, shared with [`set_app_lock_pin`]
/// so a caller can't dodge the lockout by trying one command instead of the
/// other.
#[tauri::command]
pub fn verify_app_lock_pin(
    state: State<'_, BootState>,
    pin: String,
) -> Result<AppLockPinCheckResult, String> {
    let config_path = state.paths.data_dir.join("applock.json");
    let state_path = state.paths.data_dir.join("applock-state.json");
    let config = openpal_core::applock::load_config(&config_path);
    let mut lock_state = openpal_core::applock::load_state(&state_path);
    let now = openpal_core::applock::now_epoch_seconds();
    let result = openpal_core::applock::attempt_pin(&config, &mut lock_state, &pin, now);
    openpal_core::applock::save_state(&state_path, &lock_state).map_err(friendly)?;
    Ok(result)
}

/// Turn the app lock on or off, or change its PIN. `current_pin` must match
/// the stored PIN once one is set — including to turn the feature back off
/// — checked through the same `applock::attempt_pin` lockout gate
/// [`verify_app_lock_pin`] uses, so this command can't be used to dodge it.
/// `new_pin: None` leaves an already-set PIN unchanged (same convention as
/// `set_parental_controls`'s `config.new_pin`) — turning the lock on only
/// requires a *fresh* PIN when none exists yet; saving after nothing but an
/// unlock (no PIN change) is a no-op on the PIN itself. Turning the lock
/// off clears `pin_hash`/`pin_salt` too, so no PIN state is left orphaned
/// on a disabled config.
#[tauri::command]
pub fn set_app_lock_pin(
    state: State<'_, BootState>,
    current_pin: Option<String>,
    enabled: bool,
    new_pin: Option<String>,
) -> Result<(), String> {
    let config_path = state.paths.data_dir.join("applock.json");
    let state_path = state.paths.data_dir.join("applock-state.json");
    let mut config = openpal_core::applock::load_config(&config_path);

    if config.pin_hash.is_some() {
        let mut lock_state = openpal_core::applock::load_state(&state_path);
        let now = openpal_core::applock::now_epoch_seconds();
        let result = openpal_core::applock::attempt_pin(
            &config,
            &mut lock_state,
            current_pin.as_deref().unwrap_or(""),
            now,
        );
        openpal_core::applock::save_state(&state_path, &lock_state).map_err(friendly)?;
        if !result.ok {
            return Err(if result.locked_for_seconds > 0 {
                format!(
                    "Too many wrong attempts. Try again in {}s.",
                    result.locked_for_seconds
                )
            } else {
                "That PIN isn't right.".to_string()
            });
        }
    }

    if let Some(pin) = new_pin.filter(|p| !p.is_empty()) {
        if pin.len() < 4 || pin.len() > 6 || !pin.bytes().all(|b| b.is_ascii_digit()) {
            return Err("PIN must be 4-6 digits.".to_string());
        }
        let salt = openpal_core::applock::new_salt();
        let hash = openpal_core::applock::hash_pin(&pin, &salt);
        config.pin_hash = Some(hash);
        config.pin_salt = Some(salt);
    }

    if enabled {
        if config.pin_hash.is_none() {
            return Err("Set a PIN to turn app lock on.".to_string());
        }
    } else {
        config.pin_hash = None;
        config.pin_salt = None;
    }
    config.enabled = enabled;

    openpal_core::applock::save_config(&config_path, &config).map_err(friendly)
}

/// Falls back to the recovery code when the app-lock PIN is forgotten (or
/// its lockout is in the way). Verifies `code` against `<data>/keys/
/// recovery.wrap` the same way [`attempt_recovery_unlock`] does, but never
/// touches `AppState`, `finish_unlock`, or the keychain — the app lock
/// never actually locked the DEK in the first place, so this is a
/// stripped-down identity check, not a fresh unlock. On success, turns the
/// app lock off outright (not just "unlocked until the next lockout"), so a
/// user who had to fall back to their recovery code isn't left facing the
/// same PIN, and the same lockout, again next launch.
#[tauri::command]
pub fn unlock_app_lock_with_recovery_code(
    state: State<'_, BootState>,
    code: String,
) -> Result<(), String> {
    let recovery_path = state.paths.data_dir.join("keys").join("recovery.wrap");
    verify_recovery_code_against_wrap(&recovery_path, &code)?;

    let config_path = state.paths.data_dir.join("applock.json");
    let mut config = openpal_core::applock::load_config(&config_path);
    config.enabled = false;
    config.pin_hash = None;
    config.pin_salt = None;
    openpal_core::applock::save_config(&config_path, &config).map_err(friendly)?;

    let state_path = state.paths.data_dir.join("applock-state.json");
    openpal_core::applock::save_state(&state_path, &AppLockState::default()).map_err(friendly)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Factory reset
// ---------------------------------------------------------------------------

/// Wipes the data directory (except downloaded models) and the OS keychain
/// entry, then restarts OpenPal into a fresh first-launch state. Takes
/// `State<'_, BootState>` for the same reason the app-lock commands do —
/// this must work even for a user locked out of `AppState` entirely. The
/// actual wipe doesn't happen in this process: it can't safely delete its
/// own open `app.sqlite`/log files, so it spawns a relaunch
/// (`crate::factory_reset::spawn_relaunch_for_reset`) that wipes before
/// this process's data is ever touched again, then exits.
#[tauri::command]
pub fn factory_reset(app: tauri::AppHandle, state: State<'_, BootState>) -> Result<(), String> {
    crate::factory_reset::spawn_relaunch_for_reset(&state.paths.data_dir, &state.paths.models_dir)
        .map_err(|e| {
            tracing::error!("couldn't spawn the reset relaunch: {e}");
            "Couldn't restart OpenPal to finish resetting. Please quit and reopen it yourself."
                .to_string()
        })?;
    app.exit(0);
    Ok(())
}

// ---------------------------------------------------------------------------
// Updates
// ---------------------------------------------------------------------------

const CHECK_FAILED_MSG: &str =
    "Couldn't check for updates. Check your internet connection and try again.";
const NO_UPDATE_MSG: &str = "No update is available right now.";
const NOT_CONFIGURED_MSG: &str = "Checking for updates isn't set up yet.";
/// `tauri-plugin-updater` compiles and registers fine on Android, but Play
/// Store policy doesn't want a self-updating APK and its manifest-based
/// flow isn't the Android update path regardless — see `check_for_update`/
/// `install_update`'s own doc comments.
const ANDROID_UPDATE_MSG: &str = "Updates for this app come through the Play Store.";

/// `tauri.conf.json`'s `plugins.updater` block is the one source of truth
/// for the pubkey and endpoint — both SKUs read it (Full via the plugin's
/// own config resolution, Portable by parsing the same raw JSON here, since
/// `openpal_core::update`/`update_portable` have no Tauri config access of
/// their own). The plugin's `Config::pubkey` field is required (not
/// `Option`) — registering the plugin at all panics if this block is
/// missing from `tauri.conf.json`, even before any command ever runs, so an
/// empty placeholder must stay there until a real key is generated
/// (`npx tauri signer generate`).
fn updater_json(app: &tauri::AppHandle) -> serde_json::Value {
    app.config()
        .plugins
        .0
        .get("updater")
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// The manifest endpoint. `OPENPAL_UPDATE_ENDPOINT`, when set, overrides the
/// configured one — lets a dev build point at a local fixture manifest
/// during testing without touching `tauri.conf.json`. Checking for an
/// update needs only this — the manifest itself isn't signed, so no pubkey
/// is required to *see* that a newer version exists (only to install one).
fn resolve_endpoint(app: &tauri::AppHandle) -> Result<String, String> {
    if let Ok(endpoint) = std::env::var("OPENPAL_UPDATE_ENDPOINT") {
        return Ok(endpoint);
    }
    updater_json(app)
        .get("endpoints")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| NOT_CONFIGURED_MSG.to_string())
}

/// The pubkey used to verify a downloaded Portable artifact before
/// installing it. Unlike the endpoint, this is never overridden by
/// `OPENPAL_UPDATE_ENDPOINT` — a fixture manifest for local testing should
/// still be signed with the real key, so the whole check→verify→apply path
/// is exercised as-is. Empty until a real keypair is generated, which fails
/// closed here rather than trusting an unset key.
fn resolve_pubkey(app: &tauri::AppHandle) -> Result<String, String> {
    let pubkey = updater_json(app)
        .get("pubkey")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if pubkey.is_empty() {
        Err(NOT_CONFIGURED_MSG.to_string())
    } else {
        Ok(pubkey)
    }
}

/// A dedicated client for update checks/downloads: the shared `state.http`
/// carries a 30s read timeout, too short for a multi-hundred-MB download —
/// same reasoning as `download_voice`/`preview_voice`.
fn update_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(1800))
        .build()
        .map_err(internal)
}

/// The Full-SKU updater, using `tauri.conf.json`'s configured pubkey as-is
/// and overriding only the endpoint when `OPENPAL_UPDATE_ENDPOINT` is set.
fn full_updater(app: &tauri::AppHandle) -> Result<tauri_plugin_updater::Updater, String> {
    use tauri_plugin_updater::UpdaterExt;
    match std::env::var("OPENPAL_UPDATE_ENDPOINT") {
        Ok(endpoint) => {
            let url = reqwest::Url::parse(&endpoint).map_err(internal)?;
            app.updater_builder()
                .endpoints(vec![url])
                .map_err(internal)?
                .build()
                .map_err(internal)
        }
        Err(_) => app.updater().map_err(internal),
    }
}

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AvailableUpdate {
    pub version: String,
    pub notes: String,
    pub pub_date: String,
}

impl From<openpal_core::update::AvailableUpdate> for AvailableUpdate {
    fn from(u: openpal_core::update::AvailableUpdate) -> Self {
        Self {
            version: u.version,
            notes: u.notes,
            pub_date: u.pub_date,
        }
    }
}

/// Whether a newer version is published, without downloading anything.
/// Called a few seconds after startup (if `settings.autoCheckUpdates`) and
/// from the Settings "Check for updates now" button.
#[tauri::command]
pub async fn check_for_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<AvailableUpdate>, String> {
    if cfg!(target_os = "android") {
        // Play Store policy doesn't want a self-updating APK, and
        // `tauri-plugin-updater`'s manifest-based flow isn't the Android
        // update path regardless. Safe for the un-prompted 5-second
        // post-boot auto-check too — the frontend's non-manual
        // `checkForUpdate` already swallows any error into a silent
        // `updateStatus: "idle"` (ui/src/state/store.ts); only a manual
        // "Check for updates now" click shows this message, which is the
        // accurate answer to that question here.
        return Err(ANDROID_UPDATE_MSG.to_string());
    }
    if state.paths.portable {
        let endpoint = resolve_endpoint(&app)?;
        let client = update_client()?;
        let manifest = openpal_core::update::fetch_manifest(&client, &endpoint)
            .await
            .map_err(friendly)?;
        let current = openpal_core::update::current_version();
        Ok(openpal_core::update::is_update_available(&current, &manifest).map(Into::into))
    } else {
        let updater = full_updater(&app)?;
        let update = updater.check().await.map_err(|e| {
            tracing::warn!("update check failed: {e}");
            CHECK_FAILED_MSG.to_string()
        })?;
        Ok(update.map(|u| AvailableUpdate {
            version: u.version.clone(),
            notes: u.body.clone().unwrap_or_default(),
            pub_date: u.date.map(|d| d.to_string()).unwrap_or_default(),
        }))
    }
}

/// Download and install the update, then relaunch. Re-checks internally
/// (cheap) rather than trusting a stale handle from an earlier
/// `check_for_update` call. Progress streams as the same [`DownloadEvent`]
/// shape `download_model` uses.
#[tauri::command]
pub async fn install_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    on_event: Channel<DownloadEvent>,
) -> Result<(), String> {
    if cfg!(target_os = "android") {
        return Err(ANDROID_UPDATE_MSG.to_string());
    }
    if state.paths.portable {
        install_update_portable(&app, &state, on_event).await
    } else {
        install_update_full(&app, on_event).await
    }
}

async fn install_update_full(
    app: &tauri::AppHandle,
    on_event: Channel<DownloadEvent>,
) -> Result<(), String> {
    let updater = full_updater(app)?;
    let update = updater
        .check()
        .await
        .map_err(|e| {
            tracing::warn!("update check failed: {e}");
            CHECK_FAILED_MSG.to_string()
        })?
        .ok_or_else(|| NO_UPDATE_MSG.to_string())?;

    let mut total_bytes = 0u64;
    let mut received_bytes = 0u64;
    let mut sent_started = false;
    let result = update
        .download_and_install(
            |chunk_len, content_len| {
                if !sent_started {
                    sent_started = true;
                    total_bytes = content_len.unwrap_or(0);
                    let _ = on_event.send(DownloadEvent::Started {
                        total_bytes,
                        resumed_from_bytes: 0,
                    });
                }
                received_bytes += chunk_len as u64;
                let _ = on_event.send(DownloadEvent::Progress {
                    received_bytes,
                    total_bytes,
                    bytes_per_sec: 0.0,
                    eta_seconds: 0,
                });
            },
            || {
                let _ = on_event.send(DownloadEvent::Finished);
            },
        )
        .await;

    if let Err(e) = result {
        tracing::warn!("update install failed: {e}");
        let msg = "Couldn't download or install the update. Please try again.".to_string();
        let _ = on_event.send(DownloadEvent::Failed {
            message: msg.clone(),
        });
        return Err(msg);
    }

    // Triggers the same `RunEvent::ExitRequested` / `RunEvent::Exit` a
    // normal quit does, so the engine/plugin teardown in `lib.rs` still runs.
    app.request_restart();
    Ok(())
}

async fn install_update_portable(
    app: &tauri::AppHandle,
    state: &AppState,
    on_event: Channel<DownloadEvent>,
) -> Result<(), String> {
    let endpoint = resolve_endpoint(app)?;
    let client = update_client()?;
    let manifest = openpal_core::update::fetch_manifest(&client, &endpoint)
        .await
        .map_err(friendly)?;
    let current = openpal_core::update::current_version();
    if openpal_core::update::is_update_available(&current, &manifest).is_none() {
        return Err(NO_UPDATE_MSG.to_string());
    }
    let key = openpal_core::update::portable_target_key()
        .ok_or_else(|| "Automatic updates aren't available for this platform yet.".to_string())?;
    let entry = manifest.portable.get(key).ok_or_else(|| {
        "This release doesn't include a Portable build for your platform yet.".to_string()
    })?;
    let pubkey = resolve_pubkey(app)?;

    let staging_root = state
        .paths
        .data_dir
        .join("update-staging")
        .join(&manifest.version);
    let cancel = Arc::new(AtomicBool::new(false));
    let archive = openpal_core::update_portable::download_portable_update(
        &client,
        entry,
        &pubkey,
        &staging_root,
        cancel,
        |event| {
            let _ = on_event.send(event);
        },
    )
    .await
    .map_err(friendly)?;

    let staged_unit =
        openpal_core::update_portable::extract(&archive, &staging_root.join("extracted"))
            .map_err(friendly)?;

    let install_path = crate::state::portable_install_path()
        .ok_or_else(|| "Couldn't determine where OpenPal is installed.".to_string())?;

    // Same teardown a normal quit runs (`lib.rs`'s `RunEvent::Exit` handler)
    // — the update helper takes over as a fresh process, so nothing from
    // this run must be left behind.
    state.engine.stop().await.ok();
    state.plugins.stop_all().await;
    state.remote.stop_all().await;

    openpal_core::update_portable::spawn_apply_helper(&staged_unit, &install_path)
        .map_err(friendly)?;

    app.exit(0);
    Ok(())
}

// ---------------------------------------------------------------------------
// Engine updates (llama.cpp) — a separate channel from the app updates
// above: signed with the same key (see `resolve_pubkey`), checked and
// installed independently of OpenPal's own version, and never stops the
// running engine or restarts the app. A newly downloaded build only takes
// effect the next time the AI engine starts — the same as switching models
// already requires — via `AppPaths::engine_binary`'s fresh-every-call
// resolution.
// ---------------------------------------------------------------------------

/// `plugins.engineUpdater` in `tauri.conf.json` — a block of its own,
/// deliberately separate from `plugins.updater` (never add keys to that one;
/// `tauri-plugin-updater` parses it for its own config, and an unexpected
/// field there could break plugin registration). The signing pubkey is not
/// duplicated here: engine artifacts are signed with the exact same key as
/// app updates, so `resolve_pubkey` is reused as-is below.
fn engine_updater_json(app: &tauri::AppHandle) -> serde_json::Value {
    app.config()
        .plugins
        .0
        .get("engineUpdater")
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

/// The engine manifest endpoint, overridable by `OPENPAL_ENGINE_UPDATE_ENDPOINT`
/// for local testing — same override pattern as `resolve_endpoint`.
fn resolve_engine_endpoint(app: &tauri::AppHandle) -> Result<String, String> {
    if let Ok(endpoint) = std::env::var("OPENPAL_ENGINE_UPDATE_ENDPOINT") {
        return Ok(endpoint);
    }
    engine_updater_json(app)
        .get("endpoint")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| NOT_CONFIGURED_MSG.to_string())
}

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct EngineUpdateInfo {
    pub current_tag: String,
    pub tag: String,
}

/// The engine tag currently in play: whatever a prior engine update staged
/// (`engine_updates_dir/current.json`), else the tag OpenPal shipped with.
/// `None` only if the app has no usable engine at all yet (a broken install).
fn current_engine_tag(paths: &crate::state::AppPaths) -> Option<String> {
    let current_json = paths.engine_updates_dir.join("current.json");
    if let Ok(text) = std::fs::read_to_string(&current_json) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(tag) = value.get("tag").and_then(|v| v.as_str()) {
                return Some(tag.to_string());
            }
        }
    }
    paths.bundled_engine_version_tag()
}

/// Whether a newer llama.cpp engine build is published, without downloading
/// anything. Mirrors `check_for_update`'s shape but on its own channel.
#[tauri::command]
pub async fn check_engine_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<EngineUpdateInfo>, String> {
    let Some(current_tag) = current_engine_tag(&state.paths) else {
        return Ok(None);
    };
    let endpoint = resolve_engine_endpoint(&app)?;
    let client = update_client()?;
    let manifest = openpal_core::engine_update::fetch_engine_manifest(&client, &endpoint)
        .await
        .map_err(friendly)?;
    let key = openpal_core::update::portable_target_key()
        .ok_or_else(|| "Engine updates aren't available for this platform yet.".to_string())?;
    let Some(entry) = manifest.engine.get(key) else {
        return Ok(None);
    };
    if openpal_core::engine_update::is_engine_update_available(&current_tag, &entry.tag) {
        Ok(Some(EngineUpdateInfo {
            current_tag,
            tag: entry.tag.clone(),
        }))
    } else {
        Ok(None)
    }
}

/// Download, verify, and stage a newer engine build. Re-checks internally
/// (cheap), same reasoning as `install_update`. Unlike `install_update`,
/// this never touches `state.engine` or restarts the app — see this
/// section's own doc comment.
#[tauri::command]
pub async fn install_engine_update(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    on_event: Channel<DownloadEvent>,
) -> Result<(), String> {
    let current_tag = current_engine_tag(&state.paths).ok_or_else(|| {
        "Part of OpenPal is missing. Reinstalling the app fixes this.".to_string()
    })?;
    let endpoint = resolve_engine_endpoint(&app)?;
    let client = update_client()?;
    let manifest = openpal_core::engine_update::fetch_engine_manifest(&client, &endpoint)
        .await
        .map_err(friendly)?;
    let key = openpal_core::update::portable_target_key()
        .ok_or_else(|| "Engine updates aren't available for this platform yet.".to_string())?;
    let entry = manifest
        .engine
        .get(key)
        .ok_or_else(|| NO_UPDATE_MSG.to_string())?;
    if !openpal_core::engine_update::is_engine_update_available(&current_tag, &entry.tag) {
        return Err(NO_UPDATE_MSG.to_string());
    }
    let pubkey = resolve_pubkey(&app)?;

    // Disk-space guard, same shape as `download_model`'s — engine archives
    // run tens of MB, so the same 1 GiB headroom is generous without needing
    // the manifest entry to carry its own size.
    let engine_updates_dir = state.paths.engine_updates_dir.clone();
    let hardware =
        tokio::task::spawn_blocking(move || openpal_core::hardware::detect(&engine_updates_dir))
            .await
            .map_err(internal)?;
    if hardware.free_disk_bytes < GIB {
        return Err("Not enough free disk space. Free up about 1 GB and try again.".to_string());
    }

    let cancel = Arc::new(AtomicBool::new(false));
    let archive = openpal_core::engine_update::download_and_verify_engine(
        &client,
        entry,
        &pubkey,
        &state.paths.engine_updates_dir,
        cancel,
        |event| {
            let _ = on_event.send(event);
        },
    )
    .await
    .map_err(friendly)?;

    let installed = openpal_core::engine_update::extract_and_install(
        &archive,
        &state.paths.engine_updates_dir,
        &entry.tag,
    )
    .map_err(friendly)?;
    tracing::info!(engine = %installed.display(), tag = %entry.tag, "engine update installed");

    openpal_core::engine_update::prune_old_engines(&state.paths.engine_updates_dir, &entry.tag);

    Ok(())
}

// ---------------------------------------------------------------------------
// Logs / diagnostics
// ---------------------------------------------------------------------------

/// How many lines the in-app log viewer can ask for, so a huge debug log can't
/// be slurped whole into the webview.
const MAX_LOG_LINES: usize = 5000;

/// The tail of a log file, for the in-app viewer. `which` is "engine" for the
/// AI engine's own log (`engine.log`, our #1 "why won't my model start" tool)
/// or anything else for the app log. Secrets (the engine API key, bearer
/// tokens) are masked before returning.
#[tauri::command]
pub fn get_recent_logs(
    state: State<'_, AppState>,
    which: String,
    max_lines: Option<usize>,
) -> Result<String, String> {
    let max_lines = max_lines.unwrap_or(500).clamp(1, MAX_LOG_LINES);
    let raw = if which == "engine" {
        state.engine.tail_log(max_lines)
    } else {
        match logging::newest_app_log(&state.paths.logs_dir) {
            Some(path) => logging::decrypt_tail(&path, max_lines, state.dek.as_ref()),
            None => String::new(),
        }
    };
    Ok(logging::redact(&raw))
}

/// Write a decrypted, redacted copy of every log this app keeps (every app
/// log file, `engine.log`, and `engine.prev.log`) to a location the user
/// chooses, via the native save dialog — the replacement for "Open logs
/// folder" now that logs are encrypted at rest and the raw files are no
/// longer directly shareable. Returns `false` if the dialog was closed
/// without a choice (not an error) — a plain `bool` rather than `Option<()>`,
/// since `Some(())` and `None` both serialize to JSON `null` and would be
/// indistinguishable to the frontend. `logging::redact` runs on this path
/// exactly as it does for the in-app viewer (`get_recent_logs`) — one shared
/// function, two call sites, so the engine API key and any bearer tokens are
/// masked here too before anything leaves the app.
#[tauri::command]
pub async fn export_logs(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_file_name(default_log_export_file_name())
        .add_filter("Log file", &["log", "txt"])
        .save_file(move |path| {
            let _ = tx.send(path);
        });
    let Some(picked) = rx.await.map_err(internal)? else {
        return Ok(false); // user closed the dialog
    };
    let dest = picked.into_path().map_err(internal)?;

    let dek = state.dek.as_ref();
    let mut combined = String::new();
    for path in logging::all_app_logs(&state.paths.logs_dir) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        combined.push_str(&format!("==== {name} ====\n"));
        combined.push_str(&logging::decrypt_all(&path, dek));
        combined.push_str("\n\n");
    }
    combined.push_str(&state.engine.export_logs());

    let redacted = logging::redact(&combined);
    tokio::task::spawn_blocking(move || std::fs::write(&dest, redacted))
        .await
        .map_err(internal)?
        .map(|()| true)
        .map_err(|e| {
            tracing::warn!("log export write failed: {e}");
            "Couldn't write the log export. Please try again.".to_string()
        })
}

/// `openpal-logs-<unix-seconds>.log` — a reasonable default so the save
/// dialog doesn't propose a bare "Untitled.log"; renameable in the dialog.
fn default_log_export_file_name() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("openpal-logs-{secs}.log")
}

/// Delete old daily log files (keeping the current one). Returns how many were
/// removed.
#[tauri::command]
pub fn clear_logs(state: State<'_, AppState>) -> Result<usize, String> {
    Ok(logging::clear_old_app_logs(&state.paths.logs_dir))
}

/// Export everything the user has stored locally — conversations, projects,
/// and artifacts — as one plaintext `.zip`, via the native save dialog.
/// `Ok(None)` means the dialog was closed without a choice (not an error) —
/// same cancel convention as [`download_artifact`].
#[tauri::command]
pub async fn export_data(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<openpal_core::export::ExportSummary>, String> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_file_name(default_export_file_name())
        .add_filter("Zip archive", &["zip"])
        .save_file(move |path| {
            let _ = tx.send(path);
        });
    let Some(picked) = rx.await.map_err(internal)? else {
        return Ok(None); // user closed the dialog
    };
    let dest = picked.into_path().map_err(internal)?;

    // Read under a short-lived lock, then compress/write to disk off the
    // async executor and without holding `app_db` for the duration — an
    // export can be large, and every other command touching conversations/
    // projects/artifacts/settings/custom models/plugins would otherwise
    // stall behind it for as long as the zip write takes.
    let export_data = openpal_core::export::read_export_data(&state.app_db.lock().unwrap());
    tokio::task::spawn_blocking(move || openpal_core::export::write_export_zip(&export_data, &dest))
        .await
        .map_err(internal)?
        .map(Some)
        .map_err(friendly)
}

/// Import a `.zip` produced by [`export_data`] on another device (or an
/// earlier export on this one): reads back every conversation, project, and
/// artifact it contains and upserts them into the local database, via the
/// native open dialog. `Ok(None)` means the dialog was closed without a
/// choice (not an error) — same cancel convention as `export_data`.
#[tauri::command]
pub async fn import_data(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<openpal_core::import::ImportSummary>, String> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter("Zip archive", &["zip"])
        .pick_file(move |path| {
            let _ = tx.send(path);
        });
    let Some(picked) = rx.await.map_err(internal)? else {
        return Ok(None); // user closed the dialog
    };
    let src = picked.into_path().map_err(internal)?;

    // Off the async executor, same reasoning as `export_data`: reading the
    // zip, parsing JSON, and upserting every row is real synchronous work
    // that would otherwise stall every other command touching
    // conversations/projects/artifacts/settings/custom models/plugins for as
    // long as a large import takes.
    let app_db = state.app_db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = app_db.lock().unwrap();
        openpal_core::import::read_import_zip(&conn, &src)
    })
    .await
    .map_err(internal)?
    .map(Some)
    .map_err(friendly)
}

/// `openpal-export-<unix-seconds>.zip` — a reasonable default so the save
/// dialog doesn't propose a bare "Untitled.zip"; renameable in the dialog.
fn default_export_file_name() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("openpal-export-{secs}.zip")
}

// ---------------------------------------------------------------------------
// Multi-device sync — device pairing and revocation
// (docs/SYNC_PAIRING_PROTOCOL.md, Phase 2). These commands establish the
// shared SyncKey and device roster; actual conversation/project/artifact/
// settings data sync (Phase 3, docs/design/MULTI_DEVICE_SYNC.md) is implemented
// separately — see `save_conversation`/`save_project`/`delete_project`
// above (routed through `openpal_core::sync::capture` when
// `Settings::sync_enabled`) and `crates/app/src/sync_loop.rs` (the
// background push/pull loop).
//
// Pairing is a two-tap protocol on BOTH sides, not one command per side:
// - Device A: `sync_pair_listen` polls and, on finding a request, posts a
//   challenge and returns `IncomingRequest` — it does not block waiting for
//   the tap. `sync_pair_confirm` is the separate call the UI makes once the
//   user actually taps Confirm/Reject on the rendered card.
// - Device B: `sync_pair_request` polls, and once it successfully decrypts
//   a `join_confirm`, returns `ReadyToInstall` without installing anything
//   yet. `sync_pair_finish` is the separate call once the user gives their
//   own explicit second confirmation (protocol doc step 6 — implicit
//   trust-on-decrypt is not enough).
// This split exists because a Tauri command can't block waiting on a
// *different*, later command invocation — the pending state lives in
// `PairingManager` itself (see that module's `PendingIncoming`/
// `PendingInstall`), not in these commands' stack frames.
// ---------------------------------------------------------------------------

/// Creates a new sync group on this device (no pairing involved — this
/// device is the first member). Returns the `group_secret` to show the
/// user once, for them to relay out-of-band (QR/typed) to a second device.
#[tauri::command]
pub fn sync_group_create(
    state: State<'_, AppState>,
    device_label: String,
) -> Result<String, String> {
    let conn = state.app_db.lock().unwrap();
    state
        .pairing
        .create_group(&conn, &device_label)
        .map_err(friendly)
}

/// This device's currently-active `group_secret`, if it has created or
/// joined a sync group — lets the UI show a "invite another device" QR
/// without the user having to re-enter anything, and lets
/// `sync_pair_listen`/`sync_device_remove` be called without threading the
/// secret through every call site.
#[tauri::command]
pub fn sync_group_secret(state: State<'_, AppState>) -> Result<Option<String>, String> {
    match state.pairing.load_group_secret() {
        Ok(secret) => Ok(Some(secret)),
        Err(_) => Ok(None), // no sync group set up on this device yet — not an error
    }
}

/// Device A's side of "Add a device": listens for one incoming join
/// request, challenges it, and streams exactly one `IncomingRequest`
/// event (or `Expired`/`Failed`) before returning. Does not block waiting
/// for the user's tap — see this section's module doc.
#[tauri::command]
pub async fn sync_pair_listen(
    state: State<'_, AppState>,
    on_event: Channel<openpal_core::sync::pairing::PairingEvent>,
) -> Result<(), String> {
    let group_secret = state.pairing.load_group_secret().map_err(friendly)?;
    state
        .pairing
        .listen_for_join(&group_secret, |event| {
            let _ = on_event.send(event);
        })
        .await
        .map_err(friendly)
}

/// Device A: the user tapped Confirm/Reject on the card `sync_pair_listen`
/// rendered. `approve = false` rejects without installing anything.
#[tauri::command]
pub async fn sync_pair_confirm(
    state: State<'_, AppState>,
    approve: bool,
) -> Result<openpal_core::sync::pairing::PairingEvent, String> {
    let group_secret = state.pairing.load_group_secret().map_err(friendly)?;
    // `confirm_incoming` takes `gen_n: u32` rather than `&Connection` —
    // see that method's doc comment: a `std::sync::MutexGuard` (not `Send`,
    // since `rusqlite::Connection` is not `Sync`) can never cross an
    // `async fn`'s internal `.await`, not even as a briefly-read parameter.
    // All database work happens here, synchronously, before and after the
    // awaited call — never inside it.
    let gen_n = {
        let conn = state.app_db.lock().unwrap();
        openpal_core::sync::pairing::next_gen_n(&conn)
    };
    let (event, roster_entry) = state
        .pairing
        .confirm_incoming(gen_n, &group_secret, approve)
        .await
        .map_err(friendly)?;
    if let Some(entry) = roster_entry {
        let conn = state.app_db.lock().unwrap();
        openpal_core::sync::roster::save(&conn, &entry).map_err(friendly)?;
        // A no-op if this device's `sync_key_state` row already exists at
        // `gen_n` (the common case: a device confirming ANOTHER device's
        // join doesn't itself get a new generation) — but the first-ever
        // call for a brand-new group establishes the real
        // `key_installed_at_ms` the periodic-rotation age trigger needs.
        // See `key_state::set_current_gen`'s doc comment.
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        openpal_core::sync::key_state::set_current_gen(&conn, gen_n, now_ms).map_err(friendly)?;
    }
    Ok(event)
}

/// Device B's side: joins a sync group using a `group_secret` obtained
/// out-of-band (typed or scanned from Device A's screen). Streams
/// `AwaitingChallenge` → `AwaitingConfirmation{fingerprint}` →
/// `ReadyToInstall` (or `Expired`/`Failed`) before returning — does not
/// install anything until `sync_pair_finish` is called separately.
#[tauri::command]
pub async fn sync_pair_request(
    state: State<'_, AppState>,
    group_secret: String,
    device_label: String,
    on_event: Channel<openpal_core::sync::pairing::PairingEvent>,
) -> Result<(), String> {
    state
        .pairing
        .request_join(&group_secret, &device_label, |event| {
            let _ = on_event.send(event);
        })
        .await
        .map_err(friendly)
}

/// Device B: the user gave the second, explicit "codes match, finish
/// pairing?" confirmation after `sync_pair_request` reached
/// `ReadyToInstall`. `approve = false` discards the decrypted SyncKey
/// without sealing anything.
#[tauri::command]
pub async fn sync_pair_finish(
    state: State<'_, AppState>,
    approve: bool,
) -> Result<openpal_core::sync::pairing::PairingEvent, String> {
    let (event, roster_entries) = state.pairing.finish_join(approve).await.map_err(friendly)?;
    if !roster_entries.is_empty() {
        let conn = state.app_db.lock().unwrap();
        for entry in &roster_entries {
            openpal_core::sync::roster::save(&conn, entry).map_err(friendly)?;
        }
        // This device's very first `sync_key_state` row — see
        // `key_state::set_current_gen`'s doc comment for why the first
        // call must happen here (right after the roster this device just
        // learned about is saved, so `next_gen_n`'s roster-seeded fallback
        // reads the correct generation) rather than being left to seed
        // lazily at `key_installed_at_ms: 0`.
        let gen_n = openpal_core::sync::pairing::next_gen_n(&conn);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        openpal_core::sync::key_state::set_current_gen(&conn, gen_n, now_ms).map_err(friendly)?;
    }
    Ok(event)
}

/// Cancels whichever pairing session (listen or request side) is currently
/// in flight on this device — closing the "Add a device" / "join a group"
/// screen should call this so a stale session can't later surface a card
/// for a request that arrived after the user gave up on it.
#[tauri::command]
pub async fn sync_pair_cancel(state: State<'_, AppState>) -> Result<(), String> {
    state.pairing.cancel().await;
    Ok(())
}

/// The current sync-group roster (this device plus every paired device),
/// for Settings → Devices → Sync's device list.
#[tauri::command]
pub fn sync_device_list(
    state: State<'_, AppState>,
) -> Result<Vec<openpal_core::sync::roster::DeviceSummary>, String> {
    let conn = state.app_db.lock().unwrap();
    Ok(openpal_core::sync::roster::list_summaries(&conn))
}

/// Removes `device_id` from the sync group and rotates the SyncKey,
/// delivering it to every surviving device — see
/// `docs/SYNC_PAIRING_PROTOCOL.md`'s "Revocation and rotation" for why this
/// is authenticated by this device's own persistent identity signature,
/// never by the key being replaced.
#[tauri::command]
pub async fn sync_device_remove(
    state: State<'_, AppState>,
    device_id: String,
) -> Result<(), String> {
    let group_secret = state.pairing.load_group_secret().map_err(friendly)?;
    // Every database access happens here, synchronously, under the lock —
    // then the guard is dropped before the `.await` below. Holding a
    // `std::sync::MutexGuard` (not `Send`) across an await would either
    // fail to compile or silently block the async runtime; see
    // `remove_device_locally`'s own doc comment for why this split exists.
    let pending = {
        let conn = state.app_db.lock().unwrap();
        openpal_core::sync::pairing::remove_device_locally(
            &conn,
            &state.paths.data_dir.join("keys"),
            crate::secret_store::store().as_ref(),
            &group_secret,
            &device_id,
        )
        .map_err(friendly)?
    };
    openpal_core::sync::pairing::deliver_rotations(
        state.pairing.transport(),
        pending,
        &group_secret,
    )
    .await;
    Ok(())
}

/// Let the webview record its own errors into the same log file as the backend,
/// so a crash in the UI isn't invisible. Best-effort: bad input is dropped, not
/// surfaced.
#[tauri::command]
pub fn frontend_log(level: String, message: String) {
    // Cap the message so a runaway loop in the UI can't flood the log.
    let message: String = message.chars().take(4000).collect();
    match level.as_str() {
        "error" => tracing::error!(target: "webview", "{message}"),
        "warn" => tracing::warn!(target: "webview", "{message}"),
        "info" => tracing::info!(target: "webview", "{message}"),
        _ => tracing::debug!(target: "webview", "{message}"),
    }
}

// ---------------------------------------------------------------------------
// Remote Access
// ---------------------------------------------------------------------------

/// Current Remote Access status (Settings → Devices) — whether it's turned
/// on, whether the gateway is actually running, which port/interfaces it's
/// reachable on, and how many devices are paired.
#[tauri::command]
pub async fn get_remote_access_status(
    state: State<'_, AppState>,
) -> Result<RemoteAccessStatusInfo, String> {
    Ok(state.remote.status().await)
}

/// Turn Remote Access on or off. Turning it on starts the gateway (self-
/// signed cert generated on first use) and LAN discovery; turning it off
/// tears both down immediately, so a previously paired device stops being
/// able to reach this desktop the moment the toggle flips.
#[tauri::command]
pub async fn set_remote_access_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    state.remote.set_enabled(enabled).await
}

/// Open one pairing session ("Add a device"): streams a `Waiting` event
/// with the QR payload immediately, then `Paired`/`Expired`/`Failed` once
/// the session resolves. Only one session can be open at a time — pressing
/// "Add a device" again after this returns opens a fresh one.
#[tauri::command]
pub async fn start_pairing_session(
    state: State<'_, AppState>,
    on_event: Channel<PairingEvent>,
) -> Result<(), String> {
    state
        .remote
        .start_pairing(|event| {
            let _ = on_event.send(event);
        })
        .await
}

#[tauri::command]
pub async fn list_paired_devices(
    state: State<'_, AppState>,
) -> Result<Vec<PairedDeviceInfo>, String> {
    state.remote.list_paired_devices().await
}

/// Revoke a paired device's access immediately — its bearer token stops
/// authenticating on the very next request.
#[tauri::command]
pub async fn revoke_paired_device(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.remote.revoke_paired_device(&id).await
}

#[tauri::command]
pub async fn rename_paired_device(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> Result<(), String> {
    state.remote.rename_paired_device(&id, &name).await
}

/// Best-effort "is Wake-on-LAN enabled on this machine" check, shown next
/// to instructions for turning it on. See `openpal_core::remote::WolStatus`'s
/// own doc comment for why a failed check reports `Unknown`, never a false
/// `Disabled`.
#[tauri::command]
pub async fn get_wol_status(state: State<'_, AppState>) -> Result<WolStatus, String> {
    Ok(state.remote.wol_status().await)
}

// ---------------------------------------------------------------------------
// Remote Client — this device pairing with (and chatting through) someone
// else's Remote Access gateway. On Android, `state.engine` is `state
// .remote_backend` under the hood (see `state::finish_unlock`), so
// `get_engine_status`/`chat_stream` already reflect this pairing once one
// exists; these commands cover what that generic surface doesn't: finding
// a desktop, completing pairing, and inspecting/clearing it.
// ---------------------------------------------------------------------------

/// Start browsing for `_openpal._tcp.local.` gateways on the LAN, streaming
/// each one found. Replaces any discovery session already in progress.
/// Won't find anything from behind most emulator/VM networking or an
/// overlay network (Tailscale/WireGuard) — the pairing UI's manual
/// "enter an address" field is the real fallback for those, not just a
/// test workaround; see `openpal_core::remote::client::discovery`'s doc
/// comment.
#[tauri::command]
pub async fn remote_discover_hosts(
    state: State<'_, AppState>,
    on_event: Channel<DiscoveredHost>,
) -> Result<(), String> {
    if let Some(old) = state.remote_discovery.lock().await.take() {
        old.stop().await;
    }
    if let Err(e) = crate::wifi_lock::acquire() {
        // Degraded, not fatal: many networks/devices work without it.
        tracing::warn!("remote client: couldn't acquire the wifi multicast lock: {e}");
    }
    let browser = Browser::start(move |host| {
        let _ = on_event.send(host);
    })?;
    *state.remote_discovery.lock().await = Some(browser);
    Ok(())
}

#[tauri::command]
pub async fn remote_stop_discovery(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(browser) = state.remote_discovery.lock().await.take() {
        browser.stop().await;
    }
    if let Err(e) = crate::wifi_lock::release() {
        tracing::warn!("remote client: couldn't release the wifi multicast lock: {e}");
    }
    Ok(())
}

/// Complete pairing with a desktop at `addr:port` using the code shown on
/// its screen. Returns the new pairing record on success (so the UI can
/// show "Connected to Ana's Desktop" without a second round trip).
#[tauri::command]
pub async fn remote_pair(
    state: State<'_, AppState>,
    addr: String,
    port: u16,
    code: String,
    device_name: String,
) -> Result<RemotePairingRecord, String> {
    state
        .remote_backend
        .pair(&addr, port, &code, &device_name)
        .await
        .map_err(|e| e.message())?;
    state
        .remote_backend
        .paired_record()
        .await
        .ok_or_else(|| "Pairing succeeded but the result couldn't be read back.".to_string())
}

/// The current pairing, if any — for the "manage pairing" settings screen.
/// Chat availability itself is already reflected by `get_engine_status` on
/// this backend; this is the fuller picture (which desktop, its address,
/// when pairing happened) that view alone doesn't carry.
#[tauri::command]
pub async fn remote_client_status(
    state: State<'_, AppState>,
) -> Result<Option<RemotePairingRecord>, String> {
    Ok(state.remote_backend.paired_record().await)
}

/// Forget the current pairing. Idempotent.
#[tauri::command]
pub async fn remote_forget(state: State<'_, AppState>) -> Result<(), String> {
    state.remote_backend.forget().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openpal_core::types::FitLevel;

    #[test]
    fn native_notification_needs_the_switch_on_and_the_window_unfocused() {
        assert!(should_notify_native(true, false));
        assert!(!should_notify_native(true, true));
        assert!(!should_notify_native(false, false));
        assert!(!should_notify_native(false, true));
    }

    #[test]
    fn validate_writing_tool_text_rejects_empty_and_whitespace_only() {
        assert_eq!(
            validate_writing_tool_text("", "empty"),
            Err("empty".to_string())
        );
        assert_eq!(
            validate_writing_tool_text("   \n\t", "empty"),
            Err("empty".to_string())
        );
    }

    #[test]
    fn validate_writing_tool_text_trims_and_passes_through_real_text() {
        assert_eq!(
            validate_writing_tool_text("  hello world  ", "empty"),
            Ok("hello world".to_string())
        );
    }

    #[test]
    fn validate_draft_length_accepts_the_cap_and_rejects_beyond() {
        let at_cap = "x".repeat(prompt_enhancer::MAX_DRAFT_CHARS);
        assert_eq!(validate_draft_length(&at_cap), Ok(()));

        let over_cap = "x".repeat(prompt_enhancer::MAX_DRAFT_CHARS + 1);
        assert!(validate_draft_length(&over_cap).is_err());
    }

    #[test]
    fn validate_draft_length_counts_chars_not_bytes() {
        // Each 'é' is 2 bytes in UTF-8 but 1 char, so this stays at the cap.
        let draft = "é".repeat(prompt_enhancer::MAX_DRAFT_CHARS);
        assert_eq!(validate_draft_length(&draft), Ok(()));
    }

    #[test]
    fn validate_translate_languages_accepts_a_known_target_with_no_source() {
        assert_eq!(validate_translate_languages(None, "fr"), Ok(None));
        assert_eq!(validate_translate_languages(Some(""), "fr"), Ok(None));
    }

    #[test]
    fn validate_translate_languages_accepts_a_known_source_and_target() {
        assert_eq!(
            validate_translate_languages(Some("en"), "fr"),
            Ok(Some("en"))
        );
    }

    #[test]
    fn validate_translate_languages_rejects_an_unknown_source() {
        assert!(validate_translate_languages(Some("xx-nope"), "fr").is_err());
    }

    #[test]
    fn validate_translate_languages_rejects_an_unknown_or_missing_target() {
        assert!(validate_translate_languages(None, "xx-nope").is_err());
        assert!(validate_translate_languages(None, "").is_err());
    }

    #[tokio::test]
    async fn claim_generation_slot_rejects_a_second_claim_while_the_first_is_held() {
        let slot: tokio::sync::Mutex<Option<Arc<AtomicBool>>> = tokio::sync::Mutex::new(None);

        let first = claim_generation_slot(&slot)
            .await
            .expect("first claim should succeed on a free slot");

        let second = claim_generation_slot(&slot).await;
        assert!(
            second.is_err(),
            "a second claim must be rejected while the first is still held"
        );

        drop(first);
        *slot.lock().await = None; // simulate the holder's own cleanup on completion
        claim_generation_slot(&slot)
            .await
            .expect("a claim must succeed again once the slot is released");
    }

    fn args(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn strip_gpu_layers_flag_removes_short_and_long_forms_and_their_value() {
        let mut a = args(&["-ngl", "99", "--jinja"]);
        strip_gpu_layers_flag(&mut a);
        assert_eq!(a, args(&["--jinja"]));

        let mut a = args(&["--foo", "--n-gpu-layers", "42", "--bar"]);
        strip_gpu_layers_flag(&mut a);
        assert_eq!(a, args(&["--foo", "--bar"]));
    }

    #[test]
    fn strip_gpu_layers_flag_is_a_no_op_without_the_flag() {
        let mut a = args(&["--reasoning-budget", "0"]);
        strip_gpu_layers_flag(&mut a);
        assert_eq!(a, args(&["--reasoning-budget", "0"]));
    }

    #[test]
    fn strip_gpu_layers_flag_handles_a_dangling_flag_with_no_value() {
        let mut a = args(&["--jinja", "-ngl"]);
        strip_gpu_layers_flag(&mut a);
        assert_eq!(a, args(&["--jinja"]));
    }

    fn hw(has_metal: bool, gpu_vram_bytes: Option<u64>) -> openpal_core::types::HardwareInfo {
        openpal_core::types::HardwareInfo {
            chip: "Test".to_string(),
            is_apple_silicon: has_metal,
            total_ram_bytes: 16 * 1024 * 1024 * 1024,
            available_ram_bytes: 8 * 1024 * 1024 * 1024,
            cpu_cores: 8,
            has_metal,
            free_disk_bytes: 500 * 1024 * 1024 * 1024,
            os_version: "Test 1.0".to_string(),
            gpu_name: gpu_vram_bytes.map(|_| "Test GPU".to_string()),
            gpu_vram_bytes,
        }
    }

    const VRAM_12GB: u64 = 12 * 1024 * 1024 * 1024;

    #[test]
    fn gpu_plan_never_offloads_a_disk_slow_model_on_metal() {
        // The #24510 safety invariant this whole feature is built around.
        let mut s = spec(50_000_000_000, vec![]);
        s.moe = true;
        let plan = gpu_plan(&hw(true, None), &s, false, true);
        assert_eq!(plan.gpu_layers, 0);
        assert!(!plan.cpu_moe);
    }

    #[test]
    fn gpu_plan_gives_metal_full_offload_for_fitting_models() {
        let s = spec(5_000_000_000, vec![]);
        let plan = gpu_plan(&hw(true, None), &s, false, false);
        assert_eq!(plan.gpu_layers, 99);
        assert!(!plan.cpu_moe);
    }

    #[test]
    fn gpu_plan_force_cpu_wins_over_everything() {
        let mut s = spec(5_000_000_000, vec![]);
        s.moe = true;
        for (metal, vram, disk_slow) in [
            (true, None, false),
            (false, Some(VRAM_12GB), false),
            (false, Some(VRAM_12GB), true),
        ] {
            let plan = gpu_plan(&hw(metal, vram), &s, true, disk_slow);
            assert_eq!(plan.gpu_layers, 0);
            assert!(!plan.cpu_moe);
        }
    }

    #[test]
    fn gpu_plan_offloads_moe_hot_path_on_a_big_vulkan_gpu_even_when_disk_slow() {
        // Vulkan copies GPU tensors into VRAM instead of wiring the mmap, so
        // the hot-path-in-VRAM + experts-streaming recipe is safe for
        // bigger-than-RAM MoE models (docs/design/DISK_OFFLOAD.md speed-up levers).
        let mut s = spec(50_000_000_000, vec![]);
        s.moe = true;
        for disk_slow in [false, true] {
            let plan = gpu_plan(&hw(false, Some(VRAM_12GB)), &s, false, disk_slow);
            assert_eq!(plan.gpu_layers, 99, "disk_slow={disk_slow}");
            assert!(plan.cpu_moe, "disk_slow={disk_slow}");
        }
    }

    #[test]
    fn gpu_plan_keeps_dense_models_on_cpu_even_with_a_vulkan_gpu() {
        // Offloading dense models needs VRAM-fit math we don't have; a failed
        // GPU allocation is a failed engine start.
        let s = spec(5_000_000_000, vec![]);
        let plan = gpu_plan(&hw(false, Some(VRAM_12GB)), &s, false, false);
        assert_eq!(plan.gpu_layers, 0);
        assert!(!plan.cpu_moe);
    }

    #[test]
    fn gpu_plan_ignores_small_or_absent_vulkan_gpus() {
        let mut s = spec(5_000_000_000, vec![]);
        s.moe = true;
        for vram in [None, Some(4 * 1024 * 1024 * 1024)] {
            let plan = gpu_plan(&hw(false, vram), &s, false, false);
            assert_eq!(plan.gpu_layers, 0);
            assert!(!plan.cpu_moe);
        }
    }

    // -- auto-unload: auto_unload_at_ms --------------------------------------

    fn activity(disk_slow: bool, idle_for: Duration, generating: bool) -> engine::ActivityInfo {
        engine::ActivityInfo {
            disk_slow,
            idle_for,
            generating,
            generation: 0,
        }
    }

    #[test]
    fn auto_unload_at_ms_none_when_disabled() {
        let settings = Settings {
            auto_unload_enabled: false,
            ..Settings::default()
        };
        let info = activity(false, Duration::from_secs(0), false);
        assert!(auto_unload_at_ms(&settings, &info).is_none());
    }

    #[test]
    fn auto_unload_at_ms_none_while_generating() {
        let settings = Settings::default(); // enabled by default
        let info = activity(false, Duration::from_secs(0), true);
        assert!(
            auto_unload_at_ms(&settings, &info).is_none(),
            "must never count down mid-generation regardless of idle time"
        );
    }

    #[test]
    fn auto_unload_at_ms_none_for_disk_slow_unless_opted_in() {
        let settings = Settings::default(); // auto_unload_include_disk_slow is off by default
        let info = activity(true, Duration::from_secs(0), false);
        assert!(auto_unload_at_ms(&settings, &info).is_none());
    }

    #[test]
    fn auto_unload_at_ms_some_for_disk_slow_when_opted_in() {
        let settings = Settings {
            auto_unload_include_disk_slow: true,
            ..Settings::default()
        };
        let info = activity(true, Duration::from_secs(0), false);
        assert!(auto_unload_at_ms(&settings, &info).is_some());
    }

    #[test]
    fn auto_unload_at_ms_is_not_in_the_future_once_the_limit_is_already_exceeded() {
        let settings = Settings {
            auto_unload_minutes: 15,
            ..Settings::default()
        };
        let info = activity(false, Duration::from_secs(20 * 60), false); // 20 min idle > 15 min limit
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let at = auto_unload_at_ms(&settings, &info).expect("still enabled and eligible");
        assert!(
            at <= now_ms,
            "an already-exceeded idle time must not compute a future timestamp"
        );
    }

    #[test]
    fn auto_unload_at_ms_is_roughly_minutes_from_now_when_freshly_idle() {
        let settings = Settings {
            auto_unload_minutes: 15,
            ..Settings::default()
        };
        let info = activity(false, Duration::from_secs(0), false);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let at =
            auto_unload_at_ms(&settings, &info).expect("enabled, not generating, not disk-slow");
        let expected = now_ms + 15 * 60 * 1000;
        assert!(
            (at - expected).abs() < 2000,
            "expected ~{expected}, got {at}"
        );
    }

    fn spec(size_bytes: u64, extra_shards: Vec<openpal_core::types::ModelShard>) -> ModelSpec {
        ModelSpec {
            id: "test-model".to_string(),
            name: "Test Model".to_string(),
            family: "Test".to_string(),
            publisher: "Test".to_string(),
            description: String::new(),
            strengths: vec![],
            url: "https://example.com/model-00001-of-00003.gguf".to_string(),
            file_name: "model-00001-of-00003.gguf".to_string(),
            size_bytes,
            extra_shards,
            quant: "Q4_K_M".to_string(),
            sha256: Some("shard1sha".to_string()),
            params_b: 8.0,
            moe: false,
            active_params_b: None,
            context_max: 8192,
            quality_score: 50,
            license: "Apache 2.0".to_string(),
            extra_args: vec![],
            hidden: false,
            curated: false,
            task_defaults: std::collections::HashMap::new(),
            task_defaults_thinking: std::collections::HashMap::new(),
            task_scores: std::collections::HashMap::new(),
            source: ModelSource::Catalog,
            local_path: None,
            vision: false,
            mmproj_url: None,
            mmproj_file_name: None,
            mmproj_size_bytes: None,
            mmproj_sha256: None,
            group_id: None,
            release_date: None,
        }
    }

    fn shard(url: &str, file_name: &str, size_bytes: u64) -> openpal_core::types::ModelShard {
        openpal_core::types::ModelShard {
            url: url.to_string(),
            file_name: file_name.to_string(),
            size_bytes,
            sha256: Some(format!("sha-{file_name}")),
        }
    }

    #[test]
    fn build_download_parts_covers_shard_1_only_for_an_unsharded_model() {
        let s = spec(1000, vec![]);
        let parts = build_download_parts(std::path::Path::new("/models"), &s);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].required);
        assert_eq!(
            parts[0].dest,
            std::path::Path::new("/models/model-00001-of-00003.gguf")
        );
        assert_eq!(parts[0].expected_bytes, Some(1000));
    }

    #[test]
    fn build_download_parts_orders_every_shard_before_the_optional_projector() {
        let mut s = spec(
            10,
            vec![
                shard(
                    "https://example.com/2.gguf",
                    "model-00002-of-00003.gguf",
                    20,
                ),
                shard(
                    "https://example.com/3.gguf",
                    "model-00003-of-00003.gguf",
                    30,
                ),
            ],
        );
        s.vision = true;
        s.mmproj_url = Some("https://example.com/mmproj.gguf".to_string());
        s.mmproj_file_name = Some("mmproj.gguf".to_string());
        s.mmproj_size_bytes = Some(5);

        let parts = build_download_parts(std::path::Path::new("/models"), &s);
        let names: Vec<&str> = parts
            .iter()
            .map(|p| p.dest.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "model-00001-of-00003.gguf",
                "model-00002-of-00003.gguf",
                "model-00003-of-00003.gguf",
                "mmproj.gguf"
            ]
        );

        // Every shard is required; only the trailing projector is optional.
        assert!(parts[..3].iter().all(|p| p.required));
        assert!(!parts[3].required);

        let combined_total: u64 = parts.iter().map(|p| p.expected_bytes.unwrap_or(0)).sum();
        assert_eq!(
            combined_total,
            10 + 20 + 30 + 5,
            "disk-space check and progress bar need this total"
        );
    }

    // -- hf-browser: fit-badge decoration -----------------------------------

    fn hf_file(size_bytes: u64, quant: &str) -> openpal_core::types::HfFile {
        openpal_core::types::HfFile {
            path: format!("model-{quant}.gguf"),
            size_bytes,
            quant: quant.to_string(),
            sha256: None,
            extra_shards: vec![],
        }
    }

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn decorate_hf_file_known_params_b_overrides_the_size_estimate() {
        // 7.5 GiB Q4_K_M -> hf_spec's own size-based estimate lands ~13.7B
        // params (KV-cache tier 800MB), which just tips this 16GB-RAM budget
        // (11.2GB) into Tight. Overriding to a declared 3B (KV-cache tier
        // 400MB) frees just enough of the working set to land in Good
        // instead — proof the override, not the heuristic, drove the result.
        let file = hf_file(7 * GIB + GIB / 2, "Q4_K_M");
        let hardware = hw(true, None);

        let estimated = decorate_hf_file("owner/repo", "main", &file, None, None, &hardware, false);
        assert_eq!(
            estimated.fit,
            FitLevel::Tight,
            "sanity: the size estimate alone lands Tight"
        );

        let overridden = decorate_hf_file(
            "owner/repo",
            "main",
            &file,
            Some(3.0),
            None,
            &hardware,
            false,
        );
        assert_eq!(
            overridden.fit,
            FitLevel::Good,
            "a real, smaller param count should fit better"
        );
    }

    #[test]
    fn decorate_hf_file_without_known_params_b_matches_hf_spec_directly() {
        // None must be a true passthrough: same spec, same fit_for call a
        // caller would get building it by hand.
        let file = hf_file(4 * GIB, "Q4_K_M");
        let hardware = hw(true, None);

        let decorated = decorate_hf_file("owner/repo", "main", &file, None, None, &hardware, false);
        let manual_spec = custom::hf_spec("owner/repo", "main", &file, None, &[], None);
        let (manual_fit, manual_why) = selection::fit_for(&manual_spec, &hardware, false);

        assert_eq!(decorated.fit, manual_fit);
        assert_eq!(decorated.why, manual_why);
        assert_eq!(decorated.file.path, file.path);
    }

    #[test]
    fn decorate_hf_file_disk_offload_still_helps_a_browsed_file_without_a_known_shape() {
        // Without a `shape` (the gguf_meta-derived MoE detection — not
        // fetched here), `custom::hf_spec` falls back to `moe: false` — but
        // the toggle alone decides DiskSlow eligibility now (selection.rs
        // module docs), so it still reaches DiskSlow, just with the starker
        // "almost the whole file" copy since there's no active-expert
        // shortcut to fall back on.
        let file = hf_file(200 * GIB, "Q4_K_M"); // far beyond any budget below
        let hardware = hw(true, None);

        let off = decorate_hf_file("owner/repo", "main", &file, None, None, &hardware, false);
        let on = decorate_hf_file("owner/repo", "main", &file, None, None, &hardware, true);
        assert_eq!(off.fit, FitLevel::TooBig);
        assert_eq!(on.fit, FitLevel::DiskSlow);
        assert!(!on.moe);
    }

    #[test]
    fn decorate_hf_file_uses_the_real_shape_when_given() {
        // A real MoE shape (e.g. from `cached_gguf_meta`) makes DiskSlow
        // reachable for a browsed file, unlike the moe:false fallback above.
        let file = hf_file(200 * GIB, "Q4_K_M"); // far beyond any budget below
        let hardware = hw(true, None);

        let shape = Some((200.0, true, Some(8.0))); // active experts fit comfortably
        let decorated = decorate_hf_file("owner/repo", "main", &file, None, shape, &hardware, true);
        assert!(decorated.precise);
        assert!(decorated.moe);
        assert_eq!(decorated.active_params_b, Some(8.0));
        assert_eq!(decorated.fit, FitLevel::DiskSlow);
    }

    // -- hf-browser: grid-level fit hint (decorate_hf_summary) --------------

    fn summary_with_params(total_params: Option<u64>) -> openpal_core::types::HfModelSummary {
        openpal_core::types::HfModelSummary {
            repo_id: "owner/repo".to_string(),
            author: None,
            pipeline_tag: None,
            library_name: None,
            downloads: None,
            likes: None,
            license: None,
            gated: false,
            last_modified: None,
            gguf: total_params.map(|total| openpal_core::types::HfGgufInfo {
                total_params: Some(total),
                architecture: None,
                context_length: None,
            }),
            fit: None,
            fit_why: None,
        }
    }

    #[test]
    fn decorate_hf_summary_leaves_fit_none_without_a_known_param_count() {
        let mut model = summary_with_params(None);
        let hardware = hw(true, None);
        decorate_hf_summary(&mut model, &hardware, false);
        assert!(model.fit.is_none());
        assert!(model.fit_why.is_none());
    }

    #[test]
    fn decorate_hf_summary_estimates_fit_from_gguf_total_params() {
        // A tiny 1B-param repo comfortably fits any real machine at the
        // assumed Q4_K_M size; a 500B-param repo doesn't fit any machine.
        let hardware = hw(true, None);

        let mut small = summary_with_params(Some(1_000_000_000));
        decorate_hf_summary(&mut small, &hardware, false);
        assert!(
            matches!(small.fit, Some(FitLevel::Perfect | FitLevel::Good)),
            "got {:?}",
            small.fit
        );
        assert!(small
            .fit_why
            .as_deref()
            .is_some_and(|w| w.contains("Rough estimate")));

        let mut huge = summary_with_params(Some(500_000_000_000));
        decorate_hf_summary(&mut huge, &hardware, false);
        assert_eq!(huge.fit, Some(FitLevel::TooBig));
    }
}
