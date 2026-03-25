use eframe::egui::IconData;

const SIZE: u32 = 1024;
const CENTER: f64 = SIZE as f64 / 2.0;

/// macOS-style squircle (superellipse) icon with a pie chart symbol.
pub fn generate() -> IconData {
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];

    // Squircle parameters (macOS continuous rounded rect)
    let squircle_inset = 100.0; // Apple standard: 100px margin → 824×824 squircle
    let half = CENTER - squircle_inset;
    let exponent = 4.5; // macOS-style continuous curve

    // Background gradient: deep blue-slate at top to brighter blue at bottom
    let bg_top = (30.0, 48.0, 80.0);
    let bg_bot = (44.0, 82.0, 130.0);

    // Pie chart parameters
    let pie_r = half * 0.52; // pie radius relative to squircle
    let pie_cx = CENTER;
    let pie_cy = CENTER + SIZE as f64 * 0.01; // nudge down slightly for optical center

    // Pie segments: (fraction, r, g, b) — representing disk categories
    let segments: &[(f64, u8, u8, u8)] = &[
        (0.38, 100, 180, 255), // blue — large files
        (0.24, 80, 210, 140),  // green — code/dev
        (0.18, 180, 130, 240), // purple — media
        (0.12, 255, 200, 70),  // gold — documents
        (0.08, 255, 110, 90),  // coral — archives
    ];

    // Precompute segment start angles
    let mut start_angles = Vec::with_capacity(segments.len());
    let mut angle = -std::f64::consts::FRAC_PI_2; // start at top
    for &(frac, _, _, _) in segments {
        start_angles.push(angle);
        angle += frac * std::f64::consts::TAU;
    }

    // Small gap between segments (in radians)
    let gap = 0.025;

    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f64 - CENTER;
            let dy = y as f64 - CENTER;
            let idx = ((y * SIZE + x) * 4) as usize;

            // Squircle test: |dx/half|^n + |dy/half|^n <= 1
            let sx = (dx / half).abs();
            let sy = (dy / half).abs();
            let squircle_val = sx.powf(exponent) + sy.powf(exponent);

            if squircle_val <= 1.0 {
                // Inside squircle — compute background
                let t = y as f64 / SIZE as f64; // vertical gradient
                let bg_r = bg_top.0 + (bg_bot.0 - bg_top.0) * t;
                let bg_g = bg_top.1 + (bg_bot.1 - bg_top.1) * t;
                let bg_b = bg_top.2 + (bg_bot.2 - bg_top.2) * t;

                // Subtle inner shadow at top edge
                let edge_dist = 1.0 - squircle_val;
                let shadow = if edge_dist < 0.04 {
                    0.85 + 0.15 * (edge_dist / 0.04)
                } else {
                    1.0
                };

                // Subtle highlight (lighter at top-left)
                let highlight = 1.0 + 0.08 * ((-dx - dy) / (half * 2.0)).clamp(0.0, 1.0);

                let mut r = (bg_r * shadow * highlight).clamp(0.0, 255.0);
                let mut g = (bg_g * shadow * highlight).clamp(0.0, 255.0);
                let mut b = (bg_b * shadow * highlight).clamp(0.0, 255.0);
                let mut a = 255.0;

                // Anti-alias squircle edge
                if squircle_val > 0.985 {
                    a = ((1.0 - squircle_val) / 0.015 * 255.0).clamp(0.0, 255.0);
                }

                // Pie chart
                let pdx = x as f64 - pie_cx;
                let pdy = y as f64 - pie_cy;
                let pie_dist = (pdx * pdx + pdy * pdy).sqrt();

                if pie_dist <= pie_r + 1.0 {
                    let pie_angle = pdy.atan2(pdx);

                    // Find segment
                    let mut seg_idx = segments.len() - 1;
                    for i in 0..segments.len() - 1 {
                        if angle_in_range(pie_angle, start_angles[i], start_angles[i + 1]) {
                            seg_idx = i;
                            break;
                        }
                    }

                    // Check if we're in the gap between segments
                    let in_gap = is_near_segment_edge(
                        pie_angle,
                        &start_angles,
                        segments.len(),
                        gap,
                    );

                    if !in_gap {
                        let (_, sr, sg, sb) = segments[seg_idx];

                        // Subtle radial gradient: lighter toward center
                        let grad = 1.0 + 0.15 * (1.0 - pie_dist / pie_r);

                        let pr = (sr as f64 * grad).clamp(0.0, 255.0);
                        let pg = (sg as f64 * grad).clamp(0.0, 255.0);
                        let pb = (sb as f64 * grad).clamp(0.0, 255.0);

                        // Anti-alias pie edge
                        let pie_alpha = if pie_dist > pie_r - 1.0 {
                            (pie_r - pie_dist + 1.0).clamp(0.0, 1.0)
                        } else {
                            1.0
                        };

                        // Blend pie over background
                        r = r * (1.0 - pie_alpha) + pr * pie_alpha;
                        g = g * (1.0 - pie_alpha) + pg * pie_alpha;
                        b = b * (1.0 - pie_alpha) + pb * pie_alpha;
                    }
                }

                rgba[idx] = r as u8;
                rgba[idx + 1] = g as u8;
                rgba[idx + 2] = b as u8;
                rgba[idx + 3] = a as u8;
            }
        }
    }

    IconData {
        rgba,
        width: SIZE,
        height: SIZE,
    }
}

fn angle_in_range(a: f64, start: f64, end: f64) -> bool {
    let tau = std::f64::consts::TAU;
    let a = ((a % tau) + tau) % tau;
    let s = ((start % tau) + tau) % tau;
    let e = ((end % tau) + tau) % tau;

    if s <= e {
        a >= s && a < e
    } else {
        a >= s || a < e
    }
}

/// Check if an angle is close to any segment boundary (for drawing gaps).
fn is_near_segment_edge(
    angle: f64,
    start_angles: &[f64],
    n_segments: usize,
    gap: f64,
) -> bool {
    let tau = std::f64::consts::TAU;
    let a = ((angle % tau) + tau) % tau;

    for i in 0..n_segments {
        let edge = ((start_angles[i] % tau) + tau) % tau;
        let mut diff = (a - edge).abs();
        if diff > std::f64::consts::PI {
            diff = tau - diff;
        }
        if diff < gap / 2.0 {
            return true;
        }
    }
    false
}
