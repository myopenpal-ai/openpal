//! Detect a Vulkan-visible GPU by asking the bundled engine itself.
//!
//! `llama-server --list-devices` prints exactly the devices the engine will
//! use, e.g.:
//!
//! ```text
//! Available devices:
//!   Vulkan0: NVIDIA GeForce RTX 3060 (12227 MiB, 12106 MiB free)
//!   CPU: AMD Ryzen 7 5800X
//! ```
//!
//! Asking the binary (instead of probing Vulkan ourselves) means the answer
//! can't disagree with what the engine actually supports: a CPU-only build
//! lists no `Vulkan*` device and we correctly detect nothing. macOS builds
//! list `MTL0` (Metal), which is deliberately ignored here — Metal
//! acceleration is tracked by `HardwareInfo::has_metal`, and the
//! bigger-than-RAM handling for Metal is entirely different (see
//! `docs/design/DISK_OFFLOAD.md`: Metal *wires* mmapped weights, Vulkan *copies*
//! them, which is why GPU offload of a huge MoE is safe on Vulkan only).
//!
//! Detection runs once at startup (`crates/app/src/state.rs`'s
//! `finish_unlock`) — the GPU doesn't change while the app runs.

use std::path::Path;

/// A GPU the engine's Vulkan backend can use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VulkanGpu {
    pub name: String,
    /// Reported device memory. Integrated GPUs may report shared system
    /// RAM here — an upper bound, not necessarily dedicated VRAM.
    pub vram_bytes: u64,
}

/// Run `engine_binary --list-devices` and return the largest-memory Vulkan
/// device, or `None` when there is none (CPU-only build, no GPU, macOS) or
/// the probe fails in any way — GPU detection must never break startup.
pub fn detect_vulkan_gpu(engine_binary: &Path) -> Option<VulkanGpu> {
    let output = std::process::Command::new(engine_binary)
        .arg("--list-devices")
        .output()
        .ok()?;
    // `--list-devices` prints to stdout and exits 0, but be tolerant: parse
    // whatever it printed even on a nonzero exit (some builds exit nonzero
    // after listing when no model was given).
    let text = String::from_utf8_lossy(&output.stdout);
    parse_list_devices(&text)
}

/// Parse `--list-devices` output for `Vulkan<N>: <name> (<total> MiB, ...)`
/// lines, returning the device with the most memory.
fn parse_list_devices(output: &str) -> Option<VulkanGpu> {
    output
        .lines()
        .filter_map(parse_vulkan_line)
        .max_by_key(|gpu| gpu.vram_bytes)
}

fn parse_vulkan_line(line: &str) -> Option<VulkanGpu> {
    let line = line.trim();
    let rest = line.strip_prefix("Vulkan")?;
    // "0: NVIDIA GeForce RTX 3060 (12227 MiB, 12106 MiB free)"
    let (index, rest) = rest.split_once(':')?;
    if index.is_empty() || !index.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let rest = rest.trim();
    // Name is everything before the final "(total MiB, free MiB free)".
    let open = rest.rfind('(')?;
    let name = rest[..open].trim();
    let parens = rest[open + 1..].strip_suffix(')')?;
    let total = parens.split(',').next()?.trim();
    let mib: u64 = total.strip_suffix(" MiB")?.trim().parse().ok()?;
    (!name.is_empty()).then(|| VulkanGpu {
        name: name.to_string(),
        vram_bytes: mib * 1024 * 1024,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_discrete_vulkan_gpu() {
        let out =
            "Available devices:\n  Vulkan0: NVIDIA GeForce RTX 3060 (12227 MiB, 12106 MiB free)\n";
        let gpu = parse_list_devices(out).unwrap();
        assert_eq!(gpu.name, "NVIDIA GeForce RTX 3060");
        assert_eq!(gpu.vram_bytes, 12227 * 1024 * 1024);
    }

    #[test]
    fn picks_the_largest_of_several_vulkan_devices() {
        let out = "Available devices:\n\
                   \x20 Vulkan0: AMD Radeon Graphics (8192 MiB, 8000 MiB free)\n\
                   \x20 Vulkan1: AMD Radeon RX 7900 XT (20464 MiB, 20300 MiB free)\n";
        let gpu = parse_list_devices(out).unwrap();
        assert_eq!(gpu.name, "AMD Radeon RX 7900 XT");
    }

    #[test]
    fn ignores_metal_and_cpu_devices() {
        // Real output shape of the bundled macOS b9833 build.
        let out = "Available devices:\n\
                   \x20 MTL0: Apple M4 (12124 MiB, 12123 MiB free)\n\
                   \x20 BLAS: Accelerate (0 MiB, 0 MiB free)\n";
        assert_eq!(parse_list_devices(out), None);
    }

    #[test]
    fn tolerates_garbage_and_empty_output() {
        assert_eq!(parse_list_devices(""), None);
        assert_eq!(parse_list_devices("error: unknown flag"), None);
        assert_eq!(
            parse_list_devices("VulkanX: bad (not a number MiB, x)"),
            None
        );
        assert_eq!(parse_list_devices("Vulkan0: no parens here"), None);
    }
}
