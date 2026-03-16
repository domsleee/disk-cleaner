use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use rayon::iter::ParallelBridge;
use rayon::prelude::ParallelIterator;

use crate::tree::FileNode;

/// Information about a mounted volume.
pub struct VolumeInfo {
    pub name: String,
    pub path: PathBuf,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// Get total and available bytes for the filesystem containing `path`.
#[cfg(unix)]
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

#[cfg(not(unix))]
pub fn disk_space(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// List mounted volumes. On macOS, reads `/Volumes/` and includes root `/`.
pub fn list_volumes() -> Vec<VolumeInfo> {
    let mut volumes = Vec::new();

    // Root filesystem
    if let Some((total, available)) = disk_space(Path::new("/")) {
        volumes.push(VolumeInfo {
            name: "Macintosh HD".to_string(),
            path: PathBuf::from("/"),
            total_bytes: total,
            available_bytes: available,
        });
    }

    // /Volumes entries (excludes self-referencing "Macintosh HD" symlink if present)
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        for entry in entries.flatten() {
            let path = entry.path();

            // Skip the root volume alias (symlink to /)
            if let Ok(target) = std::fs::read_link(&path) {
                if target == Path::new("/") {
                    continue;
                }
            }

            // Skip if it resolves to root
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
                    volumes.push(VolumeInfo {
                        name,
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

pub struct ScanProgress {
    pub file_count: AtomicU64,
    pub total_size: AtomicU64,
    pub cancelled: AtomicBool,
}

pub fn scan_directory(root: &Path, progress: Arc<ScanProgress>) -> FileNode {
    let mut root_node = walk_dir(root, &progress);
    root_node.expanded = true;
    root_node
}

/// Parallel recursive directory walk, following dust's par_bridge() pattern.
fn walk_dir(dir: &Path, progress: &Arc<ScanProgress>) -> FileNode {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| dir.to_string_lossy().to_string());

    let empty_dir = FileNode {
        name: name.clone(),
        path: dir.to_path_buf(),
        size: 0,
        is_dir: true,
        children: Vec::new(),
        expanded: false,
        selected: false,
    };

    // Bail out early if scan was cancelled
    if progress.cancelled.load(Ordering::Relaxed) {
        return empty_dir;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return empty_dir,
    };

    let mut children: Vec<FileNode> = entries
        .into_iter()
        .par_bridge()
        .filter_map(|entry| {
            if progress.cancelled.load(Ordering::Relaxed) {
                return None;
            }
            let entry = entry.ok()?;
            let ft = entry.file_type().ok()?;
            let path = entry.path();

            if ft.is_dir() {
                Some(walk_dir(&path, progress))
            } else if ft.is_file() {
                let metadata = entry.metadata().ok()?;
                let len = metadata.len();
                progress.file_count.fetch_add(1, Ordering::Relaxed);
                progress.total_size.fetch_add(len, Ordering::Relaxed);
                let fname = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                Some(FileNode {
                    name: fname,
                    path,
                    size: len,
                    is_dir: false,
                    children: Vec::new(),
                    expanded: false,
                    selected: false,
                })
            } else {
                None
            }
        })
        .collect();

    children.sort_by(|a, b| b.size.cmp(&a.size));
    let size = children.iter().map(|c| c.size).sum();

    FileNode {
        name,
        path: dir.to_path_buf(),
        size,
        is_dir: true,
        children,
        expanded: false,
        selected: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn new_progress() -> Arc<ScanProgress> {
        Arc::new(ScanProgress {
            file_count: AtomicU64::new(0),
            total_size: AtomicU64::new(0),
            cancelled: AtomicBool::new(false),
        })
    }

    #[test]
    fn scan_empty_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let progress = new_progress();
        let root = scan_directory(tmp.path(), progress.clone());

        assert!(root.is_dir);
        assert!(root.children.is_empty());
        assert_eq!(root.size, 0);
        assert!(root.expanded);
        assert_eq!(progress.file_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn scan_flat_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.txt"), "hello").unwrap(); // 5 bytes
        fs::write(tmp.path().join("b.txt"), "hi").unwrap(); // 2 bytes

        let progress = new_progress();
        let root = scan_directory(tmp.path(), progress.clone());

        assert_eq!(root.children.len(), 2);
        assert_eq!(progress.file_count.load(Ordering::Relaxed), 2);
        assert_eq!(progress.total_size.load(Ordering::Relaxed), 7);
        assert_eq!(root.size, 7);
        // Children sorted by size descending
        assert_eq!(root.children[0].size, 5);
        assert_eq!(root.children[1].size, 2);
    }

    #[test]
    fn scan_nested_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file.bin"), vec![0u8; 100]).unwrap();
        fs::write(tmp.path().join("root.txt"), "r").unwrap();

        let progress = new_progress();
        let root = scan_directory(tmp.path(), progress.clone());

        assert_eq!(root.children.len(), 2);
        assert_eq!(progress.file_count.load(Ordering::Relaxed), 2);
        assert_eq!(root.size, 101);

        // sub dir (100 bytes) should sort before root.txt (1 byte)
        let sub_node = &root.children[0];
        assert!(sub_node.is_dir);
        assert_eq!(sub_node.name, "sub");
        assert_eq!(sub_node.size, 100);
        assert_eq!(sub_node.children.len(), 1);
    }

    #[test]
    fn root_is_expanded_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        let progress = new_progress();
        let root = scan_directory(tmp.path(), progress);
        assert!(root.expanded);
    }

    #[test]
    fn children_not_expanded_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("child")).unwrap();
        let progress = new_progress();
        let root = scan_directory(tmp.path(), progress);
        assert!(!root.children[0].expanded);
    }
}
