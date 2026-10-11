//! `icon`: generate the OpenPal app icon, then run `tauri icon` to produce
//! every platform format into crates/app/icons/.
//!
//! Source image: a procedurally drawn 1024×1024 RGBA PNG written to
//! `bundler/out/icon-source.png`. Design (macOS app icon conventions —
//! a rounded-square tile is applied by `tauri icon`/the OS, so draw the tile
//! ourselves):
//! - Rounded-square tile (corner radius ~225px for 1024) filled with a
//!   vertical gradient: #D8744A (top) → #A63D14 (bottom) — the app's warm "Hearth" palette, on a fully
//!   transparent background outside the tile.
//! - Centered cream (#FFF7EE) speech-bubble: a rounded rectangle (~560×420, radius
//!   ~120) with a small triangular tail at the bottom-left, plus three
//!   terracotta (#B34A1F) dots (radius ~36, spaced ~120) centered inside it.
//! - Anti-alias by supersampling: draw at 4096² with simple SDF
//!   (signed-distance) coverage per shape, then box-downsample 4× to 1024².
//!   Helper SDFs: rounded_rect_sdf(p, half_extents, radius), circle, triangle.
//! - Keep the drawing code small and dependency-free beyond the `image`
//!   crate.
//!
//! Then (unless `source_only`): run `npx --no-install tauri icon
//! <source.png> -o crates/app/icons` from the repo root and verify
//! `icons/icon.icns` exists afterwards. Tell the user to `npm install` at the
//! repo root if npx can't find the tauri CLI.

use anyhow::{bail, Context};
use image::{Rgba, RgbaImage};

use crate::paths;

const SIZE: u32 = 1024;
/// Supersampling factor: shapes are sampled on a 4096² grid (4×4 per output
/// pixel) and box-averaged down — the streamed equivalent of rendering the
/// full 4096² image and downsampling 4×.
const SS: u32 = 4;

// Colors (0..1 per channel, sRGB values) — the app's "Hearth" palette:
// warm terracotta tile, cream speech bubble.
const GRAD_TOP: [f32; 3] = [216.0 / 255.0, 116.0 / 255.0, 74.0 / 255.0]; // #D8744A
const GRAD_BOTTOM: [f32; 3] = [166.0 / 255.0, 61.0 / 255.0, 20.0 / 255.0]; // #A63D14
const BUBBLE_COLOR: [f32; 3] = [1.0, 247.0 / 255.0, 238.0 / 255.0]; // #FFF7EE
const DOT_COLOR: [f32; 3] = [179.0 / 255.0, 74.0 / 255.0, 31.0 / 255.0]; // #B34A1F

// Geometry, in 1024-space.
const TILE_CENTER: (f32, f32) = (512.0, 512.0);
const TILE_HALF: (f32, f32) = (484.0, 484.0);
const TILE_RADIUS: f32 = 225.0;
const BUBBLE_CENTER: (f32, f32) = (512.0, 480.0);
const BUBBLE_HALF: (f32, f32) = (280.0, 210.0); // ~560×420
const BUBBLE_RADIUS: f32 = 120.0;
// Tail triangle at the bubble's bottom-left; its base overlaps the bubble so
// the white union is seamless.
const TAIL_A: (f32, f32) = (330.0, 640.0);
const TAIL_B: (f32, f32) = (470.0, 640.0);
const TAIL_TIP: (f32, f32) = (292.0, 806.0);
const DOT_RADIUS: f32 = 36.0;
const DOT_Y: f32 = 480.0;
const DOT_XS: [f32; 3] = [392.0, 512.0, 632.0]; // spaced 120

pub fn run(source_only: bool) -> anyhow::Result<()> {
    let out_dir = paths::repo_root().join("bundler/out");
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("failed to create {}", out_dir.display()))?;
    let source = out_dir.join("icon-source.png");

    println!("Drawing {SIZE}×{SIZE} icon source ({SS}× supersampled)…");
    let img = draw();
    img.save(&source)
        .with_context(|| format!("failed to write {}", source.display()))?;
    println!("Wrote {}", source.display());

    if source_only {
        return Ok(());
    }

    println!("Generating platform icons via `tauri icon`…");
    let status = crate::node::node_cli("npx")
        .args(["--no-install", "tauri", "icon"])
        .arg(&source)
        .args(["-o", "crates/app/icons"])
        .current_dir(paths::repo_root())
        .status()
        .context("failed to run npx — is Node installed and on PATH?")?;
    if !status.success() {
        bail!("`tauri icon` failed — if the Tauri CLI is missing, run: npm install (at the repo root)");
    }
    let icns = paths::repo_root().join("crates/app/icons/icon.icns");
    if !icns.exists() {
        bail!(
            "`tauri icon` ran but {} was not generated — check the output above",
            icns.display()
        );
    }
    println!(
        "Generated icon set in {}",
        icns.parent().expect("icons dir").display()
    );
    Ok(())
}

fn draw() -> RgbaImage {
    let mut img = RgbaImage::new(SIZE, SIZE);
    let step = 1.0 / SS as f32;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let center = (x as f32 + 0.5, y as f32 + 0.5);
            let (d_tile, d_bubble, d_dots) = scene_sdfs(center);
            let premul = if d_tile > 1.0 {
                // Fully outside the tile: transparent.
                [0.0; 4]
            } else if d_tile.abs() > 1.0 && d_bubble.abs() > 1.0 && d_dots.abs() > 1.0 {
                // Far from every edge: all 16 supersamples agree, so one
                // sample is exact (the gradient varies negligibly per pixel).
                shade(center)
            } else {
                // Edge band: average the full 4×4 supersample grid.
                let mut acc = [0.0f32; 4];
                for sy in 0..SS {
                    for sx in 0..SS {
                        let p = (
                            x as f32 + (sx as f32 + 0.5) * step,
                            y as f32 + (sy as f32 + 0.5) * step,
                        );
                        let s = shade(p);
                        for (a, v) in acc.iter_mut().zip(s) {
                            *a += v;
                        }
                    }
                }
                acc.map(|v| v / (SS * SS) as f32)
            };
            let alpha = premul[3];
            let (r, g, b) = if alpha > 0.0 {
                (premul[0] / alpha, premul[1] / alpha, premul[2] / alpha)
            } else {
                (0.0, 0.0, 0.0)
            };
            img.put_pixel(x, y, Rgba([to_u8(r), to_u8(g), to_u8(b), to_u8(alpha)]));
        }
    }
    img
}

/// Premultiplied RGBA at a sample point.
fn shade(p: (f32, f32)) -> [f32; 4] {
    let (d_tile, d_bubble, d_dots) = scene_sdfs(p);
    let tile_cov = coverage(d_tile);
    if tile_cov <= 0.0 {
        return [0.0; 4];
    }

    // Vertical gradient across the tile.
    let t = ((p.1 - (TILE_CENTER.1 - TILE_HALF.1)) / (2.0 * TILE_HALF.1)).clamp(0.0, 1.0);
    let mut rgb = [
        lerp(GRAD_TOP[0], GRAD_BOTTOM[0], t),
        lerp(GRAD_TOP[1], GRAD_BOTTOM[1], t),
        lerp(GRAD_TOP[2], GRAD_BOTTOM[2], t),
    ];

    // Cream speech bubble (rounded rect ∪ tail triangle), then the dots.
    let bubble_cov = coverage(d_bubble);
    let dot_cov = coverage(d_dots);
    for i in 0..3 {
        rgb[i] = lerp(rgb[i], BUBBLE_COLOR[i], bubble_cov);
        rgb[i] = lerp(rgb[i], DOT_COLOR[i], dot_cov);
    }

    [
        rgb[0] * tile_cov,
        rgb[1] * tile_cov,
        rgb[2] * tile_cov,
        tile_cov,
    ]
}

/// Signed distances (tile, bubble ∪ tail, dots) at a point.
fn scene_sdfs(p: (f32, f32)) -> (f32, f32, f32) {
    let d_tile = rounded_rect_sdf(p, TILE_CENTER, TILE_HALF, TILE_RADIUS);
    let d_bubble = rounded_rect_sdf(p, BUBBLE_CENTER, BUBBLE_HALF, BUBBLE_RADIUS)
        .min(triangle_sdf(p, TAIL_A, TAIL_B, TAIL_TIP));
    let d_dots = DOT_XS
        .iter()
        .map(|&x| circle_sdf(p, (x, DOT_Y), DOT_RADIUS))
        .fold(f32::INFINITY, f32::min);
    (d_tile, d_bubble, d_dots)
}

/// Coverage ramp one supersample (1/SS px) wide around the zero crossing.
fn coverage(d: f32) -> f32 {
    (0.5 - d * SS as f32).clamp(0.0, 1.0)
}

fn rounded_rect_sdf(p: (f32, f32), center: (f32, f32), half: (f32, f32), radius: f32) -> f32 {
    let qx = (p.0 - center.0).abs() - (half.0 - radius);
    let qy = (p.1 - center.1).abs() - (half.1 - radius);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
}

fn circle_sdf(p: (f32, f32), center: (f32, f32), radius: f32) -> f32 {
    ((p.0 - center.0).powi(2) + (p.1 - center.1).powi(2)).sqrt() - radius
}

/// Exact signed distance to a triangle (Inigo Quilez's construction).
fn triangle_sdf(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> f32 {
    let (e0, e1, e2) = (sub(b, a), sub(c, b), sub(a, c));
    let (v0, v1, v2) = (sub(p, a), sub(p, b), sub(p, c));
    let pq0 = sub(v0, mul(e0, (dot(v0, e0) / dot(e0, e0)).clamp(0.0, 1.0)));
    let pq1 = sub(v1, mul(e1, (dot(v1, e1) / dot(e1, e1)).clamp(0.0, 1.0)));
    let pq2 = sub(v2, mul(e2, (dot(v2, e2) / dot(e2, e2)).clamp(0.0, 1.0)));
    let s = (e0.0 * e2.1 - e0.1 * e2.0).signum();
    let dist_sq = dot(pq0, pq0).min(dot(pq1, pq1)).min(dot(pq2, pq2));
    let side = (s * (v0.0 * e0.1 - v0.1 * e0.0))
        .min(s * (v1.0 * e1.1 - v1.1 * e1.0))
        .min(s * (v2.0 * e2.1 - v2.1 * e2.0));
    -dist_sq.sqrt() * side.signum()
}

fn sub(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    (a.0 - b.0, a.1 - b.1)
}

fn dot(a: (f32, f32), b: (f32, f32)) -> f32 {
    a.0 * b.0 + a.1 * b.1
}

fn mul(a: (f32, f32), s: f32) -> (f32, f32) {
    (a.0 * s, a.1 * s)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circle_sdf_is_negative_inside_zero_on_boundary_positive_outside() {
        let center = (10.0, 10.0);
        let radius = 5.0;
        assert_eq!(circle_sdf(center, center, radius), -radius);
        assert!((circle_sdf((center.0 + radius, center.1), center, radius)).abs() < 1e-5);
        assert_eq!(
            circle_sdf((center.0 + radius * 2.0, center.1), center, radius),
            radius
        );
    }

    #[test]
    fn rounded_rect_sdf_matches_a_plain_rect_at_zero_radius() {
        let center = (0.0, 0.0);
        let half = (10.0, 5.0);
        // With radius 0.0 this degenerates to an axis-aligned rect: the point
        // exactly on the right edge, mid-height, has distance 0.
        let on_edge = rounded_rect_sdf((10.0, 0.0), center, half, 0.0);
        assert!(on_edge.abs() < 1e-5);
        // The center is inside, at negative distance equal to the shortest
        // side's half-extent.
        assert_eq!(rounded_rect_sdf(center, center, half, 0.0), -5.0);
        // Well outside is positive.
        assert!(rounded_rect_sdf((100.0, 100.0), center, half, 0.0) > 0.0);
    }

    #[test]
    fn coverage_is_full_inside_and_zero_well_outside() {
        assert_eq!(coverage(-10.0), 1.0);
        assert_eq!(coverage(10.0), 0.0);
    }

    #[test]
    fn lerp_interpolates_linearly() {
        assert_eq!(lerp(0.0, 10.0, 0.0), 0.0);
        assert_eq!(lerp(0.0, 10.0, 1.0), 10.0);
        assert_eq!(lerp(0.0, 10.0, 0.5), 5.0);
    }

    #[test]
    fn to_u8_clamps_before_scaling() {
        assert_eq!(to_u8(-1.0), 0);
        assert_eq!(to_u8(0.0), 0);
        assert_eq!(to_u8(1.0), 255);
        assert_eq!(to_u8(2.0), 255);
        assert_eq!(to_u8(0.5), 128); // 0.5 * 255 = 127.5, rounds to 128
    }
}
