/// Check whether this process has Full Disk Access (FDA) on macOS.
///
/// FDA is required to scan protected directories like `~/Library/Mail`,
/// `~/Library/Safari`, and Time Machine snapshots. Without it the scanner
/// silently gets permission-denied errors and reports incomplete sizes.
///
/// Detection strategy: attempt to read metadata of the TCC database, which
/// is only accessible with FDA. Falls back to probing well-known
/// TCC-protected paths.
///
/// On non-macOS platforms this always returns `true` (no FDA concept).
#[cfg(target_os = "macos")]
pub fn has_full_disk_access() -> bool {
    use std::fs;
    use std::path::Path;

    // Primary probe: the TCC database is only readable with FDA.
    let tcc_db = Path::new("/Library/Application Support/com.apple.TCC/TCC.db");
    if fs::metadata(tcc_db).is_ok() {
        return true;
    }

    // Fallback probes: TCC-protected user files.
    if let Some(home) = dirs::home_dir() {
        let safari_bookmarks = home.join("Library/Safari/Bookmarks.plist");
        if fs::metadata(&safari_bookmarks).is_ok() {
            return true;
        }
    }

    let time_machine_plist = Path::new("/Library/Preferences/com.apple.TimeMachine.plist");
    if fs::metadata(time_machine_plist).is_ok() {
        return true;
    }

    false
}

#[cfg(not(target_os = "macos"))]
pub fn has_full_disk_access() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_full_disk_access_returns_bool() {
        // Smoke test: just ensure it doesn't panic.
        let _result = has_full_disk_access();
    }
}
