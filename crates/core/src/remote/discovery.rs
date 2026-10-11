//! LAN discovery: advertise this desktop's gateway over mDNS as
//! `_openpal._tcp.local.` so a paired client on the same LAN can find it
//! again after a restart/IP change without the user re-entering an
//! address. `mdns-sd` is pure Rust with no system Bonjour/Avahi dependency
//! — only the "advertise" side is implemented here; a future mobile
//! client implements the "browse" side (see `docs/REMOTE_INFERENCE.md`).
//! Advertising only ever runs while Remote Access is enabled.

use mdns_sd::{ServiceDaemon, ServiceInfo};

pub const SERVICE_TYPE: &str = "_openpal._tcp.local.";

pub struct Advertisement {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertisement {
    /// Start advertising. `device_id` is used as both the mDNS instance
    /// name and (via `<id>.local.`) the advertised hostname — it only needs
    /// to be unique on this LAN, which a random id already guarantees.
    /// `fingerprint_prefix` lets a browsing client pre-filter results
    /// before ever opening a connection; the full fingerprint is still
    /// verified over the real TLS connection at pairing time.
    pub fn start(
        device_id: &str,
        device_name: &str,
        fingerprint_prefix: &str,
        port: u16,
    ) -> Result<Self, String> {
        let daemon = ServiceDaemon::new().map_err(|e| {
            tracing::warn!("remote access: mdns daemon failed to start: {e}");
            "Couldn't start LAN discovery.".to_string()
        })?;
        let hostname = format!("{device_id}.local.");
        let properties = [
            ("id", device_id),
            ("name", device_name),
            ("fp", fingerprint_prefix),
            ("ver", "1"),
        ];
        // Empty addrs + `enable_addr_auto()`: let `mdns-sd` discover this
        // machine's own addresses rather than duplicating that logic here
        // (it already has to enumerate interfaces for its own responder).
        let service_info = ServiceInfo::new(
            SERVICE_TYPE,
            device_id,
            &hostname,
            "",
            port,
            &properties[..],
        )
        .map_err(|e| {
            tracing::warn!("remote access: mdns service info invalid: {e}");
            "Couldn't start LAN discovery.".to_string()
        })?
        .enable_addr_auto();
        let fullname = service_info.get_fullname().to_string();
        daemon.register(service_info).map_err(|e| {
            tracing::warn!("remote access: mdns register failed: {e}");
            "Couldn't start LAN discovery.".to_string()
        })?;
        Ok(Self { daemon, fullname })
    }

    /// Unregister and shut down the daemon. Runs the (blocking) daemon
    /// shutdown on a blocking-safe thread since this may be called from a
    /// live tokio task (the user flipping Remote Access off), not just app
    /// exit.
    pub async fn stop(self) {
        let Self { daemon, fullname } = self;
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(recv) = daemon.unregister(&fullname) {
                let _ = recv.recv_timeout(std::time::Duration::from_secs(2));
            }
            let _ = daemon.shutdown();
        })
        .await;
    }
}
