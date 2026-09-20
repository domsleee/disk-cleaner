//! Drive discovery and capacity queries for the Windows volume picker.

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;

use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    GetThreadErrorMode, SEM_FAILCRITICALERRORS, SetThreadErrorMode,
};
use windows_sys::Win32::System::WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE};

use super::VolumeInfo;

// Empty card readers should return an error instead of opening an insert-media
// dialog. Restore the caller's thread settings when the query finishes.
struct SilentErrors(u32);

impl SilentErrors {
    fn new() -> Option<Self> {
        let mut previous = 0;
        let ok = unsafe {
            SetThreadErrorMode(GetThreadErrorMode() | SEM_FAILCRITICALERRORS, &mut previous)
        };
        (ok != 0).then_some(Self(previous))
    }
}

impl Drop for SilentErrors {
    fn drop(&mut self) {
        unsafe { SetThreadErrorMode(self.0, ptr::null_mut()) };
    }
}

/// Get total and caller-available bytes for the directory's filesystem.
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.is_empty() || wide.contains(&0) {
        return None;
    }
    // A trailing separator is required for UNC share roots.
    if !matches!(wide.last(), Some(47 | 92)) {
        wide.push(b'\\' as u16);
    }
    wide.push(0);
    let _errors = SilentErrors::new()?;
    let mut total = 0;
    let mut available = 0;
    let ok =
        unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, ptr::null_mut()) };
    (ok != 0).then_some((total, available))
}

/// List ready local disks, including removable USB drives, in drive-letter order.
pub fn list_volumes() -> Vec<VolumeInfo> {
    let Some(_errors) = SilentErrors::new() else {
        return Vec::new();
    };
    let drives = unsafe { GetLogicalDrives() };
    let mut volumes = Vec::new();
    for index in 0..26 {
        if drives & (1 << index) == 0 {
            continue;
        }
        let letter = b'A' + index as u8;
        let root = [letter as u16, b':' as u16, b'\\' as u16, 0];
        let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
        // Match the local-disk picker on macOS; avoid querying disconnected
        // network mappings or optical drives during the UI's periodic refresh.
        if !matches!(kind, DRIVE_FIXED | DRIVE_REMOVABLE) {
            continue;
        }
        let path = PathBuf::from(format!("{}:\\", letter as char));
        let Some((total_bytes, available_bytes)) = disk_space(&path) else {
            continue;
        };
        let mut label = [0u16; 261];
        let ok = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                label.as_mut_ptr(),
                label.len() as u32,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                0,
            )
        };
        let len = label.iter().position(|&c| c == 0).unwrap_or(label.len());
        let name = if ok != 0 && len > 0 {
            format!(
                "{} ({}:)",
                String::from_utf16_lossy(&label[..len]),
                letter as char
            )
        } else {
            format!("{}:", letter as char)
        };
        volumes.push(VolumeInfo {
            name,
            path,
            total_bytes,
            available_bytes,
        });
    }
    volumes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_space_handles_unicode_and_missing_directories() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("資料-é");
        std::fs::create_dir(&directory).unwrap();
        let (total, available) = disk_space(&directory).expect("directory capacity");
        assert!(total > 0);
        assert!(available <= total);
        assert_eq!(disk_space(&temp.path().join("missing")), None);
        assert_eq!(disk_space(Path::new("")), None);
        assert_eq!(disk_space(Path::new("C:\\\0ignored")), None);
    }

    #[test]
    fn picker_contains_system_drive_with_capacity() {
        let system = std::env::var("SystemDrive").expect("Windows system drive");
        let root = PathBuf::from(format!("{system}\\"));
        let volumes = list_volumes();
        let volume = volumes
            .iter()
            .find(|v| v.path == root)
            .expect("system drive card");
        assert!(volume.name.contains(&system));
        assert!(volume.total_bytes > 0);
        assert!(volume.available_bytes <= volume.total_bytes);
        assert_eq!(volume.total_bytes, disk_space(&root).unwrap().0);
        let paths: std::collections::HashSet<_> = volumes.iter().map(|v| &v.path).collect();
        assert_eq!(paths.len(), volumes.len());
    }

    #[test]
    fn queries_restore_thread_error_mode() {
        let before = unsafe { GetThreadErrorMode() };
        let _ = list_volumes();
        assert_eq!(unsafe { GetThreadErrorMode() }, before);
        let _ = disk_space(&std::env::temp_dir());
        assert_eq!(unsafe { GetThreadErrorMode() }, before);
    }
}
