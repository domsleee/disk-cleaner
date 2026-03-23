use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::VolumeInfo;
use crate::icons::IconCache;

/// Get total and available bytes for the filesystem containing `path`
/// using `GetDiskFreeSpaceExW`.
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();

    let mut free_bytes_available: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut _total_free_bytes: u64 = 0;

    let result = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_bytes_available,
            &mut total_bytes,
            &mut _total_free_bytes,
        )
    };

    if result == 0 {
        return None;
    }

    Some((total_bytes, free_bytes_available))
}

/// List mounted volumes by enumerating drive letters A-Z using `GetLogicalDrives`.
pub fn list_volumes() -> Vec<VolumeInfo> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        GetLogicalDrives, GetVolumeInformationW,
    };

    let mut volumes = Vec::new();
    let drive_bits = unsafe { GetLogicalDrives() };

    if drive_bits == 0 {
        return volumes;
    }

    for i in 0u32..26 {
        if drive_bits & (1 << i) == 0 {
            continue;
        }

        let letter = (b'A' + i as u8) as char;
        let root_path = format!("{}:\\", letter);
        let path = PathBuf::from(&root_path);

        // Get disk space — skip drives where this fails (e.g. empty DVD drive)
        let (total, available) = match disk_space(&path) {
            Some(space) => space,
            None => continue,
        };

        // Skip zero-size drives
        if total == 0 {
            continue;
        }

        // Try to get the volume label
        let root_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut label_buf = [0u16; 261];

        let label_ok = unsafe {
            GetVolumeInformationW(
                root_wide.as_ptr(),
                label_buf.as_mut_ptr(),
                label_buf.len() as u32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };

        let label = if label_ok != 0 {
            let len = label_buf.iter().position(|&c| c == 0).unwrap_or(0);
            let label_str = String::from_utf16_lossy(&label_buf[..len]);
            if label_str.is_empty() {
                format!("Local Disk ({}:)", letter)
            } else {
                format!("{} ({}:)", label_str, letter)
            }
        } else {
            format!("Local Disk ({}:)", letter)
        };

        let canonical_path = path.canonicalize().ok();
        volumes.push(VolumeInfo {
            name: label,
            canonical_path,
            path,
            total_bytes: total,
            available_bytes: available,
        });
    }

    volumes
}

/// Build a set of paths to skip during scanning on Windows.
/// Skips system directories that should not be deleted or that cause
/// double-counting via junction points.
pub fn build_skip_set(root: &Path) -> HashSet<PathBuf> {
    let mut skip = HashSet::new();

    // When scanning a drive root, skip well-known system directories
    // that are protected or cause traversal issues.
    let root_str = root.to_string_lossy();
    let is_drive_root =
        root_str.len() <= 3 && (root_str.ends_with('\\') || root_str.ends_with(':'));

    if is_drive_root {
        skip.insert(root.join("$Recycle.Bin"));
        skip.insert(root.join("System Volume Information"));
    }

    skip
}

/// Icon loading is not supported on Windows for v1.
/// Returns None for graceful emoji fallback in the UI.
pub fn load_icons(_ctx: &eframe::egui::Context) -> Option<IconCache> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_set_from_drive_root_includes_system_dirs() {
        let skip = build_skip_set(Path::new("C:\\"));
        assert!(skip.contains(&PathBuf::from("C:\\$Recycle.Bin")));
        assert!(skip.contains(&PathBuf::from("C:\\System Volume Information")));
    }

    #[test]
    fn skip_set_from_subdirectory_is_empty() {
        let skip = build_skip_set(Path::new("C:\\Users\\dom\\Documents"));
        assert!(skip.is_empty());
    }

    #[test]
    fn list_volumes_returns_at_least_one_drive() {
        let vols = list_volumes();
        assert!(
            !vols.is_empty(),
            "Windows should have at least one drive"
        );
        // C: drive should typically be present
        assert!(
            vols.iter().any(|v| v.path == PathBuf::from("C:\\")),
            "C:\\ drive should be present"
        );
    }
}
