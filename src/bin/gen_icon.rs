use disk_cleaner::app_icon;
use image::{ImageBuffer, Rgba};
use std::path::PathBuf;

fn main() {
    let icon = app_icon::generate();
    let img = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(icon.width, icon.height, icon.rgba)
        .expect("Failed to create image buffer");

    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("icon_1024.png"));
    img.save(&path).expect("Failed to save icon");
    eprintln!("Icon saved to {}", path.display());
}
