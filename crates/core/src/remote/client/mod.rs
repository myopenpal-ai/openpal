//! Mobile client for `crate::remote`'s gateway: everything a phone needs to
//! discover a desktop on the LAN, pair with it, and use it as
//! [`crate::engine::EngineManager`]'s backend
//! ([`crate::engine::EngineBackend`]) instead of a local subprocess — the
//! shape `docs/REMOTE_INFERENCE.md`'s "Building the mobile client" section
//! already sketched when Remote Access first shipped.
//!
//! Plain, cross-platform Rust — no `#[cfg(target_os = "android")]` anywhere
//! in this submodule. Only the construction site
//! (`crates/app/src/state.rs`) and the Android `MulticastLock` plugin
//! bridge (`crates/app/src/remote_client_android.rs`) are actually
//! platform-specific; everything here is unit/integration-testable on the
//! normal desktop CI runner, same as `crate::remote::gateway`'s own tests.

mod backend;
mod discovery;
mod pairing;
mod store;
mod tls;

pub use backend::RemoteBackend;
pub use discovery::{Browser, DiscoveredHost, SERVICE_TYPE};
pub use pairing::{PairError, PairOutcome};
pub use store::RemotePairingRecord;

use serde::{Deserialize, Serialize};

/// Flat status view for the remote-inference plugin's Settings section
/// (`remote_inference_status` command): whether the app's single engine
/// surface is currently routed to the paired device, who it's paired with
/// (if anyone), and what the *active* engine reports right now — the remote
/// engine's status while connected, the local engine's otherwise. Lets the
/// section show every state the ticket names (no pairing, connected and
/// running, remote revoked) from one round trip instead of threading three
/// commands together.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteInferenceStatus {
    /// Whether `EngineManager` currently routes chat/status/triage to the
    /// paired device's engine (`EngineManager::remote_active`).
    pub active: bool,
    /// The current pairing, if any — mirrors [`RemoteBackend::paired_record`].
    pub pairing: Option<RemotePairingRecord>,
    /// The active engine's status: [`crate::types::EngineStatus`] from the
    /// remote gateway while `active`, the local engine's otherwise.
    pub engine: crate::types::EngineStatus,
}
