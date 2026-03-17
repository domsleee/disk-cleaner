use eframe::egui::IconData;

const SIZE: u32 = 1024;
const CENTER: f64 = SIZE as f64 / 2.0;
const OUTER_R: f64 = CENTER - 2.0;
const INNER_R: f64 = OUTER_R * 0.38;

// Pie segments: (fraction of circle, r, g, b)
const SEGMENTS: &[(f64, u8, u8, u8)] = &[
    (0.35, 52, 152, 219),  // blue — large files
    (0.25, 46, 204, 113),  // green — code
    (0.18, 155, 89, 182),  // purple — media
    (0.12, 241, 196, 15),  // yellow — documents
    (0.10, 231, 76, 60),   // red — archives
];

pub fn generate() -> IconData {
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];

    // Precompute segment start angles
    let mut start_angles = Vec::with_capacity(SEGMENTS.len());
    let mut angle = -std::f64::consts::FRAC_PI_2; // start at top
    for &(frac, _, _, _) in SEGMENTS {
        start_angles.push(angle);
        angle += frac * std::f64::consts::TAU;
    }

    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f64 - CENTER;
            let dy = y as f64 - CENTER;
            let dist = (dx * dx + dy * dy).sqrt();

            let idx = ((y * SIZE + x) * 4) as usize;

            if dist <= OUTER_R && dist >= INNER_R {
                let angle = dy.atan2(dx);

                // Find which segment this pixel belongs to
                let mut seg_idx = SEGMENTS.len() - 1;
                for i in 0..SEGMENTS.len() - 1 {
                    // Normalize angles for comparison
                    let start = start_angles[i];
                    let end = start_angles[i + 1];
                    if angle_in_range(angle, start, end) {
                        seg_idx = i;
                        break;
                    }
                }

                let (_, r, g, b) = SEGMENTS[seg_idx];

                // Anti-alias at edges
                let alpha = if dist > OUTER_R - 1.0 {
                    ((OUTER_R - dist) * 255.0).clamp(0.0, 255.0) as u8
                } else if dist < INNER_R + 1.0 {
                    ((dist - INNER_R) * 255.0).clamp(0.0, 255.0) as u8
                } else {
                    255
                };

                rgba[idx] = r;
                rgba[idx + 1] = g;
                rgba[idx + 2] = b;
                rgba[idx + 3] = alpha;
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
    // Normalize all to [0, TAU)
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
