use eframe::egui;

/// Cached system icon textures for the tree view.
pub struct IconCache {
    pub folder: egui::TextureHandle,
    pub file: egui::TextureHandle,
}

impl IconCache {
    /// Load macOS system icons and cache them as egui textures.
    /// Returns None on non-macOS or if loading fails.
    pub fn load(ctx: &egui::Context) -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            macos::load_system_icons(ctx)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = ctx;
            None
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use objc2::AnyThread;
    use objc2_app_kit::{
        NSBitmapImageRep, NSCompositingOperation, NSGraphicsContext, NSImage, NSWorkspace,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    const ICON_PX: usize = 32;

    fn nsimage_to_rgba(image: &NSImage, size: usize) -> Option<Vec<u8>> {
        unsafe {
            let ns_size = NSSize::new(size as f64, size as f64);
            image.setSize(ns_size);

            // Create a fresh RGBA bitmap rep at the exact pixel size we want
            let rep = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                std::ptr::null_mut(),
                size as isize,
                size as isize,
                8,
                4,
                true,
                false,
                &NSString::from_str("NSDeviceRGBColorSpace"),
                0,
                0,
            )?;

            // Draw the NSImage into our bitmap rep via NSGraphicsContext
            let gctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
            NSGraphicsContext::saveGraphicsState_class();
            NSGraphicsContext::setCurrentContext(Some(&gctx));

            let draw_rect = NSRect::new(NSPoint::new(0.0, 0.0), ns_size);
            image.drawInRect_fromRect_operation_fraction(
                draw_rect,
                NSRect::ZERO,
                NSCompositingOperation::SourceOver,
                1.0,
            );

            NSGraphicsContext::restoreGraphicsState_class();

            // Read pixel data
            let width = rep.pixelsWide() as usize;
            let height = rep.pixelsHigh() as usize;
            let bytes_per_row = rep.bytesPerRow() as usize;
            let samples = rep.samplesPerPixel() as usize;
            let data_ptr = rep.bitmapData();

            if data_ptr.is_null() {
                return None;
            }

            let mut pixels = Vec::with_capacity(width * height * 4);
            // NSImage coordinate system is flipped (origin bottom-left),
            // so read rows from bottom to top for correct orientation
            for y in (0..height).rev() {
                for x in 0..width {
                    let offset = y * bytes_per_row + x * samples;
                    let r = *data_ptr.add(offset);
                    let g = *data_ptr.add(offset + 1);
                    let b = *data_ptr.add(offset + 2);
                    let a = if samples >= 4 {
                        *data_ptr.add(offset + 3)
                    } else {
                        255
                    };
                    pixels.extend_from_slice(&[r, g, b, a]);
                }
            }

            Some(pixels)
        }
    }

    pub fn load_system_icons(ctx: &egui::Context) -> Option<IconCache> {
        let workspace = NSWorkspace::sharedWorkspace();

        let folder_uti = NSString::from_str("public.folder");
        #[allow(deprecated)]
        let folder_icon = workspace.iconForFileType(&folder_uti);
        let folder_pixels = nsimage_to_rgba(&folder_icon, ICON_PX)?;

        let file_uti = NSString::from_str("public.data");
        #[allow(deprecated)]
        let file_icon = workspace.iconForFileType(&file_uti);
        let file_pixels = nsimage_to_rgba(&file_icon, ICON_PX)?;

        let folder_image =
            egui::ColorImage::from_rgba_unmultiplied([ICON_PX, ICON_PX], &folder_pixels);
        let file_image =
            egui::ColorImage::from_rgba_unmultiplied([ICON_PX, ICON_PX], &file_pixels);

        let folder_tex =
            ctx.load_texture("sys_folder_icon", folder_image, egui::TextureOptions::LINEAR);
        let file_tex =
            ctx.load_texture("sys_file_icon", file_image, egui::TextureOptions::LINEAR);

        Some(IconCache {
            folder: folder_tex,
            file: file_tex,
        })
    }
}
