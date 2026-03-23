//! Platform-specific implementations for disk space, volume listing,
//! skip-set construction, and icon loading.
//!
//! Each platform module exports the same four functions:
//!   - `disk_space(path) -> Option<(u64, u64)>`
//!   - `list_volumes() -> Vec<VolumeInfo>`
//!   - `build_skip_set(root) -> HashSet<PathBuf>`
//!   - `load_icons(ctx) -> Option<IconCache>`

use std::path::PathBuf;

/// Information about a mounted volume.
pub struct VolumeInfo {
    pub name: String,
    pub path: PathBuf,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use self::windows::*;

// Stub fallback for unsupported platforms (FreeBSD, Android, etc.)
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod fallback {
    use super::VolumeInfo;
    use crate::icons::IconCache;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    pub fn disk_space(_path: &Path) -> Option<(u64, u64)> {
        None
    }
    pub fn list_volumes() -> Vec<VolumeInfo> {
        Vec::new()
    }
    pub fn build_skip_set(_root: &Path) -> HashSet<PathBuf> {
        HashSet::new()
    }
    pub fn load_icons(_ctx: &eframe::egui::Context) -> Option<IconCache> {
        None
    }
}
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub use fallback::*;
