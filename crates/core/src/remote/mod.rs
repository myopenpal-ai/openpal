//! Remote Access: let another device on the LAN (or an already-connected
//! overlay network — Tailscale/WireGuard/router port-forward) pair with
//! this desktop and use it as the inference source for chat completions.
//! Off by default, gated behind Settings → Devices — the one explicit,
//! narrow exception to "the engine only ever listens on loopback"
//! (`crate::engine`/`ARCHITECTURE.md`).
//!
//! Shape mirrors `crate::plugin`'s existing "managed service" pattern (free
//! port → spawn/listen → health → stop on exit), plus a pairing flow
//! (`pairing.rs`) modeled on that same "explicit user action, short-lived
//! session, never an always-on listener" idea. `gateway.rs` is the inbound
//! HTTP(S) server; `wol.rs`/`wol_status.rs` are the wake-on-LAN send +
//! best-effort "is this even enabled on the host" detection;
//! `discovery.rs` advertises this desktop over mDNS; `store.rs` is the
//! `app_db`-backed persistence (paired devices, gateway config/cert).
//!
//! v1's pairing/auth is a deliberately simple, self-contained mechanism —
//! an opaque per-device bearer token, no asymmetric device identity — built
//! for this feature's own threat model (an already-TLS-pinned LAN
//! connection, or an already-encrypted overlay network), not the separate
//! multi-device conversation-sync feature's own Ed25519/X25519
//! device-identity design (`crate::crypto::device_identity`, now
//! implemented — see that module's doc comment), which targets a
//! materially different threat model (a fully untrusted, blind relay) and
//! is intentionally unused here. See `docs/REMOTE_INFERENCE.md`'s
//! migration note for revisiting this now that the shared module exists.
//!
//! **What this deliberately does not do**: auto-start the engine when a
//! request arrives for a stopped one. Wake-on-LAN only powers the
//! *machine* back on — getting OpenPal itself running and a model loaded
//! from there is already covered by the OS's own "open at login" and
//! OpenPal's existing `Settings.auto_load_on_startup` (frontend-driven).
//! Reimplementing `commands::start_engine`'s model-selection/GPU-plan/
//! disk-slow logic here would duplicate carefully-tuned, app-layer-only
//! logic inside a module that's deliberately Tauri-free; a request that
//! arrives while the engine is stopped gets a clear, actionable error
//! instead. Also deliberately out of scope for v1: per-device rate
//! limiting on the gateway (see `docs/REMOTE_INFERENCE.md`'s security
//! notes — a named, disclosed gap, not a silent one).

pub mod client;
mod discovery;
mod gateway;
mod pairing;
mod pin;
mod store;
mod wol;
mod wol_status;

pub use pairing::{PairingEvent, PairingPayload};
pub use pin::{verify_pin, PinError};
pub use store::PairedDeviceInfo;
// `build_magic_packet`/`send_magic_packet` aren't called anywhere in this
// crate today — they're here for a future mobile client (a different
// workspace crate depending on `openpal-core`, per `docs/design/MOBILE.md`'s
// layout) to send the actual wake-up. `list_interfaces` already has an
// in-crate caller (`RemoteAccessManager::start_pairing`'s payload
// building), but is re-exported alongside them since it's the other half
// of the same "what a client needs to send a magic packet" contract.
pub use wol::{
    build_magic_packet, list_interfaces, send_magic_packet, MacAddress, NetworkInterfaceInfo,
};
pub use wol_status::WolStatus;

use crate::engine::EngineManager;
use base64::Engine as _;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

/// State shared between `RemoteAccessManager` (called from Tauri commands)
/// and the gateway's HTTP handlers (`gateway.rs`), which run on a spawned
/// task for as long as Remote Access is enabled.
pub(crate) struct Shared {
    app_db: Arc<Mutex<Connection>>,
    engine: Arc<EngineManager>,
    http: reqwest::Client,
    /// The one currently-open pairing session, if any — set by
    /// `RemoteAccessManager::start_pairing`, consumed by the gateway's
    /// `/remote/v1/pair` handler. `None` whenever no "Add a device" flow is
    /// in progress, which is almost always.
    pending_pairing: AsyncMutex<Option<PendingPairing>>,
}

pub(crate) struct PendingPairing {
    pub code: String,
    pub expires_at_ms: i64,
    /// Wrong-code guesses so far this session. `gateway::pair_handler`
    /// drops the session entirely once this reaches
    /// [`pairing::MAX_PAIRING_ATTEMPTS`], rather than reinserting it for
    /// another try.
    pub attempts: u32,
    /// Taken (leaving `None`) the moment a matching code is presented, so a
    /// session can only ever be completed once. Carries the device name
    /// back to the waiting `start_pairing` call so it can emit
    /// `PairingEvent::Paired`.
    pub tx: Option<oneshot::Sender<String>>,
}

/// Owns the gateway's listening task (if Remote Access is enabled) and
/// every paired-device record. One per app instance, exactly like
/// `EngineManager`/`PluginManager` — see `crates/app/src/state.rs`'s
/// `AppState`.
pub struct RemoteAccessManager {
    shared: Arc<Shared>,
    running: AsyncMutex<Option<gateway::RunningGateway>>,
}

/// Flat status view for the UI (`get_remote_access_status`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAccessStatusInfo {
    /// The persisted user setting — independent of `running` so the UI can
    /// show "starting…"/"failed to start" rather than just an on/off toggle
    /// silently disagreeing with reality.
    pub enabled: bool,
    pub running: bool,
    pub port: Option<u16>,
    pub interfaces: Vec<NetworkInterfaceInfo>,
    pub paired_device_count: usize,
}

impl RemoteAccessManager {
    pub fn new(app_db: Arc<Mutex<Connection>>, engine: Arc<EngineManager>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("OpenPal-RemoteGateway/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client");
        Self {
            shared: Arc::new(Shared {
                app_db,
                engine,
                http,
                pending_pairing: AsyncMutex::new(None),
            }),
            running: AsyncMutex::new(None),
        }
    }

    /// Start the gateway if the persisted config says it should already be
    /// running — call once at app launch, mirroring how already-enabled
    /// plugin services resume on launch (`plugins/PLUGINS.md` §4). Errors
    /// are logged, not surfaced (there's no UI open yet to show them to).
    pub async fn resume_if_enabled(&self) {
        let enabled = {
            let conn = self.shared.app_db.lock().unwrap();
            store::load_config(&conn).enabled
        };
        if enabled {
            if let Err(e) = self.start_gateway().await {
                tracing::warn!("remote access: failed to resume on launch: {e}");
            }
        }
    }

    pub async fn status(&self) -> RemoteAccessStatusInfo {
        let (enabled, port) = {
            let conn = self.shared.app_db.lock().unwrap();
            let cfg = store::load_config(&conn);
            (cfg.enabled, cfg.port)
        };
        let running = self.running.lock().await.is_some();
        let paired_device_count = self
            .list_paired_devices()
            .await
            .map(|v| v.len())
            .unwrap_or(0);
        RemoteAccessStatusInfo {
            enabled,
            running,
            port,
            interfaces: wol::list_interfaces(),
            paired_device_count,
        }
    }

    pub async fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        if enabled {
            self.start_gateway().await?;
        } else {
            self.stop_gateway().await;
        }
        let conn = self.shared.app_db.lock().unwrap();
        let mut cfg = store::load_config(&conn);
        cfg.enabled = enabled;
        store::save_config(&conn, &cfg).map_err(|e| {
            tracing::error!("remote access: failed to persist config: {e}");
            "Couldn't save that setting.".to_string()
        })
    }

    /// Called from `crates/app/src/lib.rs`'s `RunEvent::Exit`, alongside
    /// `state.engine.stop()`/`state.plugins.stop_all()` — a spawned gateway
    /// task and mDNS responder must never outlive the app.
    pub async fn stop_all(&self) {
        self.stop_gateway().await;
    }

    async fn start_gateway(&self) -> Result<(), String> {
        let mut running = self.running.lock().await;
        if running.is_some() {
            return Ok(());
        }
        let (port, cert_der, key_der, device_id) = self.ensure_config()?;
        let fingerprint = fingerprint_hex(&cert_der);
        let gw = gateway::start(
            self.shared.clone(),
            port,
            cert_der,
            key_der,
            &device_id,
            &fingerprint,
        )
        .await?;
        *running = Some(gw);
        Ok(())
    }

    async fn stop_gateway(&self) {
        let gw = self.running.lock().await.take();
        if let Some(gw) = gw {
            gw.stop().await;
        }
    }

    /// Load persisted gateway config, filling in (and persisting) anything
    /// missing on first use: the port, the self-signed cert/key, and this
    /// install's stable device id.
    fn ensure_config(&self) -> Result<(u16, Vec<u8>, Vec<u8>, String), String> {
        let conn = self.shared.app_db.lock().unwrap();
        let mut cfg = store::load_config(&conn);
        let mut dirty = false;

        let port = match cfg.port {
            Some(p) => p,
            None => {
                let p = gateway::free_port().map_err(|e| {
                    tracing::error!("remote access: couldn't pick a port: {e}");
                    "Couldn't start the remote access server.".to_string()
                })?;
                cfg.port = Some(p);
                dirty = true;
                p
            }
        };
        let device_id = match cfg.device_id.clone() {
            Some(id) => id,
            None => {
                let id = store::new_device_id();
                cfg.device_id = Some(id.clone());
                dirty = true;
                id
            }
        };
        let (cert_der, key_der) = match (&cfg.cert_der_b64, &cfg.key_der_b64) {
            (Some(cert_b64), Some(key_b64)) => {
                let cert = base64::engine::general_purpose::STANDARD
                    .decode(cert_b64)
                    .map_err(|_| "Couldn't read the saved security certificate.".to_string())?;
                let key = base64::engine::general_purpose::STANDARD
                    .decode(key_b64)
                    .map_err(|_| "Couldn't read the saved security certificate.".to_string())?;
                (cert, key)
            }
            _ => {
                let (cert, key) = generate_self_signed_cert()?;
                cfg.cert_der_b64 = Some(base64::engine::general_purpose::STANDARD.encode(&cert));
                cfg.key_der_b64 = Some(base64::engine::general_purpose::STANDARD.encode(&key));
                dirty = true;
                (cert, key)
            }
        };

        if dirty {
            store::save_config(&conn, &cfg).map_err(|e| {
                tracing::error!("remote access: failed to persist gateway config: {e}");
                "Couldn't save the remote access configuration.".to_string()
            })?;
        }
        Ok((port, cert_der, key_der, device_id))
    }

    /// Open a pairing session: emits `PairingEvent::Waiting` with the QR
    /// payload immediately, then waits (bounded by
    /// [`pairing::PAIRING_SESSION_TTL`]) for the gateway's `/remote/v1/pair`
    /// handler to complete it, emitting `Paired`/`Expired` accordingly.
    /// Rejects if Remote Access isn't enabled, or another session is
    /// already open — "Add a device" is always one explicit action at a
    /// time, never an always-on listener.
    pub async fn start_pairing(
        &self,
        mut on_event: impl FnMut(PairingEvent) + Send,
    ) -> Result<(), String> {
        let port = {
            let running = self.running.lock().await;
            match running.as_ref() {
                Some(gw) => gw.port,
                None => return Err("Turn on Remote Access before pairing a device.".to_string()),
            }
        };
        {
            let pending = self.shared.pending_pairing.lock().await;
            if pending.is_some() {
                return Err("A pairing session is already open.".to_string());
            }
        }

        let fingerprint = {
            let conn = self.shared.app_db.lock().unwrap();
            let cfg = store::load_config(&conn);
            let cert_b64 = cfg
                .cert_der_b64
                .ok_or_else(|| "Remote Access isn't fully set up yet.".to_string())?;
            let cert = base64::engine::general_purpose::STANDARD
                .decode(cert_b64)
                .map_err(|_| "Remote Access isn't fully set up yet.".to_string())?;
            fingerprint_hex(&cert)
        };

        let code = pairing::generate_pairing_code();
        let expires_at_ms = now_ms() + pairing::PAIRING_SESSION_TTL.as_millis() as i64;
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.shared.pending_pairing.lock().await;
            *pending = Some(PendingPairing {
                code: code.clone(),
                expires_at_ms,
                attempts: 0,
                tx: Some(tx),
            });
        }

        let interfaces = wol::list_interfaces();
        let addrs: Vec<String> = interfaces
            .iter()
            .flat_map(|i| i.ipv4_addrs.iter().map(|a| a.to_string()))
            .collect();
        let mac = interfaces.first().map(|i| i.mac.to_string());
        let payload = PairingPayload {
            v: 1,
            code,
            addrs,
            port,
            fingerprint,
            mac,
            expires_at_ms,
        };
        on_event(PairingEvent::Waiting { payload });

        match tokio::time::timeout(pairing::PAIRING_SESSION_TTL, rx).await {
            Ok(Ok(device_name)) => on_event(PairingEvent::Paired { device_name }),
            Ok(Err(_)) => {
                // `tx` was dropped without sending: the gateway invalidated
                // the session (too many wrong attempts), not a TTL elapse.
                let mut pending = self.shared.pending_pairing.lock().await;
                *pending = None;
                on_event(PairingEvent::Failed {
                    message: "Too many incorrect attempts. Press \"Add a device\" again."
                        .to_string(),
                });
            }
            Err(_) => {
                let mut pending = self.shared.pending_pairing.lock().await;
                *pending = None;
                on_event(PairingEvent::Expired);
            }
        }
        Ok(())
    }

    pub async fn list_paired_devices(&self) -> Result<Vec<PairedDeviceInfo>, String> {
        let conn = self.shared.app_db.lock().unwrap();
        store::list_paired_devices(&conn).map_err(|e| {
            tracing::error!("remote access: list paired devices failed: {e}");
            "Couldn't load paired devices.".to_string()
        })
    }

    pub async fn revoke_paired_device(&self, id: &str) -> Result<(), String> {
        let conn = self.shared.app_db.lock().unwrap();
        store::revoke_paired_device(&conn, id).map_err(|e| {
            tracing::error!("remote access: revoke failed: {e}");
            "Couldn't remove that device.".to_string()
        })
    }

    pub async fn rename_paired_device(&self, id: &str, name: &str) -> Result<(), String> {
        let conn = self.shared.app_db.lock().unwrap();
        store::rename_paired_device(&conn, id, name).map_err(|e| {
            tracing::error!("remote access: rename failed: {e}");
            "Couldn't rename that device.".to_string()
        })
    }

    /// Best-effort "is Wake-on-LAN enabled on this machine" check —
    /// shells out to an OS tool (`wol_status::detect`'s own doc comment),
    /// so run off the async runtime's worker threads.
    pub async fn wol_status(&self) -> WolStatus {
        tokio::task::spawn_blocking(wol_status::detect)
            .await
            .unwrap_or_else(|e| WolStatus::Unknown {
                reason: format!("Couldn't check this on your system ({e})."),
            })
    }
}

fn generate_self_signed_cert() -> Result<(Vec<u8>, Vec<u8>), String> {
    let subject_alt_names = vec!["localhost".to_string()];
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(subject_alt_names).map_err(|e| {
            tracing::error!("remote access: cert generation failed: {e}");
            "Couldn't set up secure access.".to_string()
        })?;
    let cert_der = cert.der().to_vec();
    let key_der = signing_key.serialize_der();
    Ok((cert_der, key_der))
}

/// `pub(crate)`: reused by both `client::tls` (to compute the fingerprint a
/// mobile pairing connection actually presented, so the pinning check on
/// both ends of a pairing is byte-for-byte the same computation) and
/// `pin::verify_pin` (so the pin check hashes a presented certificate
/// identically to how the gateway computed the fingerprint being pinned
/// against in the first place).
pub(crate) fn fingerprint_hex(cert_der: &[u8]) -> String {
    let hash = Sha256::digest(cert_der);
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manager() -> RemoteAccessManager {
        let dir = tempfile::tempdir().unwrap().keep();
        let app_db = Arc::new(Mutex::new(
            crate::app_db::open(&dir.join("app.sqlite"), None)
                .unwrap()
                .0,
        ));
        let engine = Arc::new(EngineManager::new(dir.join("logs")));
        RemoteAccessManager::new(app_db, engine)
    }

    /// End-to-end through a real TLS-bound socket: enabling Remote Access
    /// generates a cert, binds `axum-server`, and actually serves — not
    /// just the handler-level tests in `gateway.rs`'s own test module.
    #[tokio::test]
    async fn enabling_serves_real_tls_and_disabling_tears_it_down() {
        let manager = test_manager();
        assert!(!manager.status().await.running);

        manager.set_enabled(true).await.unwrap();
        let status = manager.status().await;
        assert!(status.enabled);
        assert!(status.running);
        let port = status.port.unwrap();

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true) // self-signed, by design
            .build()
            .unwrap();
        let response = client
            .get(format!("https://127.0.0.1:{port}/remote/v1/status"))
            .send()
            .await
            .unwrap();
        // No bearer token presented — the real TLS-terminated server must
        // still enforce auth, not just the in-process handler tests.
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);

        manager.set_enabled(false).await.unwrap();
        let status = manager.status().await;
        assert!(!status.enabled);
        assert!(!status.running);
    }

    #[tokio::test]
    async fn pairing_end_to_end_over_real_tls_issues_a_working_token() {
        let manager = test_manager();
        manager.set_enabled(true).await.unwrap();
        let port = manager.status().await.port.unwrap();
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let pairing_task = tokio::spawn(async move {
            manager
                .start_pairing(move |event| events_clone.lock().unwrap().push(event))
                .await
                .unwrap();
            manager
        });

        // Give `start_pairing` a moment to emit `Waiting` and open the
        // session before this test tries to complete it.
        let code = loop {
            let found = {
                let events = events.lock().unwrap();
                match events.first() {
                    Some(PairingEvent::Waiting { payload }) => Some(payload.code.clone()),
                    _ => None,
                }
            };
            if let Some(code) = found {
                break code;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };

        let response = client
            .post(format!("https://127.0.0.1:{port}/remote/v1/pair"))
            .json(&serde_json::json!({ "code": code, "deviceName": "Test Phone" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = response.json().await.unwrap();
        let token = body["token"].as_str().unwrap().to_string();

        let manager = pairing_task.await.unwrap();
        assert!(
            matches!(events.lock().unwrap().get(1), Some(PairingEvent::Paired { device_name }) if device_name == "Test Phone")
        );

        // The freshly issued token must actually authenticate against the
        // real server, end to end.
        let response = client
            .get(format!("https://127.0.0.1:{port}/remote/v1/status"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);

        let devices = manager.list_paired_devices().await.unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "Test Phone");

        manager.stop_all().await;
    }
}
