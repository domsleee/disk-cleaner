use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::VolumeInfo;
use crate::icons::IconCache;

/// Get total and available bytes for the filesystem containing `path` using statvfs.
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;

    let c_path = CString::new(path.to_str()?).ok()?;
    let mut stat = MaybeUninit::<libc::statvfs>::uninit();
    let result = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if result != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    let block_size = stat.f_frsize;
    let total = stat.f_blocks as u64 * block_size;
    let available = stat.f_bavail as u64 * block_size;
    Some((total, available))
}

/// List mounted volumes on macOS. Reads `/Volumes/` and includes root `/`.
pub fn list_volumes() -> Vec<VolumeInfo> {
    let mut volumes = Vec::new();

    // Root filesystem
    let root = Path::new("/");
    if let Some((total, available)) = disk_space(root) {
        volumes.push(VolumeInfo {
            name: "Macintosh HD".to_string(),
            canonical_path: root.canonicalize().ok(),
            path: PathBuf::from("/"),
            total_bytes: total,
            available_bytes: available,
        });
    }

    // /Volumes entries (excludes self-referencing "Macintosh HD" symlink if present)
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        for entry in entries.flatten() {
            let path = entry.path();

            // Skip if it resolves to root (covers both symlinks and firmlinks)
            if let Ok(canonical) = std::fs::canonicalize(&path) {
                if canonical == Path::new("/") {
                    continue;
                }
            }

            if path.is_dir() {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());

                if let Some((total, available)) = disk_space(&path) {
                    let canonical_path = path.canonicalize().ok();
                    volumes.push(VolumeInfo {
                        name,
                        canonical_path,
                        path,
                        total_bytes: total,
                        available_bytes: available,
                    });
                }
            }
        }
    }

    volumes
}

/// Build a set of paths to skip during scanning to avoid double-counting.
/// On macOS APFS, `/System/Volumes/Data` contains the real user data but is also
/// accessible via firmlinks (e.g. `/Users` → `/System/Volumes/Data/Users`).
/// Scanning from `/` without skipping Data counts everything twice.
/// Mount points under `/Volumes/` also cause inflation when scanning root.
pub fn build_skip_set(root: &Path) -> HashSet<PathBuf> {
    let mut skip = HashSet::new();

    let data_vol = Path::new("/System/Volumes/Data");
    // Only apply APFS dedup when scanning from a path that isn't under the Data volume
    if !root.starts_with(data_vol) {
        // Skip the entire Data volume — all user-visible content is
        // accessible via firmlinks from the root, so descending into
        // /System/Volumes/Data would double-count everything.
        skip.insert(data_vol.to_path_buf());

        // Also skip other APFS sub-volume mounts that inflate size
        // Insert unconditionally — non-existent paths in the skip set
        // cost nothing (no directory entry will ever match them).
        for sub in &[
            "Preboot",
            "Recovery",
            "VM",
            "Update",
            "BaseSystem",
            "FieldService",
            "FieldServiceDiagnostic",
            "FieldServiceRepair",
            "iSCPreboot",
            "xarts",
            "Hardware",
        ] {
            let p = Path::new("/System/Volumes").join(sub);
            if !root.starts_with(&p) {
                skip.insert(p);
            }
        }
    }

    // Skip mount points under /Volumes/ to avoid counting other drives.
    if !root.starts_with("/Volumes/") && root != Path::new("/Volumes") {
        if let Ok(entries) = std::fs::read_dir("/Volumes") {
            for entry in entries.flatten() {
                let path = entry.path();
                if path != root {
                    skip.insert(path);
                }
            }
        }
    }

    skip
}

/// Load macOS system icons via NSWorkspace and return as egui textures.
pub fn load_icons(ctx: &eframe::egui::Context) -> Option<IconCache> {
    use eframe::egui;
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

            let width = rep.pixelsWide() as usize;
            let height = rep.pixelsHigh() as usize;
            let bytes_per_row = rep.bytesPerRow() as usize;
            let samples = rep.samplesPerPixel() as usize;
            let data_ptr = rep.bitmapData();

            if data_ptr.is_null() {
                return None;
            }

            let mut pixels = Vec::with_capacity(width * height * 4);
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
    let file_image = egui::ColorImage::from_rgba_unmultiplied([ICON_PX, ICON_PX], &file_pixels);

    let folder_tex = ctx.load_texture(
        "sys_folder_icon",
        folder_image,
        egui::TextureOptions::LINEAR,
    );
    let file_tex = ctx.load_texture("sys_file_icon", file_image, egui::TextureOptions::LINEAR);

    Some(IconCache {
        folder: folder_tex,
        file: file_tex,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_volumes_includes_root() {
        let vols = list_volumes();
        assert!(
            vols.iter().any(|v| v.path == PathBuf::from("/")),
            "list_volumes should include root filesystem"
        );
    }

    #[test]
    fn disk_space_returns_some_for_root() {
        let result = disk_space(Path::new("/"));
        assert!(result.is_some(), "disk_space should succeed for /");
        let (total, available) = result.unwrap();
        assert!(total > 0);
        assert!(available <= total);
    }

    #[test]
    fn build_skip_set_from_root_includes_data_volume() {
        let skip = build_skip_set(Path::new("/"));
        assert!(
            skip.contains(&PathBuf::from("/System/Volumes/Data")),
            "skip set from root should include /System/Volumes/Data"
        );
    }

    #[test]
    fn build_skip_set_under_data_volume_is_empty() {
        // When scanning from under /System/Volumes/Data, we should NOT skip Data itself
        let skip = build_skip_set(Path::new("/System/Volumes/Data/Users"));
        assert!(
            !skip.contains(&PathBuf::from("/System/Volumes/Data")),
            "skip set from under Data volume should not include Data"
        );
    }
}
