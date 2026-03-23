use eframe::egui;

/// Cached system icon textures for the tree view.
pub struct IconCache {
    pub folder: egui::TextureHandle,
    pub file: egui::TextureHandle,
}

impl IconCache {
    /// Load platform-native system icons and cache them as egui textures.
    /// Returns None on platforms without native icon support (graceful emoji fallback).
    pub fn load(ctx: &egui::Context) -> Option<Self> {
        crate::platform::load_icons(ctx)
    }
}
