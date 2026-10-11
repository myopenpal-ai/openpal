//! Browse for `_openpal._tcp.local.` gateways advertised by
//! `crate::remote::discovery::Advertisement` on the desktop side. Same
//! `mdns-sd` dependency and daemon type as that module's own advertise
//! side; the blocking-channel-to-background-thread bridge here mirrors
//! `Advertisement::stop`'s own `spawn_blocking` bridge for the same
//! reason — `mdns-sd`'s daemon API is synchronous.
//!
//! **Won't discover anything from the Android emulator** (`docs/design/MOBILE.md`)
//! — QEMU's virtual networking doesn't pass multicast from the guest to
//! the host's real LAN interface. A real device on a real LAN is
//! unaffected; either way, the manual "enter an address" fallback
//! (`docs/REMOTE_INFERENCE.md`) is real UI, not just a test workaround —
//! it's also how an overlay-network (Tailscale/WireGuard) client, whose
//! network never carries multicast either, reaches a paired desktop.

use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent};
use serde::Serialize;
use std::net::IpAddr;

pub const SERVICE_TYPE: &str = "_openpal._tcp.local.";

/// One gateway seen on the LAN, parsed from its mDNS TXT records
/// (`crate::remote::discovery::Advertisement::start`'s own `id`/`name`/`fp`
/// properties). Streamed to the frontend via a `tauri::ipc::Channel`
/// (`commands::remote_discover_hosts`), hence `Serialize` + camelCase —
/// same wire-shape convention as every other event type in this crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredHost {
    pub device_id: String,
    pub device_name: String,
    /// Fingerprint prefix only (12 hex chars) — a pre-filter for the UI's
    /// device list, never a trust decision. The real fingerprint is only
    /// ever trusted once pinned from an actual handshake
    /// (`super::tls::CapturingVerifier`, during pairing).
    pub fingerprint_prefix: String,
    pub addrs: Vec<IpAddr>,
    pub port: u16,
}

pub struct Browser {
    daemon: ServiceDaemon,
}

impl Browser {
    /// Start browsing; `on_event` is called (from a background thread —
    /// `mdns-sd`'s receiver is a blocking `flume` channel) once per
    /// resolved host. Callers typically collect these into a live list for
    /// the pairing UI.
    pub fn start(
        mut on_event: impl FnMut(DiscoveredHost) + Send + 'static,
    ) -> Result<Self, String> {
        let daemon = ServiceDaemon::new().map_err(|e| {
            tracing::warn!("remote client: mdns daemon failed to start: {e}");
            "Couldn't start looking for devices.".to_string()
        })?;
        let receiver = daemon.browse(SERVICE_TYPE).map_err(|e| {
            tracing::warn!("remote client: mdns browse failed: {e}");
            "Couldn't start looking for devices.".to_string()
        })?;
        std::thread::spawn(move || {
            while let Ok(event) = receiver.recv() {
                if let ServiceEvent::ServiceResolved(resolved) = event {
                    if let Some(host) = parse_resolved(&resolved) {
                        on_event(host);
                    }
                }
            }
        });
        Ok(Self { daemon })
    }

    /// Stop browsing and shut down the daemon. Runs the (blocking) daemon
    /// shutdown on a blocking-safe thread, mirroring
    /// `crate::remote::discovery::Advertisement::stop`.
    pub async fn stop(self) {
        let Self { daemon } = self;
        let _ = tokio::task::spawn_blocking(move || {
            let _ = daemon.shutdown();
        })
        .await;
    }
}

fn parse_resolved(info: &ResolvedService) -> Option<DiscoveredHost> {
    let props = info.get_properties();
    let device_id = props.get("id")?.val_str().to_string();
    let device_name = props.get("name")?.val_str().to_string();
    let fingerprint_prefix = props.get("fp")?.val_str().to_string();
    let addrs: Vec<IpAddr> = info
        .get_addresses()
        .iter()
        .map(|scoped| scoped.to_ip_addr())
        .collect();
    Some(DiscoveredHost {
        device_id,
        device_name,
        fingerprint_prefix,
        addrs,
        port: info.get_port(),
    })
}
