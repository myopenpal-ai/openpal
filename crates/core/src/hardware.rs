//! Detect the user's machine so we can recommend the right model.
//!
//! Implementation notes (macOS first, but keep it compiling everywhere):
//! - Use `sysinfo` for RAM (total/available, in bytes), CPU core count and
//!   disk free space (find the disk whose mount point is the longest prefix
//!   of `models_dir`, fall back to the largest disk).
//! - Chip name: `sysinfo` CPU brand, with a fallback to
//!   `sysctl -n machdep.cpu.brand_string` on macOS (gives "Apple M4" etc.).
//! - `is_apple_silicon`: on macOS, `sysctl -n hw.optional.arm64` == "1"
//!   (or `cfg!(target_arch = "aarch64")` when running natively).
//! - `has_metal`: true iff Apple Silicon (every M-series chip supports the
//!   Metal acceleration llama.cpp uses). Intel Macs run CPU-only for now.
//! - `os_version`: "macOS " + `sw_vers -productVersion` on macOS; fall back to
//!   `sysinfo` OS name/version elsewhere.
//!
//! Detection must never panic and should complete in well under a second.

use crate::types::HardwareInfo;
use std::path::{Path, PathBuf};
use sysinfo::{Disks, System};

/// Snapshot the machine. `models_dir` tells us which volume's free space
/// matters (models are large; this is the number we check before downloads).
pub fn detect(models_dir: &Path) -> HardwareInfo {
    let sys = System::new_all();

    // Runtime check (not cfg!) so an x86_64 build running under Rosetta on an
    // Apple Silicon Mac still detects the real hardware.
    let is_apple_silicon =
        command_output("sysctl", &["-n", "hw.optional.arm64"]).is_some_and(|v| v == "1");

    let chip = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .filter(|b| !b.is_empty())
        .or_else(|| command_output("sysctl", &["-n", "machdep.cpu.brand_string"]))
        .unwrap_or_else(|| "Unknown".to_string());

    let cpu_cores = System::physical_core_count()
        .unwrap_or_else(|| sys.cpus().len())
        .max(1);

    let os_version = if cfg!(target_os = "macos") {
        command_output("sw_vers", &["-productVersion"]).map(|v| format!("macOS {v}"))
    } else {
        None
    }
    .or_else(|| match (System::name(), System::os_version()) {
        (Some(name), Some(version)) => Some(format!("{name} {version}")),
        (Some(name), None) => Some(name),
        _ => None,
    })
    .unwrap_or_else(|| "Unknown".to_string());

    HardwareInfo {
        chip,
        is_apple_silicon,
        total_ram_bytes: sys.total_memory(),
        available_ram_bytes: sys.available_memory(),
        cpu_cores,
        has_metal: is_apple_silicon,
        free_disk_bytes: free_disk_bytes(models_dir),
        os_version,
        // Filled in separately (once, at startup) by asking the bundled
        // engine binary — see `gpu::detect_vulkan_gpu` and
        // `crates/app/src/state.rs`'s `finish_unlock`. This detector has no
        // engine path.
        gpu_name: None,
        gpu_vram_bytes: None,
    }
}

/// Run a command and return its trimmed stdout, or None on any failure.
fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Free space on the volume holding `models_dir`: the disk whose mount point
/// is the longest path-prefix of the (canonicalized) directory, then the
/// usual macOS data volume, then "/", then the largest disk.
pub(crate) fn free_disk_bytes(models_dir: &Path) -> u64 {
    let disks = Disks::new_with_refreshed_list();
    let canonical = canonicalize_nearest_existing(models_dir);

    let by_prefix = canonical.as_deref().and_then(|dir| {
        disks
            .list()
            .iter()
            .filter(|d| dir.starts_with(d.mount_point()))
            .max_by_key(|d| d.mount_point().as_os_str().len())
    });

    by_prefix
        .or_else(|| {
            disks
                .list()
                .iter()
                .find(|d| d.mount_point() == Path::new("/System/Volumes/Data"))
        })
        .or_else(|| {
            disks
                .list()
                .iter()
                .find(|d| d.mount_point() == Path::new("/"))
        })
        .or_else(|| disks.list().iter().max_by_key(|d| d.total_space()))
        .map(|d| d.available_space())
        .unwrap_or(0)
}

/// Canonicalize `path`, walking up to the nearest existing ancestor when the
/// directory itself hasn't been created yet.
fn canonicalize_nearest_existing(path: &Path) -> Option<PathBuf> {
    let mut current = path;
    loop {
        if let Ok(canonical) = current.canonicalize() {
            return Some(canonical);
        }
        current = current.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_sane_values_and_never_panics() {
        let dir = tempfile::tempdir().unwrap();
        let hw = detect(dir.path());
        assert!(hw.total_ram_bytes > 0);
        assert!(hw.available_ram_bytes <= hw.total_ram_bytes);
        assert!(hw.cpu_cores >= 1);
        assert!(!hw.chip.is_empty());
        assert!(!hw.os_version.is_empty());
        assert!(
            hw.free_disk_bytes > 0,
            "test machine should have free disk space"
        );
        // Metal availability tracks Apple Silicon exactly.
        assert_eq!(hw.has_metal, hw.is_apple_silicon);
    }

    #[test]
    fn detect_handles_a_models_dir_that_does_not_exist_yet() {
        let dir = tempfile::tempdir().unwrap();
        let hw = detect(&dir.path().join("not/created/yet"));
        assert!(hw.free_disk_bytes > 0);
    }
}
