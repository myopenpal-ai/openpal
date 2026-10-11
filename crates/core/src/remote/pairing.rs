//! Pairing wire types: what the desktop shows (QR + manual fallback code)
//! and the progress events Settings streams while a pairing session is
//! open. The session itself (open/expire/complete) is orchestrated by
//! `RemoteAccessManager` in `mod.rs`, since it needs to coordinate with the
//! gateway's `/remote/v1/pair` HTTP handler (`gateway.rs`) — this module is
//! just the shapes both sides agree on.
//!
//! v1's pairing is deliberately simple (a short one-time code, single-use,
//! ~5 minute expiry, never an always-on listener) rather than the
//! asymmetric device-identity model the separate multi-device
//! conversation-sync feature is designing for its own (fully untrusted
//! relay) threat model — see this module's parent doc comment.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// How long an opened pairing session stays valid before it must be
/// re-opened. Mirrors the sync feature's own "5 minutes, single-use" choice
/// for the same UX reason: a stale, forgotten-about session sitting open
/// indefinitely would be exactly the "always-on listener" this design
/// avoids.
pub const PAIRING_SESSION_TTL: Duration = Duration::from_secs(300);

/// Wrong-guess attempts allowed on one pairing session before the gateway
/// invalidates it outright, forcing "Add a device" to be pressed again,
/// rather than leaving the session open (bounded only by
/// [`PAIRING_SESSION_TTL`]) for an attacker on the LAN/overlay network to
/// keep guessing. A hard cap is strictly stronger than time-based backoff
/// here: bounds brute-force success probability to
/// `MAX_PAIRING_ATTEMPTS / 1_000_000` for the entire session lifetime,
/// regardless of attacker request rate.
pub const MAX_PAIRING_ATTEMPTS: u32 = 5;

/// Everything a future mobile client needs to complete pairing, shown as a
/// QR code (with a manually-typeable `code` fallback for a camera-less
/// flow, or desktop-to-desktop pairing).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingPayload {
    /// Wire version, so a future incompatible change can be detected
    /// cleanly instead of a client silently misparsing a newer payload.
    pub v: u32,
    pub code: String,
    /// Candidate LAN IPv4 addresses this desktop is reachable on right now
    /// (see `crate::remote::wol::list_interfaces`). A client on an overlay
    /// network (Tailscale/WireGuard) instead uses whatever address that
    /// network assigns this desktop — not derivable here, so out of scope
    /// for this payload; see `docs/REMOTE_INFERENCE.md`.
    pub addrs: Vec<String>,
    pub port: u16,
    /// SHA-256 of the gateway's self-signed certificate (DER), hex-encoded
    /// — pinned by the client (trust-on-first-use) instead of trusting any
    /// CA, since this cert is self-signed by design.
    pub fingerprint: String,
    /// The primary interface's MAC address, for a future client's
    /// wake-on-LAN use once this desktop is asleep and unreachable by IP.
    pub mac: Option<String>,
    pub expires_at_ms: i64,
}

/// Streamed by `start_pairing_session` while a pairing session is open.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PairingEvent {
    /// The session is open and `payload` is ready to render as a QR code.
    Waiting { payload: PairingPayload },
    /// A device completed pairing.
    Paired { device_name: String },
    /// Nothing paired within [`PAIRING_SESSION_TTL`]; the user must press
    /// "Add a device" again to open a new session.
    Expired,
    /// `message` is safe to show to a non-technical user.
    Failed { message: String },
}

/// A short numeric code, easy to type by hand as a fallback to scanning the
/// QR code — six digits, matching the shape most authenticator-style flows
/// already train users to expect.
pub fn generate_pairing_code() -> String {
    let n: u32 = rand::random::<u32>() % 1_000_000;
    format!("{n:06}")
}
