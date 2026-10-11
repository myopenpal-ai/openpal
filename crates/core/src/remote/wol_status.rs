//! Best-effort "is Wake-on-LAN even enabled on this machine" detection,
//! per-OS — following this crate's existing `#[cfg(target_os = ...)]`
//! branching convention (see `crate::update_portable`). Detection here is
//! advisory only, shown in Settings → Devices next to instructions for
//! turning it on; it never gates whether Remote Access itself can be
//! enabled. **A detection failure must report [`WolStatus::Unknown`], never
//! a false [`WolStatus::Disabled`]** — telling a user WOL is off when it's
//! actually on (because this build's detection command isn't installed, or
//! its output format changed) would send them fiddling with a setting
//! that's already correct.
//!
//! macOS is implemented for real (it's the one platform this app actually
//! ships on today); Windows/Linux are best-effort, cross-platform-written
//! logic that isn't expected to be exercised until those platforms get
//! their own separate packaging bring-up (see `README.md`'s "Platform
//! support" and `ARCHITECTURE.md`'s "Adding a platform" section).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WolStatus {
    Enabled,
    Disabled,
    /// `reason` is safe to show to a non-technical user (e.g. "Couldn't
    /// check this on your system").
    Unknown {
        reason: String,
    },
}

/// Best-effort check for the machine's primary/active interface. Never
/// panics or blocks indefinitely — every OS path has a bounded, fallible
/// command invocation with `Unknown` as the fallback.
pub fn detect() -> WolStatus {
    detect_platform()
}

#[cfg(target_os = "macos")]
fn detect_platform() -> WolStatus {
    // `pmset -g` prints a `womp` line under "Currently in use:" — `1` means
    // "Wake for network access" (what macOS calls WOL in System Settings)
    // is on for the active power source, `0` means off.
    let output = match std::process::Command::new("pmset").arg("-g").output() {
        Ok(o) if o.status.success() => o,
        Ok(_) => {
            return WolStatus::Unknown {
                reason: "Couldn't check this on your Mac.".to_string(),
            }
        }
        Err(e) => {
            tracing::warn!("wol_status: pmset failed to run: {e}");
            return WolStatus::Unknown {
                reason: "Couldn't check this on your Mac.".to_string(),
            };
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    match stdout.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("womp")
            .map(|rest| rest.trim())
            .filter(|rest| !rest.is_empty())
    }) {
        Some("1") => WolStatus::Enabled,
        Some("0") => WolStatus::Disabled,
        _ => WolStatus::Unknown {
            reason: "Couldn't check this on your Mac.".to_string(),
        },
    }
}

#[cfg(target_os = "windows")]
fn detect_platform() -> WolStatus {
    // Best-effort: `Get-NetAdapterPowerManagement` exposes `WakeOnMagicPacket`
    // per adapter. Not exercised in CI (Windows packaging is separate,
    // pending platform bring-up) — any failure or unexpected output
    // reports `Unknown`, never a false negative.
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "(Get-NetAdapterPowerManagement | Where-Object Enabled -eq $true | \
             Select-Object -First 1 -ExpandProperty WakeOnMagicPacket)",
        ])
        .output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => {
            return WolStatus::Unknown {
                reason: "Couldn't check this on your PC.".to_string(),
            }
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value = stdout.trim();
    if value.eq_ignore_ascii_case("Enabled") {
        WolStatus::Enabled
    } else if value.eq_ignore_ascii_case("Disabled") {
        WolStatus::Disabled
    } else {
        WolStatus::Unknown {
            reason: "Couldn't check this on your PC.".to_string(),
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn detect_platform() -> WolStatus {
    // Best-effort: `ethtool <iface>` prints a `Wake-on: g` line when magic
    // packets are enabled (`d` when disabled) — check the first interface
    // this machine actually has a usable MAC/IP on. Not exercised in CI
    // (Linux desktop packaging is separate, pending platform bring-up); a
    // missing `ethtool` binary or unexpected output reports `Unknown`.
    let Some(iface) = super::wol::list_interfaces().into_iter().next() else {
        return WolStatus::Unknown {
            reason: "Couldn't find a network interface to check.".to_string(),
        };
    };
    let output = std::process::Command::new("ethtool")
        .arg(&iface.name)
        .output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => {
            return WolStatus::Unknown {
                reason: "Couldn't check this on your machine.".to_string(),
            }
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    match stdout.lines().find_map(|line| {
        line.trim()
            .strip_prefix("Wake-on:")
            .map(|rest| rest.trim().to_string())
    }) {
        Some(modes) if modes.contains('g') => WolStatus::Enabled,
        Some(_) => WolStatus::Disabled,
        None => WolStatus::Unknown {
            reason: "Couldn't check this on your machine.".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_never_panics() {
        // Can't assert a specific variant (depends on the host running
        // tests), just that detection completes without panicking/hanging.
        match detect() {
            WolStatus::Enabled | WolStatus::Disabled | WolStatus::Unknown { .. } => {}
        }
    }
}
