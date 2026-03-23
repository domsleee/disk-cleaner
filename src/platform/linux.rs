use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::VolumeInfo;
use crate::icons::IconCache;

/// Get total and available bytes for the filesystem containing `path` using statvfs.
/// Same POSIX API as macOS.
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

/// Filesystem types that represent virtual/pseudo filesystems and should be
/// excluded from the volume list.
const VIRTUAL_FS_TYPES: &[&str] = &[
    "proc",
    "sysfs",
    "tmpfs",
    "devpts",
    "devtmpfs",
    "cgroup",
    "cgroup2",
    "pstore",
    "securityfs",
    "debugfs",
    "configfs",
    "fusectl",
    "mqueue",
    "hugetlbfs",
    "autofs",
    "tracefs",
    "binfmt_misc",
    "rpc_pipefs",
    "nfsd",
    "efivarfs",
    "bpf",
    "overlay",
    "squashfs",
    "nsfs",
    "ramfs",
];

/// Parse /proc/mounts content into a list of volumes, filtering virtual filesystems.
pub(crate) fn parse_mounts(content: &str) -> Vec<VolumeInfo> {
    let mut volumes = Vec::new();

    for line in content.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }

        let _device = fields[0];
        let mount_point = fields[1];
        let fs_type = fields[2];

        // Skip virtual filesystems
        if VIRTUAL_FS_TYPES
            .iter()
            .any(|vfs| fs_type.eq_ignore_ascii_case(vfs))
        {
            continue;
        }

        let path = PathBuf::from(mount_point);

        if let Some((total, available)) = disk_space(&path) {
            // Skip zero-size filesystems (usually virtual)
            if total == 0 {
                continue;
            }

            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| {
                    if mount_point == "/" {
                        "Root".to_string()
                    } else {
                        mount_point.to_string()
                    }
                });

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

    volumes
}

/// List mounted volumes by parsing `/proc/mounts`.
/// Filters out virtual filesystems using a fstype denylist.
pub fn list_volumes() -> Vec<VolumeInfo> {
    let content = match std::fs::read_to_string("/proc/mounts") {
        Ok(c) => c,
        Err(_) => {
            // Fallback: try /etc/mtab
            match std::fs::read_to_string("/etc/mtab") {
                Ok(c) => c,
                Err(_) => return Vec::new(),
            }
        }
    };

    parse_mounts(&content)
}

/// Build a set of paths to skip during scanning on Linux.
/// Skips virtual filesystem mount points when scanning from root.
pub fn build_skip_set(root: &Path) -> HashSet<PathBuf> {
    let mut skip = HashSet::new();

    // When scanning from root, skip well-known virtual/system directories.
    // Insert unconditionally — non-existent paths in the skip set cost nothing.
    if root == Path::new("/") {
        for dir in &["/proc", "/sys", "/dev", "/run", "/snap"] {
            skip.insert(PathBuf::from(dir));
        }
    }

    skip
}

/// Icon loading is not supported on Linux for v1.
/// Returns None for graceful emoji fallback in the UI.
pub fn load_icons(_ctx: &eframe::egui::Context) -> Option<IconCache> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mounts_filters_virtual_filesystems() {
        let input = "\
proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0
sysfs /sys sysfs rw,nosuid,nodev,noexec,relatime 0 0
/dev/sda1 / ext4 rw,relatime 0 0
tmpfs /tmp tmpfs rw,nosuid,nodev 0 0
/dev/sda2 /home ext4 rw,relatime 0 0
devpts /dev/pts devpts rw,nosuid,noexec,relatime 0 0";

        let vols = parse_mounts(input);
        // Only real filesystems should appear (/ and /home)
        // Note: disk_space may return None for paths that don't exist on the test machine,
        // so we check that no virtual fs types leaked through
        for vol in &vols {
            assert_ne!(vol.path, PathBuf::from("/proc"));
            assert_ne!(vol.path, PathBuf::from("/sys"));
            assert_ne!(vol.path, PathBuf::from("/tmp"));
            assert_ne!(vol.path, PathBuf::from("/dev/pts"));
        }
    }

    #[test]
    fn parse_mounts_handles_empty_input() {
        let vols = parse_mounts("");
        assert!(vols.is_empty());
    }

    #[test]
    fn skip_set_from_root_includes_virtual_dirs() {
        let skip = build_skip_set(Path::new("/"));
        // /proc and /sys should always exist on Linux
        assert!(skip.contains(&PathBuf::from("/proc")));
        assert!(skip.contains(&PathBuf::from("/sys")));
        assert!(skip.contains(&PathBuf::from("/dev")));
    }

    #[test]
    fn skip_set_from_non_root_is_empty() {
        let skip = build_skip_set(Path::new("/home/user"));
        assert!(skip.is_empty());
    }
}
