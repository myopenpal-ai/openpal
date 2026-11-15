//! A small, runtime plugin framework.
//!
//! A **plugin** is a directory with a `plugin.json` manifest. It can be
//! enabled/disabled, may need a one-time **setup** (create a Python venv,
//! install deps, run a setup script), may run a long-lived **service** (a
//! managed sidecar, like the engine), and may handle a **chat** turn. The full
//! contract — manifest shape, the setup/service/chat process protocols, and the
//! TypeScript mirror — lives in `plugins/PLUGINS.md`.
//!
//! This module is GUI-free: the app layer discovers plugin directories (built-in
//! resources + user folder), hands them to [`PluginManager::new`], and exposes
//! the manager through Tauri commands. Mutable state (venv, downloads, logs)
//! lives under a per-plugin **data dir**, never in the (possibly read-only)
//! plugin directory.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::Instant;

const GENERIC: &str = "Something went wrong with that add-on. Please try again.";
const SERVICE_START_TIMEOUT: Duration = Duration::from_secs(120);

/// Plugin ids that were renamed, as `(old, new)`. A persisted record under
/// the old id is carried over to the new one on load (see
/// [`migrate_renamed_ids`]), so an add-on the user had enabled stays enabled
/// across the rename, and a stale copy of the old plugin (e.g. one dropped
/// into `<data>/plugins/` by hand) is never discovered next to its successor.
const RENAMED_PLUGIN_IDS: &[(&str, &str)] = &[("model-router", "smart-model-router")];

// ---------------------------------------------------------------------------
// Manifest (mirrors plugin.json)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Present for python-runtime plugins (all of them, for now).
    #[serde(default)]
    pub python: Option<PythonRuntime>,
    /// Marks this plugin as core app infrastructure rather than an optional
    /// add-on: it's always treated as enabled (no `plugins.json` record, no
    /// Add-ons toggle), never appears in `list()`/`list_plugins` (there is
    /// nothing for the user to enable), and self-provisions on first actual
    /// use via [`PluginManager::ensure_capability_service`] instead of
    /// waiting for [`PluginManager::set_enabled`]. For a built-in capability
    /// that shouldn't need an enable step but still needs the same one-time
    /// Python setup + managed-service machinery every plugin gets. No shipped
    /// plugin is core today (`web-search` was, until it became an
    /// experimental add-on).
    #[serde(default)]
    pub core: bool,
    /// Puts this plugin behind the Settings → Advanced "Experimental
    /// features" switch: while that's off the plugin behaves as disabled
    /// whatever its record says (see [`PluginManager::experimental`]). A
    /// plugin that graduates drops the flag and keeps its users' choice.
    /// Independent of [`Self::core`]: a core plugin marked experimental is
    /// on exactly while the switch is.
    #[serde(default)]
    pub experimental: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PythonRuntime {
    /// `pip install -r <this>` during setup.
    #[serde(default)]
    pub requirements: Option<String>,
    /// First-run setup script (streams NDJSON progress).
    #[serde(default)]
    pub setup: Option<String>,
    /// Long-lived service launcher (gets `--port`).
    #[serde(default)]
    pub service: Option<String>,
    /// GET path that returns 2xx/3xx once the service is up.
    #[serde(default = "default_health_path")]
    pub service_health_path: String,
    /// Per-turn chat handler.
    #[serde(default)]
    pub chat: Option<String>,
}

fn default_health_path() -> String {
    "/".to_string()
}

// ---------------------------------------------------------------------------
// Status types sent to the UI
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SetupState {
    NotSetup,
    SettingUp {
        message: String,
        fraction: Option<f32>,
    },
    Ready,
    /// `message` is safe to show to a non-technical user.
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ServiceState {
    Stopped,
    Starting,
    Running {
        port: u16,
    },
    Failed {
        message: String,
    },
    /// The plugin declares no service.
    NotApplicable,
}

/// Flat view of one plugin for the UI (`list_plugins`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub author: String,
    pub capabilities: Vec<String>,
    pub enabled: bool,
    pub setup: SetupState,
    pub service: ServiceState,
    pub has_chat: bool,
    /// [`PluginManifest::experimental`]. Only ever `true` while experimental
    /// features are on, since an experimental plugin isn't listed otherwise.
    pub experimental: bool,
}

/// Streamed by `set_plugin_enabled` while enabling.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PluginEvent {
    Setup {
        message: String,
        fraction: Option<f32>,
    },
    Ready,
    /// `message` is safe to show to a non-technical user.
    Failed {
        message: String,
    },
}

impl PluginEvent {
    /// Translate this into the chat-turn event the UI understands, when
    /// there's something worth surfacing mid-turn — only `Setup`'s progress
    /// messages; `Ready`/`Failed` are already reflected by the caller's own
    /// success/error handling around `ensure_capability_service`, so
    /// forwarding them again here would be redundant. Shared by both call
    /// sites (plain-chat web search and the `spawn_subagent` "web-research"
    /// kind) that turn the search service's setup/start progress into a
    /// `ChatEvent::SearchSetupProgress` the chat UI already knows how to show
    /// as a status line.
    pub fn as_search_setup_chat_event(&self) -> Option<crate::types::ChatEvent> {
        match self {
            PluginEvent::Setup { message, .. } => {
                Some(crate::types::ChatEvent::SearchSetupProgress {
                    message: message.clone(),
                })
            }
            _ => None,
        }
    }
}

/// Everything the app needs to drive an enabled chat plugin's per-turn handler.
/// Internal (not serialized).
#[derive(Debug, Clone)]
pub struct ChatPlugin {
    pub id: String,
    /// The plugin venv's python.
    pub python: PathBuf,
    /// Absolute path to the chat script.
    pub script: PathBuf,
    /// Working directory (the plugin dir).
    pub cwd: PathBuf,
    pub data_dir: PathBuf,
    pub setup_ready: bool,
    pub has_service: bool,
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct DiscoveredPlugin {
    pub manifest: PluginManifest,
    pub dir: PathBuf,
}

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginRecord {
    #[serde(default)]
    enabled: bool,
    /// The `manifest.version` setup last completed for; `None` = never set up.
    #[serde(default)]
    setup_version: Option<String>,
}

struct Runtime {
    setup: SetupState,
    service: ServiceState,
    /// The running service child, if any.
    child: Option<tokio::process::Child>,
}

struct Inner {
    records: HashMap<String, PluginRecord>,
    runtime: HashMap<String, Runtime>,
}

pub struct PluginManager {
    plugins: Vec<DiscoveredPlugin>,
    /// `<app-data>/plugin-data` — per-plugin mutable state lives in `<root>/<id>`.
    data_root: PathBuf,
    /// The app-wide database — persisted enable/setup records live in its
    /// `plugin_records` singleton-row table (`crate::app_db`).
    db: Arc<Mutex<Connection>>,
    inner: tokio::sync::Mutex<Inner>,
    client: reqwest::Client,
    /// Serializes the whole check-then-act sequence in
    /// [`Self::ensure_capability_service`] (is it set up? run setup; is it
    /// running? start it) across concurrent callers — unlike `inner`, which
    /// is only ever held across one short step at a time. Without this, two
    /// turns racing to self-provision the same never-yet-set-up core
    /// service could both see "not ready" and run `setup_plugin.py`/venv
    /// creation concurrently into the same data dir.
    core_provision_lock: tokio::sync::Mutex<()>,
    /// The Settings → Advanced "Experimental features" switch
    /// (`Settings.experimental_features`). While it's off, every plugin
    /// marked [experimental](PluginManifest::experimental) behaves as
    /// disabled whatever its persisted record says: absent from
    /// [`Self::list`], never active, never enableable. Other plugins don't
    /// depend on it. The records themselves are left untouched, so turning
    /// it back on restores each plugin as it was. Off until the app layer
    /// calls [`Self::set_experimental_features`].
    experimental: AtomicBool,
}

impl PluginManager {
    /// Discover plugins from `plugin_dirs` (first id wins) and load persisted
    /// enable/setup records. No processes are started here.
    pub fn new(plugin_dirs: Vec<PathBuf>, data_root: PathBuf, db: Arc<Mutex<Connection>>) -> Self {
        let mut plugins = discover(&plugin_dirs);
        plugins.retain(|p| {
            !RENAMED_PLUGIN_IDS
                .iter()
                .any(|(old, _)| *old == p.manifest.id)
        });
        let mut records = load_records(&db.lock().unwrap());
        if migrate_renamed_ids(&mut records) {
            save_records(&db.lock().unwrap(), &records);
        }
        let mut runtime = HashMap::new();
        for p in &plugins {
            let setup = if is_set_up(&records, &data_root, p) {
                SetupState::Ready
            } else {
                SetupState::NotSetup
            };
            let service = if has_service(p) {
                ServiceState::Stopped
            } else {
                ServiceState::NotApplicable
            };
            runtime.insert(
                p.manifest.id.clone(),
                Runtime {
                    setup,
                    service,
                    child: None,
                },
            );
        }
        Self {
            plugins,
            data_root,
            db,
            inner: tokio::sync::Mutex::new(Inner { records, runtime }),
            client: reqwest::Client::new(),
            core_provision_lock: tokio::sync::Mutex::new(()),
            experimental: AtomicBool::new(false),
        }
    }

    /// Apply the Settings → Advanced "Experimental features" switch (see
    /// [`Self::experimental`]). Turning it off doesn't stop anything already
    /// running by itself — follow it with [`Self::stop_experimental_services`].
    pub fn set_experimental_features(&self, on: bool) {
        self.experimental.store(on, Ordering::Relaxed);
    }

    /// Whether `p` is experimental while experimental features are off, so
    /// it must behave as disabled.
    fn gated_off(&self, p: &DiscoveredPlugin) -> bool {
        p.manifest.experimental && !self.experimental.load(Ordering::Relaxed)
    }

    /// Stop every experimental plugin's running service, leaving other
    /// services and every persisted record alone — what turning
    /// "Experimental features" off does to plugins already running.
    pub async fn stop_experimental_services(&self) {
        let ids: Vec<String> = self
            .plugins
            .iter()
            .filter(|p| p.manifest.experimental)
            .map(|p| p.manifest.id.clone())
            .collect();
        for id in ids {
            self.stop_service(&id).await;
        }
    }

    /// Whether `p` is active right now: never while it's
    /// [gated off](Self::gated_off); otherwise a core plugin always is, and
    /// an add-on once the user enabled it.
    fn is_active(&self, records: &HashMap<String, PluginRecord>, p: &DiscoveredPlugin) -> bool {
        if self.gated_off(p) {
            return false;
        }
        p.manifest.core
            || records
                .get(&p.manifest.id)
                .map(|r| r.enabled)
                .unwrap_or(false)
    }

    /// Snapshot of every discovered plugin for the UI. Core plugins (see
    /// [`PluginManifest::core`]) are omitted — there's nothing for the user
    /// to enable/disable, so the Add-ons screen has nothing to show for them.
    /// Experimental plugins are omitted too while experimental features are
    /// off, so no UI surface gated on one (Browse, Writing Tools, the Voice
    /// mic, …) can appear either.
    pub async fn list(&self) -> Vec<PluginInfo> {
        let inner = self.inner.lock().await;
        self.plugins
            .iter()
            .filter(|p| !p.manifest.core && !self.gated_off(p))
            .map(|p| {
                let rec = inner.records.get(&p.manifest.id);
                let rt = inner.runtime.get(&p.manifest.id);
                PluginInfo {
                    id: p.manifest.id.clone(),
                    name: p.manifest.name.clone(),
                    description: p.manifest.description.clone(),
                    version: p.manifest.version.clone(),
                    author: p.manifest.author.clone(),
                    capabilities: p.manifest.capabilities.clone(),
                    enabled: rec.map(|r| r.enabled).unwrap_or(false),
                    setup: rt.map(|r| r.setup.clone()).unwrap_or(SetupState::NotSetup),
                    service: rt
                        .map(|r| r.service.clone())
                        .unwrap_or(ServiceState::NotApplicable),
                    has_chat: has_chat(p),
                    experimental: p.manifest.experimental,
                }
            })
            .collect()
    }

    /// Enable or disable a plugin. Enabling runs setup (streaming progress) if
    /// needed, then starts the service; re-calling with `true` on a failed or
    /// half-set-up plugin retries. Disabling stops the service. Failures are
    /// emitted as `PluginEvent::Failed` *and* returned as `Err`. Enabling an
    /// experimental plugin is refused while experimental features are off.
    pub async fn set_enabled(
        &self,
        id: &str,
        enabled: bool,
        mut on_event: impl FnMut(PluginEvent) + Send,
    ) -> Result<(), String> {
        let plugin = self
            .find(id)
            .cloned()
            .ok_or_else(|| "That add-on isn't installed.".to_string())?;

        if enabled && self.gated_off(&plugin) {
            let message =
                "Turn on experimental features in Settings to use this add-on.".to_string();
            on_event(PluginEvent::Failed {
                message: message.clone(),
            });
            return Err(message);
        }

        {
            let mut inner = self.inner.lock().await;
            inner.records.entry(id.to_string()).or_default().enabled = enabled;
            save_records(&self.db.lock().unwrap(), &inner.records);
        }

        if !enabled {
            self.stop_service(id).await;
            return Ok(());
        }

        // Setup, if not already done for this version.
        if !self.is_ready(&plugin).await {
            self.set_setup(
                id,
                SetupState::SettingUp {
                    message: "Preparing…".to_string(),
                    fraction: Some(0.0),
                },
            )
            .await;
            if let Err(e) = self.run_setup(&plugin, &mut on_event).await {
                self.set_setup(id, SetupState::Failed { message: e.clone() })
                    .await;
                on_event(PluginEvent::Failed { message: e.clone() });
                return Err(e);
            }
            self.set_setup(id, SetupState::Ready).await;
            let mut inner = self.inner.lock().await;
            inner
                .records
                .entry(id.to_string())
                .or_default()
                .setup_version = Some(plugin.manifest.version.clone());
            save_records(&self.db.lock().unwrap(), &inner.records);
        }

        // Start the service, if the plugin has one.
        if has_service(&plugin) {
            on_event(PluginEvent::Setup {
                message: "Starting…".to_string(),
                fraction: None,
            });
            match self.start_service(&plugin).await {
                Ok(_) => {
                    on_event(PluginEvent::Ready);
                    Ok(())
                }
                Err(e) => {
                    on_event(PluginEvent::Failed { message: e.clone() });
                    Err(e)
                }
            }
        } else {
            on_event(PluginEvent::Ready);
            Ok(())
        }
    }

    /// The enabled, chat-capable plugin (if any). The caller checks
    /// `setup_ready` and calls [`ensure_service`] before driving it.
    pub async fn active_chat_plugin(&self) -> Option<ChatPlugin> {
        let inner = self.inner.lock().await;
        for p in &self.plugins {
            if !has_chat(p) {
                continue;
            }
            if !self.is_active(&inner.records, p) {
                continue;
            }
            let chat = p.manifest.python.as_ref().and_then(|py| py.chat.clone())?;
            let data_dir = self.data_root.join(&p.manifest.id);
            return Some(ChatPlugin {
                id: p.manifest.id.clone(),
                python: venv_python(&self.data_root, &p.manifest.id),
                script: p.dir.join(chat),
                cwd: p.dir.clone(),
                data_dir,
                setup_ready: is_set_up(&inner.records, &self.data_root, p),
                has_service: has_service(p),
            });
        }
        None
    }

    /// The id of the enabled, set-up plugin that advertises `capability`
    /// (e.g. "tts", "stt") and backs it with a service, if any. The caller
    /// drives it directly over its loopback service: get the running port with
    /// [`ensure_service`], then call its HTTP endpoints. This is the
    /// service-capability analogue of [`active_chat_plugin`] (chat plugins are
    /// driven per-turn as a subprocess; service plugins are called over HTTP).
    pub async fn active_service_plugin(&self, capability: &str) -> Option<String> {
        let inner = self.inner.lock().await;
        for p in &self.plugins {
            if !provides(p, capability) {
                continue;
            }
            if !self.is_active(&inner.records, p) || !is_set_up(&inner.records, &self.data_root, p)
            {
                continue;
            }
            return Some(p.manifest.id.clone());
        }
        None
    }

    /// Whether any enabled plugin advertises `capability`, regardless of
    /// whether it backs it with a service — the UI-only capability-flag
    /// pattern (`plugins/PLUGINS.md` §8), usable from Rust call sites too
    /// (not just the TS mirror, e.g. `modelBrowserAvailable`/
    /// `smartModelRouterAvailable` in `store.ts`), unlike [`Self::active_service_plugin`]
    /// which requires `has_service`/`is_set_up`. Used e.g. by
    /// `commands::classify_message` to decide whether the `smart-model-router`
    /// plugin being enabled should keep classification running even when
    /// `Settings.auto_pick_sampling` is off.
    pub async fn capability_enabled(&self, capability: &str) -> bool {
        let inner = self.inner.lock().await;
        self.plugins.iter().any(|p| {
            p.manifest.capabilities.iter().any(|c| c == capability)
                && self.is_active(&inner.records, p)
        })
    }

    /// Make sure the [active](Self::is_active) plugin backing the service
    /// `capability` (e.g. `"search"`) is running, and return its port.
    /// Callers pass the capability rather than an id, so they needn't know
    /// which plugin backs it. An add-on counts once the user enabled it and
    /// its setup finished — [`Self::set_enabled`] runs that setup, and this
    /// never races it with a second one. A [core](PluginManifest::core)
    /// plugin has no enable step, so this self-provisions it instead: runs
    /// its one-time setup first if needed. Either way the service is then
    /// started if it isn't running. `on_event` receives the same
    /// setup-progress events the Add-ons screen would have shown, so a
    /// caller mid-chat-turn can surface "Setting up…" instead of silently
    /// blocking — but only while setup/start is actually happening; an
    /// already-running service returns its port with no events at all, so a
    /// steady-state turn doesn't flash a "Starting…" status it would be
    /// misleading to show.
    ///
    /// Serialized by [`Self::core_provision_lock`] for the whole call, unlike
    /// [`Self::set_enabled`]'s per-step locking: two turns racing to
    /// self-provision the same never-yet-set-up core service is a real
    /// scenario here (there's no "enable" button forcing that to happen at
    /// most once), whereas `set_enabled` is only ever one explicit user
    /// click at a time.
    pub async fn ensure_capability_service(
        &self,
        capability: &str,
        mut on_event: impl FnMut(PluginEvent) + Send,
    ) -> Result<u16, String> {
        let _guard = self.core_provision_lock.lock().await;

        let plugin = {
            let inner = self.inner.lock().await;
            self.plugins
                .iter()
                .find(|p| provides(p, capability) && self.is_active(&inner.records, p))
                .cloned()
        }
        .ok_or_else(|| "Turn this add-on on in Plugins first.".to_string())?;
        let id = plugin.manifest.id.clone();

        if !self.is_ready(&plugin).await {
            if !plugin.manifest.core {
                return Err("This add-on is still setting up. Try again in a moment.".to_string());
            }
            self.set_setup(
                &id,
                SetupState::SettingUp {
                    message: "Preparing…".to_string(),
                    fraction: Some(0.0),
                },
            )
            .await;
            if let Err(e) = self.run_setup(&plugin, &mut on_event).await {
                self.set_setup(&id, SetupState::Failed { message: e.clone() })
                    .await;
                on_event(PluginEvent::Failed { message: e.clone() });
                return Err(e);
            }
            self.set_setup(&id, SetupState::Ready).await;
            let mut inner = self.inner.lock().await;
            let rec = inner.records.entry(id.clone()).or_default();
            rec.setup_version = Some(plugin.manifest.version.clone());
            rec.enabled = true;
            save_records(&self.db.lock().unwrap(), &inner.records);
            drop(inner);
        }

        let already_running = matches!(
            self.inner
                .lock()
                .await
                .runtime
                .get(&id)
                .map(|r| r.service.clone()),
            Some(ServiceState::Running { .. })
        );
        if !already_running {
            on_event(PluginEvent::Setup {
                message: "Starting…".to_string(),
                fraction: None,
            });
        }
        match self.ensure_service(&id).await {
            Ok(Some(port)) => {
                if !already_running {
                    on_event(PluginEvent::Ready);
                }
                Ok(port)
            }
            // `provides` above already required `has_service`, so a
            // service-capability plugin always has one.
            Ok(None) => Err(GENERIC.to_string()),
            Err(e) => {
                on_event(PluginEvent::Failed { message: e.clone() });
                Err(e)
            }
        }
    }

    /// Make sure the plugin's service is running, starting it if needed.
    /// `Ok(None)` means the plugin has no service.
    pub async fn ensure_service(&self, id: &str) -> Result<Option<u16>, String> {
        let plugin = self.find(id).cloned().ok_or_else(|| GENERIC.to_string())?;
        if !has_service(&plugin) {
            return Ok(None);
        }
        if let Some(ServiceState::Running { port }) = self
            .inner
            .lock()
            .await
            .runtime
            .get(id)
            .map(|r| r.service.clone())
        {
            return Ok(Some(port));
        }
        Ok(Some(self.start_service(&plugin).await?))
    }

    /// Stop every running service (call on app exit).
    pub async fn stop_all(&self) {
        let ids: Vec<String> = self.plugins.iter().map(|p| p.manifest.id.clone()).collect();
        for id in ids {
            self.stop_service(&id).await;
        }
    }

    /// Re-read the persisted records after something other than this
    /// manager rewrote them (a data import, see [`import_records_json`]) —
    /// otherwise the next enable/disable would save the stale in-memory copy
    /// over the import. Re-derives each idle plugin's setup state from the
    /// new records (one mid-setup or failed is left alone) and stops the
    /// service of any plugin the import turned off.
    pub async fn reload_records(&self) {
        let records = load_records(&self.db.lock().unwrap());
        let to_stop: Vec<String> = {
            let mut inner = self.inner.lock().await;
            for p in &self.plugins {
                if let Some(rt) = inner.runtime.get_mut(&p.manifest.id) {
                    if matches!(rt.setup, SetupState::Ready | SetupState::NotSetup) {
                        rt.setup = if is_set_up(&records, &self.data_root, p) {
                            SetupState::Ready
                        } else {
                            SetupState::NotSetup
                        };
                    }
                }
            }
            inner.records = records;
            self.plugins
                .iter()
                .filter(|p| !self.is_active(&inner.records, p))
                .map(|p| p.manifest.id.clone())
                .collect()
        };
        for id in to_stop {
            self.stop_service(&id).await;
        }
    }

    // -- internals ---------------------------------------------------------

    fn find(&self, id: &str) -> Option<&DiscoveredPlugin> {
        self.plugins.iter().find(|p| p.manifest.id == id)
    }

    async fn is_ready(&self, p: &DiscoveredPlugin) -> bool {
        let inner = self.inner.lock().await;
        is_set_up(&inner.records, &self.data_root, p)
    }

    async fn set_setup(&self, id: &str, state: SetupState) {
        let mut inner = self.inner.lock().await;
        if let Some(rt) = inner.runtime.get_mut(id) {
            rt.setup = state;
        }
    }

    async fn set_service(&self, id: &str, state: ServiceState) {
        let mut inner = self.inner.lock().await;
        if let Some(rt) = inner.runtime.get_mut(id) {
            rt.service = state;
        }
    }

    async fn run_setup(
        &self,
        p: &DiscoveredPlugin,
        on_event: &mut (impl FnMut(PluginEvent) + Send),
    ) -> Result<(), String> {
        let id = &p.manifest.id;
        let Some(py) = p.manifest.python.as_ref() else {
            return Ok(()); // nothing to install
        };
        let data_dir = self.data_root.join(id);
        std::fs::create_dir_all(&data_dir).map_err(|e| {
            tracing::error!("plugin {id}: could not create data dir: {e}");
            GENERIC.to_string()
        })?;
        let setup_log = data_dir.join("setup.log");
        let _ = std::fs::File::create(&setup_log); // truncate per setup run
        let venv = data_dir.join("venv");
        let venv_py = venv_python(&self.data_root, id);

        // 1. Python venv.
        if !venv_py.exists() {
            progress(on_event, "Creating its private Python space…", Some(0.1));
            self.set_setup(
                id,
                SetupState::SettingUp {
                    message: "Creating its private Python space…".to_string(),
                    fraction: Some(0.1),
                },
            )
            .await;
            run_step(
                &base_python(),
                &[
                    "-m".into(),
                    "venv".into(),
                    venv.to_string_lossy().into_owned(),
                ],
                &p.dir,
                &setup_log,
            )
            .await
            .map_err(|e| {
                tracing::error!("plugin {id}: venv creation failed: {e}");
                "Couldn't set up the add-on's Python environment. Make sure Python 3 is \
                 installed and try again."
                    .to_string()
            })?;
        }

        // 2. pip requirements.
        if let Some(req) = py.requirements.as_deref() {
            progress(on_event, "Installing its building blocks…", Some(0.25));
            self.set_setup(
                id,
                SetupState::SettingUp {
                    message: "Installing its building blocks…".to_string(),
                    fraction: Some(0.25),
                },
            )
            .await;
            run_step(
                &venv_py,
                &[
                    "-m".into(),
                    "pip".into(),
                    "install".into(),
                    "-r".into(),
                    p.dir.join(req).to_string_lossy().into_owned(),
                ],
                &p.dir,
                &setup_log,
            )
            .await
            .map_err(|e| {
                tracing::error!("plugin {id}: pip install failed: {e}");
                "Couldn't install the add-on's parts. Check your internet connection and \
                 try again."
                    .to_string()
            })?;
        }

        // 3. plugin-specific setup script (streams its own progress).
        if let Some(setup) = py.setup.as_deref() {
            self.run_setup_script(
                &venv_py,
                &p.dir.join(setup),
                &p.dir,
                &data_dir,
                &setup_log,
                on_event,
                id,
            )
            .await?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)] // internal helper; every arg is already a distinct required input, not a candidate for bundling.
    async fn run_setup_script(
        &self,
        python: &Path,
        script: &Path,
        cwd: &Path,
        data_dir: &Path,
        log: &Path,
        on_event: &mut (impl FnMut(PluginEvent) + Send),
        id: &str,
    ) -> Result<(), String> {
        let err = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .map_err(|e| {
                tracing::error!("plugin {id}: open setup log: {e}");
                GENERIC.to_string()
            })?;
        let mut child = Command::new(python)
            .arg(script)
            .arg("--data-dir")
            .arg(data_dir)
            .current_dir(cwd)
            .env("PYTHONUNBUFFERED", "1")
            .env("OPENPAL_PLUGIN_DATA", data_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(err))
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                tracing::error!("plugin {id}: spawn setup script: {e}");
                GENERIC.to_string()
            })?;

        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill().await;
            return Err(GENERIC.to_string());
        };
        let mut lines = BufReader::new(stdout).lines();
        let mut terminal_error: Option<String> = None;
        let mut done = false;

        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                tracing::debug!("plugin {id} setup: non-json line: {line}");
                continue;
            };
            match v.get("type").and_then(Value::as_str) {
                Some("progress") => {
                    let message = v
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Setting up…")
                        .to_string();
                    let fraction = v.get("fraction").and_then(Value::as_f64).map(|f| f as f32);
                    on_event(PluginEvent::Setup {
                        message: message.clone(),
                        fraction,
                    });
                    self.set_setup(id, SetupState::SettingUp { message, fraction })
                        .await;
                }
                Some("log") => {
                    let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or("");
                    tracing::debug!("plugin {id} setup: {msg}");
                }
                Some("done") => done = true,
                Some("error") => {
                    terminal_error = Some(
                        v.get("message")
                            .and_then(Value::as_str)
                            .unwrap_or(GENERIC)
                            .to_string(),
                    );
                }
                _ => {}
            }
        }

        let status = child.wait().await;
        if let Some(message) = terminal_error {
            return Err(message);
        }
        match status {
            Ok(s) if s.success() || done => Ok(()),
            _ => Err("The add-on's setup didn't finish. Please try again.".to_string()),
        }
    }

    /// Spawn the plugin's service on a free loopback port and wait until it's
    /// healthy. Mirrors the engine's start/health-poll lifecycle.
    async fn start_service(&self, p: &DiscoveredPlugin) -> Result<u16, String> {
        let id = &p.manifest.id;
        let py = p
            .manifest
            .python
            .as_ref()
            .ok_or_else(|| GENERIC.to_string())?;
        let service = py.service.as_deref().ok_or_else(|| GENERIC.to_string())?;

        let data_dir = self.data_root.join(id);
        let venv_py = venv_python(&self.data_root, id);
        let log = data_dir.join("service.log");
        let _ = std::fs::File::create(&log); // truncate per start
        let port = free_port().map_err(|e| {
            tracing::error!("plugin {id}: could not pick a port: {e}");
            GENERIC.to_string()
        })?;
        let logf = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .map_err(|e| {
                tracing::error!("plugin {id}: open service log: {e}");
                GENERIC.to_string()
            })?;
        let logf2 = logf.try_clone().map_err(|e| e.to_string())?;

        let child = Command::new(&venv_py)
            .arg(p.dir.join(service))
            .arg("--port")
            .arg(port.to_string())
            .arg("--data-dir")
            .arg(&data_dir)
            .current_dir(&p.dir)
            .env("PYTHONUNBUFFERED", "1")
            .env("OPENPAL_PLUGIN_DATA", &data_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(logf))
            .stderr(Stdio::from(logf2))
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                tracing::error!("plugin {id}: spawn service failed: {e}");
                "Couldn't start the add-on's background service.".to_string()
            })?;
        tracing::info!(plugin = %id, port, "plugin service: starting");

        {
            let mut inner = self.inner.lock().await;
            if let Some(rt) = inner.runtime.get_mut(id) {
                rt.child = Some(child);
                rt.service = ServiceState::Starting;
            }
        }

        let health_url = format!("http://127.0.0.1:{port}{}", py.service_health_path);
        let deadline = Instant::now() + SERVICE_START_TIMEOUT;
        loop {
            // Did the child exit while starting?
            {
                let mut inner = self.inner.lock().await;
                match inner.runtime.get_mut(id).and_then(|rt| rt.child.as_mut()) {
                    Some(child) => {
                        if let Ok(Some(_)) = child.try_wait() {
                            if let Some(rt) = inner.runtime.get_mut(id) {
                                rt.child = None;
                                rt.service = ServiceState::Failed {
                                    message: "It stopped while starting.".to_string(),
                                };
                            }
                            drop(inner);
                            tracing::error!(
                                "plugin {id}: service exited during startup; see {}",
                                log.display()
                            );
                            return Err("The add-on's service stopped while starting. \
                                        Open Add-ons to try again."
                                .to_string());
                        }
                    }
                    None => return Err(GENERIC.to_string()),
                }
            }

            let healthy = matches!(
                self.client.get(&health_url).timeout(Duration::from_secs(3)).send().await,
                Ok(r) if r.status().is_success() || r.status().is_redirection()
            );
            if healthy {
                break;
            }
            if Instant::now() >= deadline {
                self.stop_service(id).await;
                self.set_service(
                    id,
                    ServiceState::Failed {
                        message: "It took too long to start.".to_string(),
                    },
                )
                .await;
                tracing::error!(
                    "plugin {id}: service health timed out; see {}",
                    log.display()
                );
                return Err(
                    "The add-on's service took too long to start. Open Add-ons to \
                            try again."
                        .to_string(),
                );
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }

        self.set_service(id, ServiceState::Running { port }).await;
        tracing::info!(plugin = %id, port, "plugin service: ready");
        Ok(port)
    }

    pub async fn stop_service(&self, id: &str) {
        let child = {
            let mut inner = self.inner.lock().await;
            inner.runtime.get_mut(id).and_then(|rt| rt.child.take())
        };
        if let Some(mut child) = child {
            if let Err(e) = child.kill().await {
                tracing::warn!("plugin {id}: stopping service failed (already gone?): {e}");
            }
            tracing::info!(plugin = %id, "plugin service: stopped");
        }
        let mut inner = self.inner.lock().await;
        if let Some(rt) = inner.runtime.get_mut(id) {
            if !matches!(rt.service, ServiceState::NotApplicable) {
                rt.service = ServiceState::Stopped;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

fn has_service(p: &DiscoveredPlugin) -> bool {
    p.manifest
        .python
        .as_ref()
        .and_then(|py| py.service.as_deref())
        .is_some()
}

fn has_chat(p: &DiscoveredPlugin) -> bool {
    p.manifest.capabilities.iter().any(|c| c == "chat")
        && p.manifest
            .python
            .as_ref()
            .and_then(|py| py.chat.as_deref())
            .is_some()
}

/// Whether the plugin advertises `capability` (e.g. "tts", "stt") and backs it
/// with a service the app can call over loopback HTTP (the Voice plugin).
fn provides(p: &DiscoveredPlugin, capability: &str) -> bool {
    p.manifest.capabilities.iter().any(|c| c == capability) && has_service(p)
}

fn venv_python(data_root: &Path, id: &str) -> PathBuf {
    data_root.join(id).join("venv/bin/python")
}

/// A plugin is "set up" when its recorded setup version matches the manifest and
/// its venv still exists. Python-less plugins need no setup.
fn is_set_up(
    records: &HashMap<String, PluginRecord>,
    data_root: &Path,
    p: &DiscoveredPlugin,
) -> bool {
    if p.manifest.python.is_none() {
        return true;
    }
    let version_matches = records
        .get(&p.manifest.id)
        .and_then(|r| r.setup_version.as_deref())
        == Some(p.manifest.version.as_str());
    version_matches && venv_python(data_root, &p.manifest.id).exists()
}

fn progress(on_event: &mut (impl FnMut(PluginEvent) + Send), message: &str, fraction: Option<f32>) {
    on_event(PluginEvent::Setup {
        message: message.to_string(),
        fraction,
    });
}

/// Run a command to completion, appending its output to `log`. Errors on a
/// non-zero exit.
async fn run_step(program: &Path, args: &[String], cwd: &Path, log: &Path) -> Result<(), String> {
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|e| e.to_string())?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("PYTHONUNBUFFERED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .status()
        .await
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("command exited with {status}"))
    }
}

fn discover(dirs: &[PathBuf]) -> Vec<DiscoveredPlugin> {
    let mut found: Vec<DiscoveredPlugin> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for base in dirs {
        let Ok(entries) = std::fs::read_dir(base) else {
            continue;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            let manifest_path = dir.join("plugin.json");
            if !manifest_path.exists() {
                continue;
            }
            let raw = match std::fs::read_to_string(&manifest_path) {
                Ok(raw) => raw,
                Err(e) => {
                    tracing::warn!("could not read {}: {e}", manifest_path.display());
                    continue;
                }
            };
            match serde_json::from_str::<PluginManifest>(&raw) {
                Ok(manifest) => {
                    if seen.insert(manifest.id.clone()) {
                        found.push(DiscoveredPlugin { manifest, dir });
                    }
                }
                Err(e) => tracing::warn!("bad plugin.json in {}: {e}", dir.display()),
            }
        }
    }
    found.sort_by_key(|a| a.manifest.name.to_lowercase());
    found
}

/// Carry each renamed plugin's persisted record over to its new id (see
/// [`RENAMED_PLUGIN_IDS`]); a record already under the new id wins. Returns
/// whether anything changed, i.e. whether the records need saving.
fn migrate_renamed_ids(records: &mut HashMap<String, PluginRecord>) -> bool {
    let mut changed = false;
    for (old, new) in RENAMED_PLUGIN_IDS {
        if let Some(rec) = records.remove(*old) {
            records.entry((*new).to_string()).or_insert(rec);
            changed = true;
        }
    }
    changed
}

fn load_records(conn: &Connection) -> HashMap<String, PluginRecord> {
    conn.query_row(
        "SELECT json FROM plugin_records WHERE key = 'current'",
        [],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .ok()
    .flatten()
    .and_then(|s| serde_json::from_str(&s).ok())
    .unwrap_or_default()
}

fn save_records(conn: &Connection, records: &HashMap<String, PluginRecord>) {
    let Ok(json) = serde_json::to_string_pretty(records) else {
        return;
    };
    let _ = conn.execute(
        "INSERT INTO plugin_records (key, json) VALUES ('current', ?1)
         ON CONFLICT(key) DO UPDATE SET json = excluded.json",
        params![json],
    );
}

/// Every persisted enable/setup record, as the pretty JSON object (`id` →
/// `{enabled, setupVersion}`) a full data export writes to `plugins.json`
/// (`crate::export`), with how many records it holds.
pub fn export_records_json(conn: &Connection) -> Result<(String, usize), serde_json::Error> {
    let records = load_records(conn);
    Ok((serde_json::to_string_pretty(&records)?, records.len()))
}

/// How many plugins the persisted records say are turned on — what the
/// data export's Plugins checkbox reports it would carry.
pub fn enabled_record_count(conn: &Connection) -> usize {
    load_records(conn).values().filter(|r| r.enabled).count()
}

/// Merge a `plugins.json` written by [`export_records_json`] (on this device
/// or another) into the persisted records, returning how many it carried.
/// Each imported plugin takes the imported `enabled`; plugins absent from
/// it keep theirs. `setupVersion` always stays this device's own: it
/// describes a venv on *this* disk, so a plugin set up elsewhere but never
/// here still reads as not set up and offers setup when turned on. A running
/// [`PluginManager`] keeps its own copy of the records — follow this with
/// [`PluginManager::reload_records`].
pub fn import_records_json(conn: &Connection, json: &str) -> Result<usize, serde_json::Error> {
    let imported: HashMap<String, PluginRecord> = serde_json::from_str(json)?;
    let mut records = load_records(conn);
    for (id, rec) in &imported {
        records.entry(id.clone()).or_default().enabled = rec.enabled;
    }
    migrate_renamed_ids(&mut records);
    save_records(conn, &records);
    Ok(imported.len())
}

fn free_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// The interpreter used to *create* plugin venvs: an explicit `OPENPAL_PYTHON`,
/// else `python3` from PATH.
fn base_python() -> PathBuf {
    if let Ok(p) = std::env::var("OPENPAL_PYTHON") {
        let p = PathBuf::from(p);
        if p.exists() {
            return p;
        }
    }
    PathBuf::from("python3")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// See `crate::store::tests::test_conn`'s own doc comment for why
    /// `.keep()` is used here.
    fn test_conn() -> Arc<Mutex<Connection>> {
        let dir = tempfile::tempdir().unwrap().keep();
        Arc::new(Mutex::new(
            crate::app_db::open(&dir.join("app.sqlite"), None)
                .unwrap()
                .0,
        ))
    }

    /// The shipped web-search manifest must always deserialize into what the
    /// framework expects — this guards the Rust ↔ plugin.json contract.
    #[test]
    fn parses_the_web_search_manifest() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../plugins/web-search/plugin.json"
        );
        let raw = std::fs::read_to_string(path).expect("web-search plugin.json present");
        let m: PluginManifest = serde_json::from_str(&raw).expect("manifest parses");
        assert_eq!(m.id, "web-search");
        assert!(m.capabilities.iter().any(|c| c == "search"));
        let py = m.python.as_ref().expect("has a python runtime");
        // No `chat` handler: web search runs through the native Rust
        // `web_search`/`fetch_url` tools now, not a Python sidecar — this
        // plugin only still launches the local SearXNG service they query.
        assert_eq!(py.chat, None);
        assert_eq!(py.setup.as_deref(), Some("setup_plugin.py"));
        assert_eq!(py.service.as_deref(), Some("service.py"));
        assert_eq!(py.service_health_path, "/healthz");
        assert!(!m.core, "web-search is an add-on the user turns on");
        assert!(m.experimental);

        // The discovered, shipped plugin advertises a service but no chat.
        let p = DiscoveredPlugin {
            manifest: m,
            dir: PathBuf::from("/tmp/x"),
        };
        assert!(!has_chat(&p));
        assert!(has_service(&p));
    }

    /// The shipped speech manifest must deserialize into a service-only plugin
    /// advertising the `tts` + `stt` capabilities (it has no chat handler).
    #[test]
    fn parses_the_speech_manifest() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../plugins/speech/plugin.json"
        );
        let raw = std::fs::read_to_string(path).expect("speech plugin.json present");
        let m: PluginManifest = serde_json::from_str(&raw).expect("manifest parses");
        assert_eq!(m.id, "speech");
        assert!(m.capabilities.iter().any(|c| c == "tts"));
        assert!(m.capabilities.iter().any(|c| c == "stt"));
        let py = m.python.as_ref().expect("has a python runtime");
        assert_eq!(py.service.as_deref(), Some("service.py"));
        assert_eq!(py.service_health_path, "/healthz");
        assert_eq!(py.chat.as_deref(), None);

        let p = DiscoveredPlugin {
            manifest: m,
            dir: PathBuf::from("/tmp/x"),
        };
        assert!(!has_chat(&p), "speech is not a chat plugin");
        assert!(has_service(&p));
        assert!(provides(&p, "tts"));
        assert!(provides(&p, "stt"));
        assert!(!provides(&p, "chat"), "it doesn't advertise chat");
    }

    /// The shipped hf-browser manifest is python-less (see PLUGINS.md §8): a
    /// pure UI-capability flag with no setup/service/chat of its own.
    #[test]
    fn parses_the_hf_browser_manifest() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../plugins/hf-browser/plugin.json"
        );
        let raw = std::fs::read_to_string(path).expect("hf-browser plugin.json present");
        let m: PluginManifest = serde_json::from_str(&raw).expect("manifest parses");
        assert_eq!(m.id, "hf-browser");
        assert!(m.capabilities.iter().any(|c| c == "model-browser"));
        assert!(
            m.python.is_none(),
            "hf-browser is python-less — pure Rust + UI"
        );

        let p = DiscoveredPlugin {
            manifest: m,
            dir: PathBuf::from("/tmp/x"),
        };
        assert!(!has_chat(&p));
        assert!(!has_service(&p));
        // provides() requires a service (§7); a python-less plugin never has
        // one — the UI must gate on capability + enabled directly (§8), not provides().
        assert!(!provides(&p, "model-browser"));
    }

    /// The shipped smart-model-router manifest is also python-less (see
    /// PLUGINS.md §8): a pure UI+Rust capability flag with no setup/service/
    /// chat of its own — same shape as hf-browser, but its capability also
    /// gates a backend decision (`commands::classify_message`) via
    /// `capability_enabled`, not just a UI reveal.
    #[test]
    fn parses_the_smart_model_router_manifest() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../plugins/smart-model-router/plugin.json"
        );
        let raw = std::fs::read_to_string(path).expect("smart-model-router plugin.json present");
        let m: PluginManifest = serde_json::from_str(&raw).expect("manifest parses");
        assert_eq!(m.id, "smart-model-router");
        assert!(m.capabilities.iter().any(|c| c == "smart-model-router"));
        assert!(
            m.python.is_none(),
            "smart-model-router is python-less — pure Rust + UI"
        );

        let p = DiscoveredPlugin {
            manifest: m,
            dir: PathBuf::from("/tmp/x"),
        };
        assert!(!has_chat(&p));
        assert!(!has_service(&p));
        assert!(!provides(&p, "smart-model-router"));
    }

    /// The shipped remote-inference manifest is also python-less (see
    /// PLUGINS.md §8): a pure UI+Rust capability flag with no setup/service/
    /// chat of its own. Its capability gates a backend decision through
    /// `capability_enabled` — not just a UI reveal — like
    /// smart-model-router's: the remote-inference Settings section is
    /// revealed by the UI-side check, while `crates/app`'s
    /// `remote_inference_*` commands route `state.engine` to the paired
    /// device's engine. No network activity at all while the plugin is off.
    #[test]
    fn parses_the_remote_inference_manifest() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../plugins/remote-inference/plugin.json"
        );
        let raw = std::fs::read_to_string(path).expect("remote-inference plugin.json present");
        let m: PluginManifest = serde_json::from_str(&raw).expect("manifest parses");
        assert_eq!(m.id, "remote-inference");
        assert!(m.capabilities.iter().any(|c| c == "remote-inference"));
        assert!(
            m.python.is_none(),
            "remote-inference is python-less — pure Rust + UI"
        );

        let p = DiscoveredPlugin {
            manifest: m,
            dir: PathBuf::from("/tmp/x"),
        };
        assert!(!has_chat(&p));
        assert!(!has_service(&p));
        assert!(!provides(&p, "remote-inference"));
    }

    /// The shipped writing-tools manifest is also python-less (see
    /// PLUGINS.md §8): a pure UI-capability flag with no setup/service/chat
    /// of its own, same shape as smart-model-router. Unlike smart-model-router, though,
    /// its capability gates a UI surface only — per `docs/design/HF_BROWSER.md`'s
    /// decision 2, `translate_stream`/`correct_stream` are net-new Tauri
    /// commands with no other call site, so they are deliberately **not**
    /// gated by `PluginManager::capability_enabled` (unlike smart-model-router's
    /// capability, which also gates `commands::classify_message`).
    #[test]
    fn parses_the_writing_tools_manifest() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../plugins/writing-tools/plugin.json"
        );
        let raw = std::fs::read_to_string(path).expect("writing-tools plugin.json present");
        let m: PluginManifest = serde_json::from_str(&raw).expect("manifest parses");
        assert_eq!(m.id, "writing-tools");
        assert!(m.capabilities.iter().any(|c| c == "writing-tools"));
        assert!(
            m.python.is_none(),
            "writing-tools is python-less -- pure Rust + UI"
        );

        let p = DiscoveredPlugin {
            manifest: m,
            dir: PathBuf::from("/tmp/x"),
        };
        assert!(!has_chat(&p));
        assert!(!has_service(&p));
        assert!(!provides(&p, "writing-tools"));
    }

    #[test]
    fn provides_requires_capability_and_service() {
        // Advertises tts but has no service -> not consumable as a service cap.
        let no_service: PluginManifest =
            serde_json::from_str(r#"{"id":"x","name":"X","capabilities":["tts"]}"#).unwrap();
        let p = DiscoveredPlugin {
            manifest: no_service,
            dir: PathBuf::from("/tmp/x"),
        };
        assert!(!provides(&p, "tts"));

        // Has a service but doesn't advertise the capability.
        let other: PluginManifest = serde_json::from_str(
            r#"{"id":"y","name":"Y","capabilities":["chat"],"python":{"service":"s.py"}}"#,
        )
        .unwrap();
        let p = DiscoveredPlugin {
            manifest: other,
            dir: PathBuf::from("/tmp/y"),
        };
        assert!(!provides(&p, "tts"));
        assert!(provides(&p, "chat"));
    }

    #[test]
    fn service_health_path_defaults_to_root() {
        let m: PluginManifest =
            serde_json::from_str(r#"{"id":"x","name":"X","python":{}}"#).unwrap();
        assert_eq!(m.python.unwrap().service_health_path, "/");
    }

    /// End-to-end proxy for what a real `npm run dev` session sees: point a
    /// real `PluginManager` at the repo's actual `plugins/` directory (the
    /// exact dev-mode fallback `resolve_plugin_dirs` uses,
    /// crates/app/src/state.rs:307) with no `plugins.json` yet, and confirm
    /// `list_plugins()` would show all six built-ins (hf-browser,
    /// smart-model-router, remote-inference, and writing-tools
    /// disabled-but-ready by default; speech and web-search disabled and not
    /// set up). Exercising
    /// this in-process stands in for a GUI check of the Plugins screen, as
    /// it looks once experimental features are on (every add-on shipped today
    /// is experimental, so the list is empty otherwise).
    #[tokio::test]
    async fn discovers_the_real_repo_plugins_including_hf_browser() {
        let repo_plugins_dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins"));
        let tmp = tempfile::tempdir().unwrap();
        let manager = PluginManager::new(
            vec![repo_plugins_dir],
            tmp.path().join("plugin-data"),
            test_conn(),
        );
        manager.set_experimental_features(true);
        let plugins = manager.list().await;
        let mut ids: Vec<&str> = plugins.iter().map(|p| p.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            [
                "hf-browser",
                "remote-inference",
                "smart-model-router",
                "speech",
                "web-search",
                "writing-tools"
            ]
        );
        assert!(plugins.iter().all(|p| p.experimental));

        let web_search = plugins
            .iter()
            .find(|p| p.id == "web-search")
            .expect("web-search discovered");
        // Never enabled by default: nothing leaves the machine until the
        // user turns it on, which is also what installs its SearXNG.
        assert!(!web_search.enabled);
        assert!(
            matches!(web_search.setup, SetupState::NotSetup),
            "expected NotSetup, got {:?}",
            web_search.setup
        );

        let hf = plugins
            .iter()
            .find(|p| p.id == "hf-browser")
            .expect("hf-browser discovered");
        assert_eq!(hf.name, "Model Browser");
        assert!(hf.capabilities.iter().any(|c| c == "model-browser"));
        // Never enabled by default — the Browse entry stays hidden until the
        // user opts in from Add-ons (docs/design/HF_BROWSER.md design decision 1).
        assert!(!hf.enabled);
        // Python-less: "ready" immediately, no venv/setup ever runs, and the
        // service is permanently notApplicable — matches the UI gate in
        // ui/src/state/store.ts (modelBrowserAvailable checks capability +
        // enabled only, never service).
        assert!(
            matches!(hf.setup, SetupState::Ready),
            "expected Ready, got {:?}",
            hf.setup
        );
        assert!(
            matches!(hf.service, ServiceState::NotApplicable),
            "expected NotApplicable, got {:?}",
            hf.service
        );
        assert!(!hf.has_chat);

        let router = plugins
            .iter()
            .find(|p| p.id == "smart-model-router")
            .expect("smart-model-router discovered");
        assert_eq!(router.name, "Smart Model Router");
        assert!(router
            .capabilities
            .iter()
            .any(|c| c == "smart-model-router"));
        // Never enabled by default — multi-model auto-pick is opt-in.
        assert!(!router.enabled);
        assert!(
            matches!(router.setup, SetupState::Ready),
            "expected Ready, got {:?}",
            router.setup
        );
        assert!(
            matches!(router.service, ServiceState::NotApplicable),
            "expected NotApplicable, got {:?}",
            router.service
        );
        assert!(!router.has_chat);

        let writing_tools = plugins
            .iter()
            .find(|p| p.id == "writing-tools")
            .expect("writing-tools discovered");
        assert_eq!(writing_tools.name, "Writing Tools");
        assert!(writing_tools
            .capabilities
            .iter()
            .any(|c| c == "writing-tools"));
        // Never enabled by default — the Writing Tools sidebar entry stays
        // hidden until the user opts in from Add-ons.
        assert!(!writing_tools.enabled);
        // Python-less: "ready" immediately, no venv/setup ever runs, and the
        // service is permanently notApplicable — same as hf-browser/smart-model-router.
        assert!(
            matches!(writing_tools.setup, SetupState::Ready),
            "expected Ready, got {:?}",
            writing_tools.setup
        );
        assert!(
            matches!(writing_tools.service, ServiceState::NotApplicable),
            "expected NotApplicable, got {:?}",
            writing_tools.service
        );
        assert!(!writing_tools.has_chat);
    }

    /// `capability_enabled` is the backend-side analogue of
    /// `smartModelRouterAvailable`/`modelBrowserAvailable` in `store.ts` —
    /// capability + enabled only, no service/setup requirement, since a
    /// python-less plugin has neither.
    #[tokio::test]
    async fn capability_enabled_reflects_enable_state() {
        let repo_plugins_dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins"));
        let tmp = tempfile::tempdir().unwrap();
        let manager = PluginManager::new(
            vec![repo_plugins_dir],
            tmp.path().join("plugin-data"),
            test_conn(),
        );
        manager.set_experimental_features(true);

        assert!(!manager.capability_enabled("smart-model-router").await);

        manager
            .set_enabled("smart-model-router", true, |_| {})
            .await
            .expect("smart-model-router is python-less, enabling can't fail");
        assert!(manager.capability_enabled("smart-model-router").await);
        // Unrelated capability, still false.
        assert!(!manager.capability_enabled("model-browser").await);

        manager
            .set_enabled("smart-model-router", false, |_| {})
            .await
            .unwrap();
        assert!(!manager.capability_enabled("smart-model-router").await);
    }

    /// The "Experimental features" switch gates an experimental add-on:
    /// while it's off an enabled one is hidden from the Add-ons list, its
    /// capability reads as off, and it can't be enabled — but its persisted
    /// record is untouched, so turning the switch back on restores it as it
    /// was.
    #[tokio::test]
    async fn experimental_features_gate_experimental_plugins() {
        let repo_plugins_dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins"));
        let tmp = tempfile::tempdir().unwrap();
        let manager = PluginManager::new(
            vec![repo_plugins_dir],
            tmp.path().join("plugin-data"),
            test_conn(),
        );

        // Off by default: nothing listed, and enabling is refused.
        assert!(manager.list().await.is_empty());
        let mut failed = false;
        let refused = manager
            .set_enabled("writing-tools", true, |e| {
                failed |= matches!(e, PluginEvent::Failed { .. });
            })
            .await;
        assert!(refused.is_err());
        assert!(failed, "a refusal is also emitted as PluginEvent::Failed");

        manager.set_experimental_features(true);
        manager
            .set_enabled("writing-tools", true, |_| {})
            .await
            .expect("writing-tools is python-less, enabling can't fail");
        assert!(manager.capability_enabled("writing-tools").await);

        manager.set_experimental_features(false);
        manager.stop_experimental_services().await;
        assert!(manager.list().await.is_empty());
        assert!(!manager.capability_enabled("writing-tools").await);

        manager.set_experimental_features(true);
        assert!(manager.capability_enabled("writing-tools").await);
        let writing_tools = manager
            .list()
            .await
            .into_iter()
            .find(|p| p.id == "writing-tools")
            .expect("writing-tools listed again");
        assert!(writing_tools.enabled, "the per-plugin choice survives");
    }

    /// Only plugins whose manifest says `"experimental": true` depend on the
    /// switch: an add-on without the flag stays listed and enableable while
    /// it's off, and a core plugin with it is on exactly while it's on.
    #[tokio::test]
    async fn only_plugins_marked_experimental_are_gated() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        for (id, manifest) in [
            (
                "stable",
                r#"{"id":"stable","name":"Stable","capabilities":["stablecap"]}"#,
            ),
            (
                "trialcore",
                r#"{"id":"trialcore","name":"Trial Core","capabilities":["trialcap"],
                    "core":true,"experimental":true}"#,
            ),
        ] {
            std::fs::create_dir_all(plugins.join(id)).unwrap();
            std::fs::write(plugins.join(id).join("plugin.json"), manifest).unwrap();
        }
        let manager =
            PluginManager::new(vec![plugins], tmp.path().join("plugin-data"), test_conn());

        let listed = manager.list().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "stable");
        assert!(!listed[0].experimental);
        manager
            .set_enabled("stable", true, |_| {})
            .await
            .expect("a non-experimental add-on is enableable with the switch off");
        assert!(manager.capability_enabled("stablecap").await);
        assert!(!manager.capability_enabled("trialcap").await);

        manager.set_experimental_features(true);
        assert!(manager.capability_enabled("trialcap").await);
        assert!(manager.capability_enabled("stablecap").await);

        manager.set_experimental_features(false);
        manager.stop_experimental_services().await;
        assert!(!manager.capability_enabled("trialcap").await);
        assert!(manager.capability_enabled("stablecap").await);
    }

    /// An add-on's service is only ever provided once the user enabled it
    /// and its setup finished: a disabled one is refused, and so is an
    /// enabled one still setting up (the Plugins screen's `set_enabled` owns
    /// that setup, so this never starts a second one next to it).
    #[tokio::test]
    async fn ensure_capability_service_needs_an_enabled_set_up_addon() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("plugins/addon");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"id":"addon","name":"Addon","version":"0.1.0","capabilities":["cap"],
                "python":{"setup":"setup_plugin.py","service":"service.py"}}"#,
        )
        .unwrap();
        let manager = PluginManager::new(
            vec![tmp.path().join("plugins")],
            tmp.path().join("plugin-data"),
            test_conn(),
        );
        let off = manager.ensure_capability_service("cap", |_| {}).await;
        assert_eq!(off.unwrap_err(), "Turn this add-on on in Plugins first.");

        // Enabled, but its setup never finished (no setup version, no venv).
        let db = test_conn();
        db.lock()
            .unwrap()
            .execute(
                "INSERT INTO plugin_records (key, json) VALUES ('current', ?1)",
                params![r#"{"addon":{"enabled":true}}"#],
            )
            .unwrap();
        let manager = PluginManager::new(
            vec![tmp.path().join("plugins")],
            tmp.path().join("plugin-data"),
            db,
        );
        let mut events = 0;
        let setting_up = manager
            .ensure_capability_service("cap", |_| events += 1)
            .await;
        assert_eq!(
            setting_up.unwrap_err(),
            "This add-on is still setting up. Try again in a moment."
        );
        assert_eq!(events, 0, "no setup was started");
        assert!(matches!(
            manager.list().await[0].setup,
            SetupState::NotSetup
        ));
    }

    /// A data import from a device where the add-on was enabled and set up
    /// turns it on here, but — never set up on this device — it still
    /// offers setup rather than claiming to be ready, and a running manager
    /// sees the import without a restart.
    #[tokio::test]
    async fn an_imported_set_up_record_still_offers_setup_on_a_new_device() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("plugins/addon");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"id":"addon","name":"Addon","version":"0.1.0","capabilities":["cap"],
                "python":{"setup":"setup_plugin.py","service":"service.py"}}"#,
        )
        .unwrap();
        let db = test_conn();
        let manager = PluginManager::new(
            vec![tmp.path().join("plugins")],
            tmp.path().join("plugin-data"),
            db.clone(),
        );
        assert!(!manager.list().await[0].enabled);

        let count = import_records_json(
            &db.lock().unwrap(),
            r#"{"addon":{"enabled":true,"setupVersion":"0.1.0"}}"#,
        )
        .unwrap();
        assert_eq!(count, 1);
        manager.reload_records().await;

        let addon = &manager.list().await[0];
        assert!(addon.enabled);
        assert!(matches!(addon.setup, SetupState::NotSetup));
        assert!(!manager.is_ready(&manager.plugins[0]).await);
        assert_eq!(
            manager
                .ensure_capability_service("cap", |_| {})
                .await
                .unwrap_err(),
            "This add-on is still setting up. Try again in a moment.",
            "never tries to start a service from a venv that isn't here"
        );
    }

    /// A user who had the plugin enabled under its old `model-router` id
    /// keeps it enabled as `smart-model-router`, and the carried-over record
    /// is persisted so the old key is gone from the database.
    #[tokio::test]
    async fn renamed_plugin_keeps_its_enabled_record() {
        let repo_plugins_dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins"));
        let tmp = tempfile::tempdir().unwrap();
        let db = test_conn();
        db.lock()
            .unwrap()
            .execute(
                "INSERT INTO plugin_records (key, json) VALUES ('current', ?1)",
                params![r#"{"model-router":{"enabled":true}}"#],
            )
            .unwrap();

        let manager = PluginManager::new(
            vec![repo_plugins_dir],
            tmp.path().join("plugin-data"),
            db.clone(),
        );
        manager.set_experimental_features(true);

        assert!(manager.capability_enabled("smart-model-router").await);
        let saved = load_records(&db.lock().unwrap());
        assert!(!saved.contains_key("model-router"));
        assert!(saved["smart-model-router"].enabled);
    }

    /// A record already under the new id wins over a leftover old one.
    #[test]
    fn migrate_renamed_ids_keeps_an_existing_new_record() {
        let mut records = HashMap::new();
        records.insert(
            "model-router".to_string(),
            PluginRecord {
                enabled: true,
                setup_version: None,
            },
        );
        records.insert(
            "smart-model-router".to_string(),
            PluginRecord {
                enabled: false,
                setup_version: None,
            },
        );
        assert!(migrate_renamed_ids(&mut records));
        assert!(!records.contains_key("model-router"));
        assert!(!records["smart-model-router"].enabled);
        assert!(
            !migrate_renamed_ids(&mut records),
            "nothing left to carry over"
        );
    }

    /// A stale copy of a renamed plugin (e.g. dropped into `<data>/plugins/`
    /// by hand) is never discovered next to its successor.
    #[tokio::test]
    async fn stale_renamed_plugin_is_not_discovered() {
        let tmp = tempfile::tempdir().unwrap();
        let stale = tmp.path().join("plugins/model-router");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(
            stale.join("plugin.json"),
            r#"{"id":"model-router","name":"Model Router","capabilities":["model-router"]}"#,
        )
        .unwrap();

        let manager = PluginManager::new(
            vec![tmp.path().join("plugins")],
            tmp.path().join("plugin-data"),
            test_conn(),
        );
        manager.set_experimental_features(true);
        assert!(manager.list().await.is_empty());
    }

    /// A `"core": true` plugin is never shown as an
    /// Add-on and is treated as enabled unconditionally — even with a
    /// persisted record explicitly saying `enabled: false` (simulating a
    /// stale/irrelevant record; a core plugin has no Add-ons toggle to write
    /// one) — because there's no user-facing enable step for it to depend
    /// on. `is_set_up`/venv presence is faked here (a marker file, not a
    /// real venv) so this stays a fast unit test; the real
    /// setup-then-start flow (`ensure_capability_service`) needs a live Python
    /// subprocess and is exercised by `plugins/web-search/README.md`'s
    /// manual verification recipe instead.
    #[tokio::test]
    async fn core_plugin_is_hidden_and_always_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin_dir = tmp.path().join("plugins/testcore");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"id":"testcore","name":"Test Core","version":"0.1.0",
                "capabilities":["testcap"],"core":true,
                "python":{"service":"service.py"}}"#,
        )
        .unwrap();

        let data_root = tmp.path().join("plugin-data");
        let venv_python = data_root.join("testcore/venv/bin/python");
        std::fs::create_dir_all(venv_python.parent().unwrap()).unwrap();
        std::fs::write(&venv_python, "").unwrap();
        let db = test_conn();
        // Explicitly disabled in the persisted record — a core plugin must
        // ignore this, since nothing in the UI ever writes one for it.
        db.lock()
            .unwrap()
            .execute(
                "INSERT INTO plugin_records (key, json) VALUES ('current', ?1)",
                params![r#"{"testcore":{"enabled":false,"setupVersion":"0.1.0"}}"#],
            )
            .unwrap();

        let manager = PluginManager::new(vec![tmp.path().join("plugins")], data_root, db);

        assert!(
            manager.list().await.is_empty(),
            "a core plugin must not appear in the Add-ons list"
        );
        // Experimental features are off (the default) — this plugin isn't
        // marked experimental, so it doesn't depend on that switch.
        assert!(manager.capability_enabled("testcap").await);
        assert_eq!(
            manager.active_service_plugin("testcap").await.as_deref(),
            Some("testcore"),
            "a core plugin must be active regardless of its stored enabled flag"
        );
    }

    #[test]
    fn discover_reads_manifests_and_first_dir_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("builtin/web-search");
        let b = tmp.path().join("user/web-search");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(
            a.join("plugin.json"),
            r#"{"id":"web-search","name":"Built-in"}"#,
        )
        .unwrap();
        std::fs::write(
            b.join("plugin.json"),
            r#"{"id":"web-search","name":"User"}"#,
        )
        .unwrap();
        // A directory without a manifest is ignored.
        std::fs::create_dir_all(tmp.path().join("builtin/not-a-plugin")).unwrap();

        let found = discover(&[tmp.path().join("builtin"), tmp.path().join("user")]);
        assert_eq!(found.len(), 1, "the same id is not listed twice");
        assert_eq!(found[0].manifest.name, "Built-in", "earlier dir wins on id");
    }

    #[test]
    fn python_less_plugins_need_no_setup() {
        let records = HashMap::new();
        let p = DiscoveredPlugin {
            manifest: serde_json::from_str(r#"{"id":"x","name":"X"}"#).unwrap(),
            dir: PathBuf::from("/tmp/x"),
        };
        assert!(is_set_up(&records, &PathBuf::from("/tmp/data"), &p));
    }
}
