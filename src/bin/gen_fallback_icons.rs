/// Generate 64×64 fallback folder and file icons for HiDPI displays.
///
/// Run: cargo run --bin gen_fallback_icons
/// Output: assets/folder_icon.png, assets/file_icon.png
use image::{ImageBuffer, Rgba, RgbaImage};
use std::path::Path;

const SIZE: u32 = 64;
const TRANSPARENT: Rgba<u8> = Rgba([0, 0, 0, 0]);

fn main() {
    let folder = generate_folder_icon();
    let file = generate_file_icon();

    let assets = Path::new("assets");
    std::fs::create_dir_all(assets).expect("create assets dir");

    folder
        .save(assets.join("folder_icon.png"))
        .expect("save folder icon");
    file.save(assets.join("file_icon.png"))
        .expect("save file icon");

    eprintln!("Generated 64x64 icons in assets/");
}

fn blend(bg: Rgba<u8>, fg: Rgba<u8>, alpha: f32) -> Rgba<u8> {
    let a = (fg.0[3] as f32 / 255.0) * alpha;
    let inv = 1.0 - a;
    Rgba([
        (fg.0[0] as f32 * a + bg.0[0] as f32 * inv) as u8,
        (fg.0[1] as f32 * a + bg.0[1] as f32 * inv) as u8,
        (fg.0[2] as f32 * a + bg.0[2] as f32 * inv) as u8,
        ((a + bg.0[3] as f32 / 255.0 * inv) * 255.0).min(255.0) as u8,
    ])
}

fn put_aa(img: &mut RgbaImage, x: i32, y: i32, color: Rgba<u8>, alpha: f32) {
    if x >= 0 && y >= 0 && (x as u32) < SIZE && (y as u32) < SIZE && alpha > 0.0 {
        let prev = *img.get_pixel(x as u32, y as u32);
        img.put_pixel(x as u32, y as u32, blend(prev, color, alpha));
    }
}

fn fill_rect(img: &mut RgbaImage, x1: u32, y1: u32, x2: u32, y2: u32, color: Rgba<u8>) {
    for y in y1..y2 {
        for x in x1..x2 {
            if x < SIZE && y < SIZE {
                img.put_pixel(x, y, color);
            }
        }
    }
}

fn fill_rounded_rect(
    img: &mut RgbaImage,
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
    r: u32,
    color: Rgba<u8>,
) {
    // Fill center and edge strips
    fill_rect(img, x1 + r, y1, x2 - r, y2, color);
    fill_rect(img, x1, y1 + r, x1 + r, y2 - r, color);
    fill_rect(img, x2 - r, y1 + r, x2, y2 - r, color);

    // Fill rounded corners with AA
    for corner in 0..4 {
        let (cx, cy) = match corner {
            0 => (x1 + r, y1 + r), // top-left
            1 => (x2 - r - 1, y1 + r), // top-right
            2 => (x1 + r, y2 - r - 1), // bottom-left
            _ => (x2 - r - 1, y2 - r - 1), // bottom-right
        };
        for dy in 0..=r {
            for dx in 0..=r {
                let dist = ((dx * dx + dy * dy) as f32).sqrt();
                if dist <= r as f32 {
                    let alpha = if dist > (r as f32 - 1.0) {
                        r as f32 - dist + 1.0
                    } else {
                        1.0
                    }
                    .clamp(0.0, 1.0);

                    let (px, py) = match corner {
                        0 => (cx as i32 - dx as i32, cy as i32 - dy as i32),
                        1 => (cx as i32 + dx as i32, cy as i32 - dy as i32),
                        2 => (cx as i32 - dx as i32, cy as i32 + dy as i32),
                        _ => (cx as i32 + dx as i32, cy as i32 + dy as i32),
                    };
                    put_aa(img, px, py, color, alpha);
                }
            }
        }
    }
}

fn generate_folder_icon() -> RgbaImage {
    let mut img: RgbaImage = ImageBuffer::from_pixel(SIZE, SIZE, TRANSPARENT);

    let body = Rgba([66, 152, 250, 255]); // macOS-style blue
    let tab = Rgba([56, 138, 235, 255]); // slightly darker tab
    let highlight = Rgba([100, 175, 255, 255]); // top highlight

    // Tab: top-left area
    fill_rounded_rect(&mut img, 6, 12, 28, 22, 3, tab);

    // Main body
    fill_rounded_rect(&mut img, 6, 20, 58, 54, 4, body);

    // Top highlight strip on body
    fill_rect(&mut img, 10, 21, 54, 23, highlight);

    img
}

fn generate_file_icon() -> RgbaImage {
    let mut img: RgbaImage = ImageBuffer::from_pixel(SIZE, SIZE, TRANSPARENT);

    let page = Rgba([245, 247, 250, 255]); // off-white
    let border = Rgba([185, 195, 210, 255]); // subtle border
    let fold_bg = Rgba([215, 222, 232, 255]); // dog-ear fill
    let line_color = Rgba([195, 205, 218, 255]); // text lines

    let left = 12_u32;
    let right = 52_u32;
    let top = 4_u32;
    let bottom = 60_u32;
    let fold = 12_u32;

    // Page body — skip dog-ear triangle in top-right
    for y in top..bottom {
        for x in left..right {
            let in_fold = y < top + fold && x >= right - fold;
            if in_fold {
                let dx = x - (right - fold);
                let dy = y - top;
                if dx + dy >= fold {
                    continue;
                }
            }
            img.put_pixel(x, y, page);
        }
    }

    // Border: left
    for y in top..bottom {
        img.put_pixel(left, y, border);
    }
    // Border: bottom
    for x in left..=right {
        if x < SIZE {
            img.put_pixel(x, bottom - 1, border);
        }
    }
    // Border: right (below fold)
    for y in (top + fold)..bottom {
        img.put_pixel(right - 1, y, border);
    }
    // Border: top (left of fold)
    for x in left..(right - fold) {
        img.put_pixel(x, top, border);
    }

    // Dog-ear diagonal
    for i in 0..=fold {
        let x = (right - fold + i).min(SIZE - 1);
        let y = (top + i).min(SIZE - 1);
        if x < SIZE && y < SIZE {
            img.put_pixel(x, y, border);
        }
    }
    // Dog-ear: vertical crease line
    for y in top..(top + fold) {
        let x = right - fold;
        if x < SIZE && y < SIZE {
            img.put_pixel(x, y, fold_bg);
        }
    }
    // Dog-ear: horizontal crease line
    for x in (right - fold)..right {
        let y = top + fold;
        if x < SIZE && y < SIZE {
            img.put_pixel(x, y, fold_bg);
        }
    }
    // Dog-ear fill triangle (the folded part)
    for dy in 0..fold {
        for dx in 0..fold {
            if dx + dy < fold && dx <= dy {
                let x = right - fold + dx;
                let y = top + dy;
                if x < SIZE && y < SIZE {
                    img.put_pixel(x, y, fold_bg);
                }
            }
        }
    }

    // Text lines
    for &(ly, lx_end) in &[(24, 44), (32, 44), (40, 44), (48, 36)] {
        for x in 18..lx_end {
            img.put_pixel(x, ly, line_color);
            img.put_pixel(x, ly + 1, line_color);
        }
    }

    img
}
