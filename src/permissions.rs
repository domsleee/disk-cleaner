//! Full Disk Access (FDA) detection and helpers for macOS.
//!
//! FDA is required to scan protected directories like `~/Library/Mail`,
//! `~/Library/Safari`, and Time Machine snapshots. Without it the scanner
//! silently gets permission-denied errors and reports incomplete sizes.

/// Check whether this process has Full Disk Access on macOS.
///
/// Detection strategy: probe the TCC database (only readable with FDA),
/// then fall back to well-known TCC-protected user paths.
///
/// On non-macOS platforms this always returns `true`.
#[cfg(target_os = "macos")]
pub fn has_full_disk_access() -> bool {
    use std::fs;
    use std::path::Path;

    // Primary probe: the TCC database is only readable with FDA.
    let tcc_db = Path::new("/Library/Application Support/com.apple.TCC/TCC.db");
    if fs::metadata(tcc_db).is_ok() {
        return true;
    }

    // Fallback probes: TCC-protected user files that are very likely to exist.
    if let Some(home) = dirs::home_dir() {
        let probes = [
            home.join("Library/Safari/Bookmarks.plist"),
            home.join("Library/Cookies"),
            home.join("Library/Mail"),
        ];
        for path in &probes {
            if path.exists() && fs::metadata(path).is_ok() {
                return true;
            }
        }
    }

    let time_machine = std::path::Path::new("/Library/Preferences/com.apple.TimeMachine.plist");
    if fs::metadata(time_machine).is_ok() {
        return true;
    }

    false
}

#[cfg(not(target_os = "macos"))]
pub fn has_full_disk_access() -> bool {
    true
}

/// Open the macOS System Settings pane for Full Disk Access.
///
/// On non-macOS this is a no-op.
#[cfg(target_os = "macos")]
pub fn open_fda_settings() {
    // macOS 13+ (Ventura) uses the new System Settings URL scheme.
    // Falls back gracefully on older versions.
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        .spawn();
}

#[cfg(not(target_os = "macos"))]
pub fn open_fda_settings() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_full_disk_access_returns_bool() {
        // Smoke test: just ensure it doesn't panic.
        let _result = has_full_disk_access();
    }

    #[test]
    fn open_fda_settings_does_not_panic() {
        // Don't actually open settings in CI, but verify it compiles.
        // On non-macOS this is a no-op.
        #[cfg(not(target_os = "macos"))]
        open_fda_settings();
    }
}
