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
