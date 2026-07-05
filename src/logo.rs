//! Procedural app logo: Logitech-blue mouse glyph on a dark rounded square.
//! Drawn with signed-distance functions so it stays crisp at any size and
//! nothing needs to be shipped as an image asset.

pub const LOGI_BLUE: (u8, u8, u8) = (0x00, 0xb8, 0xfc);
const BG: (u8, u8, u8) = (0x16, 0x16, 0x18);

/// Straight-alpha RGBA pixels, `size` x `size`.
pub fn logo_rgba(size: usize) -> Vec<u8> {
    let s = size as f32;
    // Antialias over ~1 pixel: distance is in normalized units, so scale by size.
    let aa = |d: f32| (0.5 - d * s).clamp(0.0, 1.0);

    let mut rgba = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let px = (x as f32 + 0.5) / s;
            let py = (y as f32 + 0.5) / s;

            let bg_a = aa(sd_rounded_square(px, py, 0.02, 0.18));
            // Mouse body: vertical capsule.
            let body = sd_segment(px, py, 0.5, 0.44, 0.5, 0.58) - 0.19;
            let outline_a = aa(body.abs() - 0.035);
            // Scroll wheel / button split.
            let wheel_a = aa(sd_segment(px, py, 0.5, 0.33, 0.5, 0.45) - 0.032);
            let blue_a = outline_a.max(wheel_a);

            let alpha = bg_a;
            if alpha <= 0.0 {
                continue;
            }
            let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * blue_a) as u8;
            let i = (y * size + x) * 4;
            rgba[i] = mix(BG.0, LOGI_BLUE.0);
            rgba[i + 1] = mix(BG.1, LOGI_BLUE.1);
            rgba[i + 2] = mix(BG.2, LOGI_BLUE.2);
            rgba[i + 3] = (alpha * 255.0) as u8;
        }
    }
    rgba
}

/// Rounded square centered in the unit box with `margin` inset and corner radius `r`.
fn sd_rounded_square(px: f32, py: f32, margin: f32, r: f32) -> f32 {
    let half = 0.5 - margin - r;
    let bx = (px - 0.5).abs() - half;
    let by = (py - 0.5).abs() - half;
    let outside = (bx.max(0.0).powi(2) + by.max(0.0).powi(2)).sqrt();
    outside + bx.max(by).min(0.0) - r
}

/// Distance from point to the segment (ax,ay)-(bx,by).
fn sd_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let (ex, ey) = (px - ax, py - ay);
    let t = ((ex * dx + ey * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    let (cx, cy) = (ex - dx * t, ey - dy * t);
    (cx * cx + cy * cy).sqrt()
}
