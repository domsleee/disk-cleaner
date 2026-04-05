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

/// Pre-probe TCC-protected directories that fall under `scan_root` to trigger
/// macOS permission dialogs upfront, before the real scan begins.
///
/// macOS TCC prompts are lazy — they fire only when the app first attempts to
/// access a protected directory. Without this pre-flight, prompts appear
/// mid-scan, blocking the scanner thread and surprising the user.
///
/// If the app already has Full Disk Access, this is a no-op (no prompts fire).
/// On non-macOS platforms this does nothing.
#[cfg(target_os = "macos")]
pub fn preflight_tcc_probe(scan_root: &std::path::Path) {
    use std::fs;

    // If we already have FDA, all folders are accessible — skip probing.
    if has_full_disk_access() {
        return;
    }

    let Some(home) = dirs::home_dir() else {
        return;
    };

    // TCC-protected directories under the user's home.
    let protected = [
        home.join("Desktop"),
        home.join("Documents"),
        home.join("Downloads"),
        home.join("Library/Mail"),
        home.join("Library/Messages"),
        home.join("Library/Safari"),
    ];

    for dir in &protected {
        // Only probe if this directory falls under the scan root.
        if dir.starts_with(scan_root) || scan_root.starts_with(dir) {
            // Attempt read_dir to trigger the TCC prompt. We don't care
            // about the result — the side effect (the macOS dialog) is
            // what matters.
            let _ = fs::read_dir(dir);
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn preflight_tcc_probe(_scan_root: &std::path::Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_full_disk_access_returns_bool() {
        // Smoke test: just ensure it doesn't panic.
        let _result = has_full_disk_access();
    }
}
