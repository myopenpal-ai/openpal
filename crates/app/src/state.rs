use crate::audio::AudioPlayer;
use crate::generation::GenerationQueue;
use openpal_core::account::AccountManager;
use openpal_core::agent::{audit::AuditLog, ConsentBroker};
use openpal_core::catalog::Catalog;
use openpal_core::crypto::key::DataKey;
use openpal_core::custom::CustomModels;
use openpal_core::engine::{EngineBackend, EngineManager};
use openpal_core::gguf_meta::GgufMeta;
use openpal_core::plugin::PluginManager;
use openpal_core::remote::client::{Browser, RemoteBackend};
use openpal_core::remote::RemoteAccessManager;
use openpal_core::sync::hlc::HlcClock;
use openpal_core::sync::mailbox_client::{relay_base_url, validate_relay_url, MailboxClient};
use openpal_core::sync::pairing::PairingManager;
use openpal_core::sync::roster;
use openpal_core::types::{
    DiskSpeedInfo, HardwareInfo, HfListing, HfModelDetails, ParentalConfig, ParentalState, Settings,
};
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager};

pub struct AppPaths {
    /// The resolved data directory everything else derives from. In a normal
    /// install this is the OS app-data dir; in the Portable build it's a `data/`
    /// folder next to the executable. Surfaced to the user in Settings.
    pub data_dir: PathBuf,
    pub models_dir: PathBuf,
    /// Where user-authored `/<slug>` skills lived before they moved onto
    /// `app_db` (one Markdown file per skill) — kept only as the source path
    /// for `crate::skills::migrate_legacy_skills`'s one-time migration on a
    /// returning install; nothing writes here anymore.
    pub skills_dir: PathBuf,
    /// Folder holding the app's rolling log files and the engine's `engine.log`.
    /// Surfaced to the user as "Open logs folder".
    pub logs_dir: PathBuf,
    /// True when running as the no-install Portable build: data lives next to
    /// the executable and built-in plugins are not discovered. The UI uses this
    /// to hide the Plugins tab.
    pub portable: bool,
    /// A read-only `models/` folder a "Portable + Model" distribution ships
    /// beside the executable (sibling of the marker). Searched in addition to
    /// the writable models dir so a pre-staged GGUF is found — and runnable —
    /// even when the medium is read-only (`data/` then falls back to app-data).
    /// `None` unless portable and the folder exists.
    pub bundled_models_dir: Option<PathBuf>,
    /// Where the in-app engine updater downloads and stages a newer llama.cpp
    /// build (`<tag>/` subdirectories plus a `current.json` pointer) — always
    /// writable, unlike the bundled copy below. See
    /// `openpal_core::engine_update`.
    pub engine_updates_dir: PathBuf,
    /// The engine OpenPal shipped with, read-only once installed (inside the
    /// packaged app's resource dir, or the dev tree). Used only as the
    /// fallback `engine_binary()` returns when no downloaded engine is
    /// staged, or the staged one has gone missing.
    bundled_engine_binary: Option<PathBuf>,
}

impl AppPaths {
    /// The llama-server binary to launch: a dev/test `OPENPAL_ENGINE_DIR`
    /// override if set (see [`engine_override_binary`]), else a downloaded
    /// update in `engine_updates_dir` if one is staged and its `current.json`
    /// still points at a binary that exists, else the bundled fallback.
    /// Resolved fresh on every call (a cheap JSON read + `exists()` check)
    /// rather than cached at boot, so an update staged mid-session takes
    /// effect on the engine's next start without needing to restart OpenPal
    /// itself.
    pub fn engine_binary(&self) -> Option<PathBuf> {
        if let Some(override_binary) = engine_override_binary() {
            return Some(override_binary);
        }
        openpal_core::engine_update::resolve_current_engine(
            &self.engine_updates_dir,
            self.bundled_engine_binary.as_deref(),
        )
    }

    /// The `tag` of the baseline engine OpenPal compares updates against
    /// (`engine-version.json`, staged next to the bundled fallback binary by
    /// `bundler fetch-engine`) — the baseline `check_engine_update`/
    /// `install_engine_update` compare against before any in-app engine update
    /// has ever been downloaded. When a dev/test `OPENPAL_ENGINE_DIR` override
    /// is active, its own `engine-version.json` is used instead (the update
    /// check should baseline against the engine actually in use, e.g. b11454,
    /// not the shipped b11429 the override replaced).
    pub fn bundled_engine_version_tag(&self) -> Option<String> {
        if let Some(override_binary) = engine_override_binary() {
            if let Some(tag) = read_engine_version_tag(override_binary.parent()?) {
                return Some(tag);
            }
        }
        read_engine_version_tag(self.bundled_engine_binary.as_ref()?.parent()?)
    }
}

/// Managed from the moment `setup()` starts until the process exits —
/// Phase 1 (pre-unlock, no crypto) builds and manages this; nothing about
/// it ever changes once minted except `needs_recovery`. Commands that must
/// work before a key exists (`get_app_info`, `generate_recovery_code`,
/// `attempt_recovery_unlock`) take `State<'_, BootState>`; everything else
/// takes `State<'_, AppState>`, which isn't managed at all until
/// [`finish_unlock`] succeeds — see `crate::lib`'s `setup()` for why that
/// alone is enough to keep the other ~93 commands from touching a store
/// before a key is available, with no per-command gate needed (Tauri's
/// `State<T>` extractor fails a command cleanly with a catchable error when
/// `T` isn't managed yet, rather than panicking).
pub struct BootState {
    pub paths: Arc<AppPaths>,
    /// Set when `bootstrap::mint_or_unlock` returned
    /// [`crate::bootstrap::KeyStatus::NeedsRecovery`]: the OS keychain
    /// couldn't produce the master key on this launch, but a recovery slot
    /// exists to fall back to. Read by `commands::get_app_info` (drives the
    /// frontend's recovery-code prompt) and cleared by
    /// `commands::attempt_recovery_unlock` on success. Always `false` in
    /// portable mode, which never calls `mint_or_unlock` at all.
    pub needs_recovery: AtomicBool,
    /// Whether the legacy on-disk `<data>/skills` store existed at boot —
    /// Phase 1 no longer creates this directory (nothing writes skill
    /// content to the filesystem anymore), so this is a pure existence
    /// check. `finish_unlock` uses it together with whether `app_db` was
    /// just created to decide: migrate the legacy skills, seed the starter
    /// skill (a genuinely first-ever run), or do neither (an ordinary
    /// relaunch) — see its own doc comment.
    legacy_skills_dir_existed: bool,
    /// Keeps the non-blocking log writer's worker thread alive for the whole
    /// run and flushes buffered lines on shutdown. Never read directly — held
    /// only for its `Drop`. `None` if file logging couldn't start.
    _log_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
    /// The live-swappable handle behind the one `tracing` subscriber
    /// installed in Phase 1 — see `crate::logging`'s own doc comment.
    /// `finish_unlock` calls `logging::upgrade_to_encrypted` on this to
    /// move from the pre-unlock plaintext `boot.log` to the real,
    /// possibly-encrypted app log, once a DEK (or its absence, in
    /// portable mode) is known.
    pub log_writer: crate::logging::LogWriter,
    /// Refuses a second instance against this data dir. Never read
    /// directly — held only for its `Drop`, which removes `<data>/.lock`.
    _instance_lock: openpal_core::boot::lock::InstanceLock,
}

impl BootState {
    /// Phase 1, pre-unlock, no crypto: resolve every filesystem path, take
    /// the instance lock, and bring up file logging — including the panic
    /// hook, installed as a side effect of `logging::init` — using
    /// `boot.json`'s last-known log level (possibly stale by one launch;
    /// `finish_unlock` writes a fresh copy once `Settings` is readable).
    /// Getting logging running this early, before a key is ever touched, is
    /// what makes a Phase 2 failure a logged, diagnosable event instead of
    /// today's unlogged, unmessaged process death (see `lib.rs`'s `setup()`
    /// for what happens if [`finish_unlock`] fails after this returns).
    pub fn initialize(app: &AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let (data_dir, portable) = resolve_data_dir(&app.path().app_data_dir()?);
        // Before anything else touches this data dir: refuse to run a
        // second instance against it. `acquire` creates `data_dir` itself
        // if needed, so this doesn't have to wait for the directory loop
        // below.
        let instance_lock = openpal_core::boot::lock::acquire(&data_dir.join(".lock"))?;
        let models_dir = data_dir.join("models");
        let skills_dir = data_dir.join("skills");
        let logs_dir = data_dir.join("logs");
        let engine_updates_dir = data_dir.join("engine");
        // Checked before Phase 2 might migrate it — see this field's own doc
        // comment on `BootState`. Deliberately *not* created below anymore:
        // nothing writes skill content to the filesystem in steady state.
        let legacy_skills_dir_existed = skills_dir.exists();
        for dir in [&data_dir, &models_dir, &logs_dir, &engine_updates_dir] {
            std::fs::create_dir_all(dir)?;
        }
        let bundled_models_dir = resolve_bundled_models_dir(portable);
        if portable {
            tracing::info!(data_dir = %data_dir.display(), "running in portable mode");
            if let Some(dir) = &bundled_models_dir {
                tracing::info!(bundled_models = %dir.display(), "pre-staged model folder found");
            }
        }

        let manifest = openpal_core::boot::manifest::load(&data_dir.join("boot.json"));
        let (log_guard, log_writer) =
            crate::logging::init(&logs_dir, manifest.log_level.as_deref().unwrap_or("info"));

        let bundled_engine_binary = resolve_engine_binary(app);
        if bundled_engine_binary.is_none() {
            tracing::warn!(
                "llama-server not found — run `cargo run -p openpal-bundler -- fetch-engine` \
                 to stage it for development"
            );
        }

        // A previous launch that crashed or was force-killed never ran its
        // `Child`'s `Drop` (`kill_on_drop` only fires on a clean exit), so
        // its `llama-server` can still be running — sometimes with a full
        // model resident — and starve this launch's own engine start for
        // RAM. Do this before anything else that might start an engine.
        if let Some(engine_binary) = &bundled_engine_binary {
            let killed = openpal_core::boot::orphan_engine::kill_orphaned_engines(engine_binary);
            if !killed.is_empty() {
                tracing::warn!(
                    pids = ?killed,
                    "killed llama-server process(es) orphaned by a previous crash"
                );
            }
        }

        Ok(Self {
            paths: Arc::new(AppPaths {
                data_dir,
                models_dir,
                skills_dir,
                logs_dir,
                portable,
                bundled_models_dir,
                engine_updates_dir,
                bundled_engine_binary,
            }),
            needs_recovery: AtomicBool::new(false),
            legacy_skills_dir_existed,
            _log_guard: log_guard,
            log_writer,
            _instance_lock: instance_lock,
        })
    }
}

/// Phase 2, unlock: derive/hold the DEK, load every store, build and manage
/// `AppState`, and start the tickers that must not run before a store
/// exists to write into (`idle::run`/`parental::run`). Shared by the
/// synchronous happy path (`lib.rs`'s `setup()`, when `mint_or_unlock`
/// returns `Ready` immediately) and `commands::attempt_recovery_unlock`
/// (when it doesn't) — one code path builds `AppState`, so the two triggers
/// can't drift apart. `dek` is `None` only in portable mode; the
/// `NeedsRecovery` case never reaches this function at all — it's handled
/// entirely by setting `BootState::needs_recovery` and waiting for recovery
/// to call this instead.
pub fn finish_unlock(
    app: &AppHandle,
    boot: &BootState,
    dek: Option<DataKey>,
) -> Result<(), Box<dyn std::error::Error>> {
    let paths = boot.paths.clone();

    // Opened, keyed, and migrated first — before anything else in this
    // function, and strictly before `dek` moves into the `AppState` struct
    // literal below (there's no way to recover it after; `DataKey` has no
    // `Clone`). Every store below (settings, custom models, plugin records,
    // and — post-boot — conversations/projects/artifacts) reads and writes
    // through this one connection.
    let app_db_path = paths.data_dir.join("app.sqlite");
    let (app_db_conn, app_db_was_fresh, app_db_version_before) =
        openpal_core::app_db::open(&app_db_path, dek.as_ref())?;
    let app_db = Arc::new(Mutex::new(app_db_conn));

    // Move the app log from the pre-unlock plaintext `boot.log` to its real
    // backend now that a DEK (or its deliberate absence, in portable mode)
    // is known — see `crate::logging`'s own doc comment. `dek` is still
    // borrowable here, strictly before it moves into the `AppState` literal
    // below.
    crate::logging::upgrade_to_encrypted(&boot.log_writer, &paths.logs_dir, dek.as_ref());

    let settings = openpal_core::settings::load(&app_db.lock().unwrap());

    // Only on the run that just created app_db (never on an ordinary
    // relaunch, where the database already has whatever skills — or lack of
    // them — a prior run left it with). Needs `settings.language` for the
    // starter-skill case, so this can only run once settings are loaded
    // above. See `openpal_core::skills::migrate_legacy_skills`'s own doc
    // comment for why a legacy on-disk store is migrated rather than
    // silently orphaned.
    if app_db_was_fresh {
        if boot.legacy_skills_dir_existed {
            openpal_core::skills::migrate_legacy_skills(
                &mut app_db.lock().unwrap(),
                &paths.skills_dir,
            );
        } else {
            openpal_core::skills::seed_starter_skill(
                &mut app_db.lock().unwrap(),
                &settings.language,
            );
        }
    }

    // One-time backfill for conversations saved before `ChatMessage.id`
    // existed — a fresh database has no conversations to backfill, so this
    // only ever does real work on a pre-existing database crossing schema
    // v11. See `openpal_core::store::backfill_message_ids`'s doc comment for
    // why this explicit, persisted, run-once step is required rather than
    // relying on `ChatMessage.id`'s `#[serde(default)]` alone.
    if app_db_version_before < 11 {
        openpal_core::store::backfill_message_ids(&mut app_db.lock().unwrap())?;
    }

    let parental_config_path = paths.data_dir.join("parental-config.json");
    let parental_config = openpal_core::parental::load_config(&parental_config_path);
    let parental_state_path = paths.data_dir.join("parental-state.json");
    let parental_state = openpal_core::parental::load_state(&parental_state_path);

    let plugin_dirs = resolve_plugin_dirs(app, &paths.data_dir, paths.portable);
    let plugins = Arc::new(PluginManager::new(
        plugin_dirs,
        paths.data_dir.join("plugin-data"),
        app_db.clone(),
    ));
    plugins.set_experimental_features(settings.experimental_features);

    let custom = openpal_core::custom::load(&app_db.lock().unwrap());
    let mut hardware = openpal_core::hardware::detect(&paths.models_dir);
    // Ask the engine itself which GPU (if any) its Vulkan backend sees —
    // Windows/Linux only; on macOS acceleration is Metal (`has_metal`)
    // and the bundled build has no Vulkan backend to ask about.
    if !cfg!(target_os = "macos") {
        if let Some(binary) = paths.engine_binary() {
            if let Some(gpu) = openpal_core::gpu::detect_vulkan_gpu(&binary) {
                tracing::info!(
                    gpu = %gpu.name,
                    vram_mb = gpu.vram_bytes / (1024 * 1024),
                    "vulkan gpu detected"
                );
                hardware.gpu_name = Some(gpu.name);
                hardware.gpu_vram_bytes = Some(gpu.vram_bytes);
            }
        }
    }
    tracing::info!(
        chip = %hardware.chip,
        ram_gb = hardware.total_ram_bytes / (1024 * 1024 * 1024),
        "hardware detected"
    );

    // REST-call client shared by model-browsing (Hugging Face), cloud API
    // calls, and downloads: a 20 s connect + 30 s read timeout is right for
    // those. **Do NOT use it to stream an LLM SSE response** — the 30 s
    // `read_timeout` is applied per body read and cannot be overridden
    // per-request, so a streaming turn (agent/tool loop, cloud chat) whose
    // server idles between chunks for 30 s (prompt processing on a
    // disk-streamed model is measured in minutes; generation gaps at
    // ~0.05–0.08 tok/s are 12–20 s) is cut off mid-stream with
    // `error decoding response body`. The turn paths build their own
    // client via `agent_bridge::turn_http_client` (no read timeout) — keep
    // it that way.
    let http = reqwest::Client::builder()
        .user_agent(concat!("OpenPal/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(30))
        .build()?;

    // `boot.json` is unencrypted and meant to be readable pre-unlock, so the
    // *next* launch's Phase 1 sees today's language/log level rather than
    // the ones it started with — the concrete fix for `boot::manifest`'s
    // own "renders in English forever" scenario. This is a write-back for
    // next time, not a live reconfiguration: `logging::init` sets the
    // process's global tracing subscriber once, so this launch keeps
    // whichever level Phase 1 already loaded.
    let boot_json_path = paths.data_dir.join("boot.json");
    let mut manifest = openpal_core::boot::manifest::load(&boot_json_path);
    manifest.language = Some(settings.language.clone());
    manifest.log_level = Some(settings.log_level.clone());
    if let Err(e) = openpal_core::boot::manifest::save(&boot_json_path, &manifest) {
        tracing::warn!("couldn't write boot.json: {e}");
    }

    let logs_dir = paths.logs_dir.clone();
    // Built before the `AppState` literal below, strictly before `dek` moves
    // into it (`DataKey` has no `Clone`) — `set_dek` only ever needs a
    // borrow, deriving and keeping its own purpose-scoped subkey rather than
    // holding onto the raw DEK itself. See `EngineManager::set_dek`'s own
    // doc comment.
    // Built on every platform — mirrors `remote: RemoteAccessManager`'s own
    // "present everywhere, only ever meaningfully used where the matching
    // UI exists" precedent (here: Android's pairing screen). Keyed
    // unconditionally so its own connectivity log
    // (`RemoteBackend::export_logs`) is never left permanently plaintext
    // even where nothing pairs with it yet.
    let remote_backend = RemoteBackend::new(
        paths.data_dir.join("remote-pairing.json"),
        logs_dir.clone(),
        crate::secret_store::store(),
    );
    remote_backend.set_dek(dek.as_ref());

    // Android's W^X policy forbids exec'ing the bundled `llama-server`
    // binary the way `SubprocessBackend` does — `RemoteBackend` (a paired
    // desktop's engine, over the Remote Access gateway) is the only backend
    // that can work there at all; see `docs/design/MOBILE.md`.
    #[cfg(not(target_os = "android"))]
    let engine = {
        let engine = Arc::new(EngineManager::new(logs_dir.clone()));
        engine.set_dek(dek.as_ref());
        engine
    };
    #[cfg(target_os = "android")]
    let engine = Arc::new(EngineManager::with_backend(Box::new(
        remote_backend.clone(),
    )));

    // This device's per-launch HLC for Phase 3 multi-device sync (see
    // `AppState::sync_clock`'s own doc comment) — seeded with this device's
    // real sync roster id when one already exists (a device that already
    // created/joined a sync group in a previous run), or a placeholder when
    // it doesn't. `sync::hlc::HlcClock`'s own doc comment is explicit that a
    // fresh clock at each launch is fine, so this one-time construction
    // (never rebuilt mid-run) matches its intended lifetime exactly.
    let sync_clock = {
        let conn = app_db.lock().unwrap();
        let device_id = roster::list(&conn)
            .into_iter()
            .find(|entry| entry.is_self)
            .map(|entry| entry.device_id)
            .unwrap_or_else(|| "unpaired".to_string());
        HlcClock::new(device_id)
    };

    let remote = RemoteAccessManager::new(app_db.clone(), engine.clone());
    // Honor a persisted `Settings::sync_relay_url` override at startup, not
    // just `update_settings`'s live `set_base_url` call on a later change —
    // `commands::update_settings` already validates on save, but a
    // persisted value could predate that check (an older build, or a
    // hand-edited settings file), so re-validate here too and fall back to
    // the default with a loud log rather than constructing a client
    // against something that will just fail every request.
    let relay_url = settings
        .sync_relay_url
        .clone()
        .and_then(|url| match validate_relay_url(&url) {
            Ok(()) => Some(url),
            Err(e) => {
                tracing::error!("persisted sync relay URL {url:?} is invalid ({e}); using the default relay instead");
                None
            }
        })
        .unwrap_or_else(relay_base_url);
    let injection = Arc::new(openpal_core::injection::Detector::new(
        paths.data_dir.join("prompt-guard"),
    ));
    let injection_http = http.clone();
    app.manage(AppState {
        injection: injection.clone(),
        engine,
        remote,
        remote_backend,
        remote_discovery: tokio::sync::Mutex::new(None),
        plugins,
        account: AccountManager::new(
            paths.data_dir.join("account.json"),
            crate::secret_store::store(),
        ),
        pairing: PairingManager::new(
            MailboxClient::new(relay_url),
            paths.data_dir.join("keys"),
            crate::secret_store::store(),
        ),
        agent_consent: Arc::new(ConsentBroker::new(paths.data_dir.join("agent-grants.json"))),
        // Rides `app_db`'s own PRAGMA key — see `crate::agent::audit`'s own
        // doc comment for why the hash-chained log needs no key of its own
        // once it's a table there instead of a standalone file.
        agent_audit: Arc::new(AuditLog::open(app_db.clone())),
        paths,
        catalog: Catalog::load(),
        hardware,
        settings: tokio::sync::Mutex::new(settings),
        parental_config: tokio::sync::Mutex::new(parental_config),
        parental_config_path,
        parental_state: tokio::sync::Mutex::new(parental_state),
        parental_state_path,
        custom: tokio::sync::Mutex::new(custom),
        downloads: tokio::sync::Mutex::new(HashMap::new()),
        generation: GenerationQueue::default(),
        dictation_stop: tokio::sync::Mutex::new(None),
        audio: AudioPlayer::new(),
        http,
        disk_speed_cache: tokio::sync::Mutex::new(None),
        hf_details_cache: tokio::sync::Mutex::new(HashMap::new()),
        hf_listing_cache: tokio::sync::Mutex::new(HashMap::new()),
        gguf_meta_cache: tokio::sync::Mutex::new(HashMap::new()),
        dek,
        app_db,
        sync_clock,
    });

    tauri::async_runtime::spawn(crate::idle::run(app.clone()));
    tauri::async_runtime::spawn(crate::parental::run(app.clone()));
    tauri::async_runtime::spawn(crate::backup::run(app.clone()));
    // After `AppState` exists, like the two above: its first tick runs at
    // once, so "Delete old conversations" applies on every unlock, not only
    // after the app has stayed open for an interval.
    tauri::async_runtime::spawn(crate::retention::run(app.clone()));
    // Downloads the classifier if it's missing (silently, retrying until it
    // works) on every desktop install, so it's there the moment someone
    // turns agent mode or web search on. The first tool turn loads it
    // (`commands::chat_stream`); `crate::idle` releases it.
    if wants_classifier(std::env::consts::OS) {
        tauri::async_runtime::spawn(injection.download_until_present(injection_http));
    }
    tauri::async_runtime::spawn(crate::sync_loop::run(app.clone()));
    // Resume Remote Access if it was left on — mirrors how an already-
    // enabled plugin service resumes on launch (`plugins/PLUGINS.md` §4).
    // Spawned (not awaited here) since `finish_unlock` itself is
    // synchronous and this involves real I/O (TLS cert generation on first
    // use, binding a socket).
    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(state) = app_handle.try_state::<AppState>() {
            state.remote.resume_if_enabled().await;
        }
    });
    Ok(())
}

pub struct AppState {
    pub paths: Arc<AppPaths>,
    pub catalog: Catalog,
    /// Snapshot taken at startup. Disk space is re-detected before downloads;
    /// chip/RAM don't change while we run.
    pub hardware: HardwareInfo,
    pub settings: tokio::sync::Mutex<Settings>,
    /// Parental-controls configuration (schedule/caps/PIN) — kept out of
    /// `Settings` so its `pin_hash`/`pin_salt` never rides along on
    /// `get_settings`/`update_settings`. See `commands::set_parental_controls`.
    pub parental_config: tokio::sync::Mutex<ParentalConfig>,
    pub parental_config_path: PathBuf,
    /// Today's running usage-seconds/messages-sent counters, rolled over at
    /// local midnight. Written far more often than `parental_config`
    /// (`parental::run`'s accrual ticker, every sent message), hence a
    /// separate file.
    pub parental_state: tokio::sync::Mutex<ParentalState>,
    pub parental_state_path: PathBuf,
    /// Models the user added themselves (Hugging Face picks / local files).
    pub custom: tokio::sync::Mutex<CustomModels>,
    /// `Arc`-wrapped so `remote: RemoteAccessManager`'s gateway (a spawned,
    /// long-lived task, unlike a per-command borrow of `AppState`) can hold
    /// its own cheap clone to proxy chat completions through — see
    /// `RemoteAccessManager::new`.
    pub engine: Arc<EngineManager>,
    /// Remote Access: pairing, LAN discovery, wake-on-LAN, and the gateway
    /// that proxies chat completions to a paired device. Off by default
    /// (Settings → Devices); see `openpal_core::remote`'s own doc comment.
    pub remote: RemoteAccessManager,
    /// The other side of Remote Access: this device pairing with (and
    /// chatting through) *someone else's* gateway. On Android this is also
    /// what `engine` is boxed around (see `finish_unlock`) — held here too,
    /// separately, for the pairing-specific commands (`remote_pair`,
    /// `remote_forget`, `remote_client_status`) that aren't part of the
    /// generic `EngineBackend` surface `engine` exposes. Present on every
    /// platform (cheap — no local model, no subprocess) but only ever
    /// paired from Android's Settings UI today; see `docs/design/MOBILE.md`.
    pub remote_backend: RemoteBackend,
    /// The active LAN-discovery browse session, if any
    /// (`commands::remote_discover_hosts`/`remote_stop_discovery`).
    /// Present on every platform, harmlessly unused where no UI starts one
    /// — same precedent as `remote`/`remote_backend` above.
    pub remote_discovery: tokio::sync::Mutex<Option<Browser>>,
    /// Enable/disable, setup, and services for installed plugins. `Arc`-
    /// wrapped (like `agent_consent`/`agent_audit`) so a tool such as
    /// `SpawnSubagentTool` can hold its own cheap clone across a turn's
    /// `await` points, without borrowing `AppState`, to lazily start e.g.
    /// the web-search service (see
    /// `PluginManager::ensure_capability_service`) only if it actually gets
    /// used.
    pub plugins: Arc<PluginManager>,
    /// Sign-in state and cached premium/cloud-model entitlements.
    pub account: AccountManager,
    /// Device pairing/rotation state for multi-device conversation sync
    /// (`docs/SYNC_PAIRING_PROTOCOL.md`, Phase 2). Talks to a sync relay
    /// (`openpal_core::sync::mailbox_client::relay_base_url` — no deployed
    /// default instance exists yet, see that function's doc comment) over
    /// HTTP; never touches the webview directly, same boundary every other
    /// network-capable manager on this struct respects.
    pub pairing: PairingManager<MailboxClient>,
    /// Agent mode's pending per-call consent prompts + persisted "Always
    /// allow" grants. Long-lived (unlike the rest of an agent turn's
    /// dependencies, which `agent_bridge::build_deps` assembles fresh per
    /// turn) because grants and in-flight prompts must survive across
    /// turns and be reachable from the Settings "standing permissions" UI.
    /// `Arc`-wrapped so a turn's `AgentDeps` can hold its own cheap clone
    /// without borrowing `AppState` for the duration of the turn. Also holds
    /// the prompt-injection review cards of agent and web-search turns
    /// (`respond_injection_review`).
    pub agent_consent: Arc<ConsentBroker>,
    /// Agent mode's hash-chained tool-call lifecycle log.
    pub agent_audit: Arc<AuditLog>,
    /// The always-on prompt-injection detector (`openpal_core::injection`) —
    /// a knowing exception to mono-model first, with no setting to turn it
    /// off. Its classifier downloads in the background after unlock on
    /// desktop (see `finish_unlock`), loads on the first scan of a turn that
    /// uses tools, and is released once idle or with the local chat model;
    /// each such turn wraps it in its own `TurnGuard`, which holds it loaded.
    pub injection: Arc<openpal_core::injection::Detector>,
    /// model_id -> cancel flag for in-flight downloads.
    pub downloads: tokio::sync::Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// The single queue every request that uses the model waits in — chat
    /// turns, Writing Tools, the Message improver, the safety classifier.
    /// See `crate::generation`.
    pub generation: GenerationQueue,
    /// Stop flag for an in-flight voice-typing recording, if any (set by
    /// `stop_dictation` to end recording and transcribe what was captured).
    pub dictation_stop: tokio::sync::Mutex<Option<Arc<AtomicBool>>>,
    /// Native audio playback for the Voice plugin's "read aloud".
    pub audio: AudioPlayer,
    pub http: reqwest::Client,
    /// The models volume's measured write speed, probed once and cached for
    /// the rest of this run (`paths.models_dir` never changes without a
    /// relaunch — see `commands::cached_volume_speed`). Holding the lock
    /// across the probe itself also serializes concurrent probe requests
    /// instead of letting them race and contend for the same disk I/O.
    pub disk_speed_cache: tokio::sync::Mutex<Option<DiskSpeedInfo>>,
    /// Short-TTL cache for Hugging Face repo metadata, keyed by repo id. Same
    /// held-lock-across-the-fetch single-flight trick as `disk_speed_cache`:
    /// the Discover model page fires `get_hf_model_details` and
    /// `get_hf_quant_picker` (which also needs the repo's `gguf.total`
    /// params) at the same time, so without this every page open sent two
    /// identical `/api/models/{repo}` requests. Whichever call reaches the
    /// lock first does the real fetch; the other blocks briefly and then
    /// gets the just-cached result instead of a second request to Hugging
    /// Face. See `commands::cached_hf_details`. TTL matches HF's 5-minute
    /// rate-limit window (`commands::HF_CACHE_TTL`).
    pub hf_details_cache: tokio::sync::Mutex<HashMap<String, (std::time::Instant, HfModelDetails)>>,
    /// Same idea for the repo file tree (`list_gguf_files`), keyed by
    /// (repo id, revision) — reused across Discover's quant picker and the
    /// SettingsSheet "add your own model" listing. Never used by
    /// `add_hf_model` itself, which always re-fetches fresh right before a
    /// download is committed to. See `commands::cached_hf_listing`.
    pub hf_listing_cache:
        tokio::sync::Mutex<HashMap<(String, String), (std::time::Instant, HfListing)>>,
    /// Short-TTL cache for a repo file's own parsed GGUF header (real
    /// MoE-ness and exact parameter counts, see `openpal_core::gguf_meta`),
    /// keyed by (repo id, revision, file path). Same held-lock single-flight
    /// shape as `hf_listing_cache`/`hf_details_cache`. `get_hf_quant_picker`
    /// only ever fetches this once per repo (the smallest file — MoE-ness
    /// and parameter shape are shared across every quant of the same repo),
    /// so a revisit within the TTL avoids a second range-fetch. See
    /// `commands::cached_gguf_meta`.
    #[allow(clippy::type_complexity)]
    pub gguf_meta_cache:
        tokio::sync::Mutex<HashMap<(String, String, String), (std::time::Instant, GgufMeta)>>,
    /// The unwrapped data-encryption key, once `bootstrap::mint_or_unlock` (or
    /// `commands::attempt_recovery_unlock`) has produced one — the only thing
    /// any SQLCipher connection ever sees, via `PRAGMA key`. `None` only in
    /// portable mode (which never calls `mint_or_unlock`); `AppState` isn't
    /// built at all while a key is still outstanding (see `BootState`'s doc
    /// comment), so there's no "not unlocked yet" case to represent here.
    /// `DataKey` has no `Clone` by design — every consumer borrows this as
    /// `state.dek.as_ref()`, never takes ownership.
    pub dek: Option<DataKey>,
    /// The app-wide SQLCipher database (`<data>/app.sqlite`) — conversations,
    /// projects, artifacts, settings, custom models, and plugin records.
    /// Opened, keyed, and migrated once in `finish_unlock`, before `dek`
    /// moves into this struct. A plain `Arc<Mutex<Connection>>`, not a cache
    /// keyed by anything — there is exactly one of these. Nothing here is
    /// model-callable, so every access goes through narrow, fixed-shape
    /// functions in `openpal_core::store`/`projects`/`artifacts`/`settings`/
    /// `custom`/`plugin` instead. `std::sync::Mutex`, not
    /// `tokio::sync::Mutex`: every command touching this store is (or must
    /// become) a short, synchronous lock, never held across an `.await`.
    pub app_db: Arc<Mutex<Connection>>,
    /// This device's per-launch Hybrid Logical Clock for Phase 3 multi-device
    /// sync change capture (`openpal_core::sync::hlc`, `openpal_core::sync::
    /// capture`) — long-lived (constructed once here, never rebuilt per
    /// call) rather than a fresh clock per command, so its logical counter
    /// is meaningful across a whole run; `HlcClock`'s own doc comment
    /// confirms a fresh clock at each *launch* (not per call) is fine.
    /// Seeded from this device's real sync roster id
    /// (`sync::roster::list`'s `is_self` row) when a sync group already
    /// exists; falls back to a placeholder `"unpaired"` device id
    /// otherwise. That placeholder is never actually load-bearing:
    /// `Settings::sync_enabled` (the gate every sync-aware write path
    /// checks) can't be meaningfully turned on before a sync group exists,
    /// and creating/joining one only takes effect for this field on the
    /// *next* launch (this clock isn't rebuilt mid-run) — acceptable per
    /// the same "don't over-engineer a placeholder" call this field's
    /// construction site documents.
    pub sync_clock: HlcClock,
}

/// Marker file a Portable distribution ships next to the executable (beside the
/// `.app` on macOS). Its presence — or the `OPENPAL_PORTABLE` env var, handy for
/// dev testing — switches the app into portable mode.
const PORTABLE_MARKER: &str = "openpal-portable";

/// Whether this OS downloads the prompt-injection classifier (302 MB): every
/// desktop one, not Android, where no turn uses tools — the only turns it
/// checks. Takes the OS name (`std::env::consts::OS`) so both answers are
/// testable on a desktop build.
fn wants_classifier(os: &str) -> bool {
    os != "android"
}

/// Resolve the data directory and whether this is a Portable build. Portable
/// mode keeps everything (models, conversations, settings, logs, plugin data) in
/// a `data/` folder next to the executable, so the app leaves no trace on the
/// host and can run from a USB stick. If that sibling folder isn't writable
/// (e.g. the build sits in a read-only location), it falls back to the OS
/// app-data dir for storage but stays `portable = true`, so the UI still
/// presents the portable experience (no built-in plugins).
fn resolve_data_dir(app_data: &Path) -> (PathBuf, bool) {
    let Some(root) = portable_root() else {
        return (app_data.to_path_buf(), false);
    };
    let triggered =
        std::env::var_os("OPENPAL_PORTABLE").is_some() || root.join(PORTABLE_MARKER).exists();
    if !triggered {
        return (app_data.to_path_buf(), false);
    }
    let data = root.join("data");
    if std::fs::create_dir_all(&data).is_ok() && is_writable(&data) {
        (data, true)
    } else {
        tracing::warn!(
            "portable marker found but {} is not writable — keeping data in the app-data dir",
            data.display()
        );
        (app_data.to_path_buf(), true)
    }
}

/// The Portable distribution's own swappable unit — a directory (macOS
/// `.app`, the flat Windows folder) or a single file (Linux `.AppImage`) —
/// used by `openpal_core::update_portable` to apply an in-place update.
/// Resolved the same way as [`portable_root`] (in fact reuses it) since the
/// swap target and the marker/`data/` sibling must describe the exact same
/// layout; `None` when [`portable_root`] itself can't be resolved (e.g. the
/// portable executable's own location couldn't be determined).
pub(crate) fn portable_install_path() -> Option<PathBuf> {
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        return Some(PathBuf::from(appimage));
    }
    let root = portable_root()?;
    if cfg!(target_os = "macos") {
        Some(root.join("OpenPal.app"))
    } else {
        Some(root)
    }
}

/// The folder a Portable distribution's marker and `data/` live in: next to the
/// executable, or — on macOS, where the executable is buried in the bundle —
/// beside the `.app`. Uses `current_exe()`, never the current working directory
/// (which is `/` when a macOS `.app` is double-clicked).
fn portable_root() -> Option<PathBuf> {
    // AppImage (Linux): the executable runs from a read-only mount, so the
    // distributable is the `.AppImage` file itself — its path is in `$APPIMAGE`,
    // and portable data lives beside it.
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        let appimage = PathBuf::from(appimage);
        return appimage.parent().map(Path::to_path_buf);
    }
    let exe = std::env::current_exe().ok()?;
    #[cfg(target_os = "macos")]
    {
        // A packaged app lives at `…/OpenPal.app/Contents/MacOS/<exe>`; when that
        // is the shape, the portable root is the folder containing the `.app`.
        let in_bundle = exe
            .ancestors()
            .nth(3)
            .is_some_and(|p| p.extension().is_some_and(|e| e == "app"));
        if in_bundle {
            return exe.ancestors().nth(4).map(Path::to_path_buf);
        }
    }
    exe.parent().map(Path::to_path_buf)
}

/// A "Portable + Model" distribution stages an optional read-only GGUF in a
/// `models/` folder beside the executable (a sibling of the marker; see
/// `bundler portable --model`). Return it when present so the app can find and
/// run that model in addition to anything the user downloads into the writable
/// models dir — crucially this still works when the medium is read-only and the
/// writable data dir has fallen back to the OS app-data location. Only consulted
/// in portable mode; a normal install never ships this folder.
fn resolve_bundled_models_dir(portable: bool) -> Option<PathBuf> {
    if !portable {
        return None;
    }
    let dir = portable_root()?.join("models");
    dir.is_dir().then_some(dir)
}

/// Best-effort writability probe: can we create (then remove) a file in `dir`?
fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(".openpal-write-test");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// File name of the staged llama.cpp server binary (`.exe` on Windows).
#[cfg(windows)]
const ENGINE_BINARY: &str = "llama-server.exe";
#[cfg(not(windows))]
const ENGINE_BINARY: &str = "llama-server";

/// Dev/test escape hatch for swapping the engine without touching the shipped
/// build (or an in-app downloaded update): when `OPENPAL_ENGINE_DIR` names a
/// directory whose `llama-server`[.exe] exists, that binary wins in
/// [`AppPaths::engine_binary`]. Mirrors the repo's other `OPENPAL_*` dev
/// overrides (`OPENPAL_ENGINE_UPDATE_ENDPOINT`, `OPENPAL_PORTABLE`, …). One
/// workflow — try a newer llama.cpp build (e.g. one that adds Kimi K2 GGUF
/// support) with the app, leaving the shipped engine untouched:
///
/// ```text
/// cargo run -p openpal-bundler -- fetch-engine --tag b11454 --force \
///     --dest "$PWD/target/k2-engine"
/// OPENPAL_ENGINE_DIR="$PWD/target/k2-engine" npm run dev
/// ```
///
/// Unset (the default) → normal engine resolution, so the shipped engine stays
/// the default. Use an absolute path (as above): `tauri dev` runs from
/// `crates/app/`, so a repo-root-relative path would resolve against the wrong
/// cwd. A set-but-missing binary is logged and ignored rather than erroring.
fn engine_override_binary() -> Option<PathBuf> {
    let dir = std::env::var_os(OPENPAL_ENGINE_DIR)?;
    let binary = PathBuf::from(dir).join(ENGINE_BINARY);
    if !binary.is_file() {
        tracing::warn!(
            "{OPENPAL_ENGINE_DIR} is set but {} is not a file — ignoring the override",
            binary.display()
        );
        return None;
    }
    tracing::info!(
        engine = %binary.display(),
        "using {OPENPAL_ENGINE_DIR} engine override"
    );
    Some(binary)
}

/// Environment variable for [`engine_override_binary`].
const OPENPAL_ENGINE_DIR: &str = "OPENPAL_ENGINE_DIR";

/// Read the `tag` field of `engine-version.json` living next to `engine_binary`
/// (the file `bundler fetch-engine` writes). `None` when missing/unparseable —
/// callers fall back to the shipped engine's tag.
fn read_engine_version_tag(engine_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(engine_dir.join("engine-version.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("tag")?.as_str().map(str::to_string)
}

/// The llama-server binary is staged by the bundler module (`bundler/`):
/// packaged app -> `<resource dir>/llama/llama-server[.exe]`
/// (e.g. `<.app>/Contents/Resources/llama/` on macOS, the exe's own folder on
/// Windows, the AppImage mount on Linux); development ->
/// `crates/app/resources/llama/llama-server[.exe]`.
fn resolve_engine_binary(app: &AppHandle) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        candidates.push(resource_dir.join("llama").join(ENGINE_BINARY));
        candidates.push(resource_dir.join("resources/llama").join(ENGINE_BINARY));
    }
    #[cfg(debug_assertions)]
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources/llama")
            .join(ENGINE_BINARY),
    );
    candidates.into_iter().find(|p| p.exists())
}

/// Directories scanned for plugins (each subfolder with a `plugin.json` is one):
/// built-in (`<.app>/Contents/Resources/plugins/` packaged, `<repo>/plugins/`
/// in dev) and the user's own folder under the data dir. Earlier dirs win on id.
///
/// A dev build scans `<repo>/plugins/` first: its resource dir can still hold
/// a copy staged by an earlier `build` (Tauri copies resources into
/// `target/debug/` and never prunes them), which would otherwise win and hide
/// every edit to the plugin sources.
///
/// In portable mode the built-in dirs are skipped: the Python plugins (web
/// search, voice) need a host Python + network and can't be made portable (see
/// docs/design/PORTABLE.md), so nothing built-in ships and the Plugins tab is empty. A
/// power user can still drop a plugin into `<data>/plugins/`.
///
/// Skipped the same way on mobile: `speech`/`web-search` are Python-sidecar
/// plugins (no Python there either), and `hf-browser`/`smart-model-router` are pure
/// Rust but both about managing *locally installed* models, which don't exist
/// on a phone chatting through `RemoteBackend` — none of the 4 built-in
/// plugins do anything useful on Android/iOS. `dirs.push(data_dir...)` stays
/// unconditional below regardless (harmless empty scan — there's no way to
/// drop a custom plugin into a sandboxed mobile app's data dir today anyway).
fn resolve_plugin_dirs(app: &AppHandle, data_dir: &Path, portable: bool) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if !portable && !cfg!(any(target_os = "android", target_os = "ios")) {
        #[cfg(debug_assertions)]
        dirs.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins"));
        if let Ok(resource_dir) = app.path().resource_dir() {
            dirs.push(resource_dir.join("plugins"));
            dirs.push(resource_dir.join("resources/plugins"));
        }
    }
    dirs.push(data_dir.join("plugins"));
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_classifier_downloads_on_desktop_but_not_android() {
        for os in ["macos", "windows", "linux"] {
            assert!(wants_classifier(os), "{os}");
        }
        assert!(!wants_classifier("android"));
    }

    /// `engine_override_binary` reads the process-global `OPENPAL_ENGINE_DIR`,
    /// so the branches are exercised sequentially inside a single `#[test]`
    /// rather than in parallel tests (same `set_var` presence as
    /// `openpal_core`'s `cloud.rs` tests). The guard restores whatever was
    /// there before on drop, whatever path the test takes.
    #[test]
    fn engine_override_follows_the_env_dir_when_present() {
        let previous = std::env::var_os(OPENPAL_ENGINE_DIR);

        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join(ENGINE_BINARY);
        // 1. Unset → no override.
        std::env::remove_var(OPENPAL_ENGINE_DIR);
        assert_eq!(engine_override_binary(), None);

        // 2. Set, but the directory has no engine binary → no override.
        std::env::set_var(OPENPAL_ENGINE_DIR, tmp.path());
        assert_eq!(engine_override_binary(), None);
        assert!(!missing.exists());

        // 3. Set and the binary exists → the override wins.
        std::fs::write(&missing, b"stub").unwrap();
        assert_eq!(engine_override_binary(), Some(missing));

        // 4. `read_engine_version_tag` reads the override dir's
        //    engine-version.json — the update-check baseline tracks whatever
        //    engine is actually in use.
        assert_eq!(read_engine_version_tag(tmp.path()), None);
        std::fs::write(
            tmp.path().join("engine-version.json"),
            r#"{"tag":"b11454","target":"macos-arm64"}"#,
        )
        .unwrap();
        assert_eq!(
            read_engine_version_tag(tmp.path()).as_deref(),
            Some("b11454")
        );

        match previous {
            Some(v) => std::env::set_var(OPENPAL_ENGINE_DIR, v),
            None => std::env::remove_var(OPENPAL_ENGINE_DIR),
        }
    }
}
