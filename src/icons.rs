use eframe::egui;

const FOLDER_PNG: &[u8] = include_bytes!("../assets/folder_icon.png");
const FILE_PNG: &[u8] = include_bytes!("../assets/file_icon.png");

/// Cached icon textures for the tree view.
pub struct IconCache {
    pub folder: egui::TextureHandle,
    pub file: egui::TextureHandle,
}

impl IconCache {
    /// Load the embedded icons and cache them as egui textures.
    /// Returns None only if a PNG fails to decode.
    pub fn load(ctx: &egui::Context) -> Option<Self> {
        let folder = load_png_texture(ctx, "embedded_folder_icon", FOLDER_PNG)?;
        let file = load_png_texture(ctx, "embedded_file_icon", FILE_PNG)?;
        Some(Self { folder, file })
    }
}

fn load_png_texture(
    ctx: &egui::Context,
    name: &str,
    png_bytes: &[u8],
) -> Option<egui::TextureHandle> {
    let icon = eframe::icon_data::from_png_bytes(png_bytes).ok()?;
    let size = [icon.width as usize, icon.height as usize];
    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &icon.rgba);
    Some(ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR))
}
