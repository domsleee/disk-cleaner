use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use rayon::iter::ParallelBridge;
use rayon::prelude::ParallelIterator;

use crate::tree::{DirNode, FileLeaf, FileNode};

// Re-export platform types and functions so existing callers (main.rs, bins)
// continue to work via `scanner::list_volumes()`, `scanner::disk_space()`, etc.
pub use crate::platform::{disk_space, list_volumes, VolumeInfo};

pub struct ScanProgress {
    pub file_count: AtomicU64,
    pub total_size: AtomicU64,
    pub cancelled: AtomicBool,
}

pub fn scan_directory(root: &Path, progress: Arc<ScanProgress>) -> FileNode {
    let skip = Arc::new(crate::platform::build_skip_set(root));
    // Root node gets the full absolute path as its name so that
    // path reconstruction (root.name / child.name / ...) produces
    // correct absolute paths.
    let mut root_node = walk_dir(root, &progress, &skip);
    root_node.set_expanded(true);
    // Override name to be the full path (walk_dir used file_name only)
    if let FileNode::Dir(d) = &mut root_node {
        d.name = root.to_string_lossy().into_owned().into_boxed_str();
    }
    root_node
}

/// Parallel recursive directory walk, following dust's par_bridge() pattern.
fn walk_dir(dir: &Path, progress: &Arc<ScanProgress>, skip: &Arc<HashSet<PathBuf>>) -> FileNode {
    let dir_name: Box<str> = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.to_string_lossy().into_owned())
        .into_boxed_str();

    let empty_dir = FileNode::Dir(DirNode {
        name: dir_name.clone(),
        size: 0,
        children: Vec::new(),
        expanded: false,
    });

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
                if skip.contains(&path) {
                    return None;
                }
                // On Windows, skip NTFS reparse points (junctions, symlinks)
                // to avoid double-counting or infinite loops — same class of
                // issue as APFS firmlink dedup on macOS.
                #[cfg(target_os = "windows")]
                {
                    use std::os::windows::fs::MetadataExt;
                    if let Ok(meta) = entry.metadata() {
                        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                            return None;
                        }
                    }
                }
                Some(walk_dir(&path, progress, skip))
            } else if ft.is_file() {
                let metadata = entry.metadata().ok()?;
                let len = metadata.len();
                progress.file_count.fetch_add(1, Ordering::Relaxed);
                progress.total_size.fetch_add(len, Ordering::Relaxed);
                let name = entry
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
                    .into_boxed_str();
                Some(FileNode::File(FileLeaf { name, size: len }))
            } else {
                None
            }
        })
        .collect();

    children.sort_by_key(|b| std::cmp::Reverse(b.size()));
    let size = children.iter().map(|c| c.size()).sum();

    FileNode::Dir(DirNode {
        name: dir_name,
        size,
        children,
        expanded: false,
    })
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

        assert!(root.is_dir());
        assert!(root.children().is_empty());
        assert_eq!(root.size(), 0);
        assert!(root.expanded());
        assert_eq!(progress.file_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn scan_flat_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.txt"), "hello").unwrap(); // 5 bytes
        fs::write(tmp.path().join("b.txt"), "hi").unwrap(); // 2 bytes

        let progress = new_progress();
        let root = scan_directory(tmp.path(), progress.clone());

        assert_eq!(root.children().len(), 2);
        assert_eq!(progress.file_count.load(Ordering::Relaxed), 2);
        assert_eq!(progress.total_size.load(Ordering::Relaxed), 7);
        assert_eq!(root.size(), 7);
        // Children sorted by size descending
        assert_eq!(root.children()[0].size(), 5);
        assert_eq!(root.children()[1].size(), 2);
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

        assert_eq!(root.children().len(), 2);
        assert_eq!(progress.file_count.load(Ordering::Relaxed), 2);
        assert_eq!(root.size(), 101);

        // sub dir (100 bytes) should sort before root.txt (1 byte)
        let sub_node = &root.children()[0];
        assert!(sub_node.is_dir());
        assert_eq!(sub_node.name(), "sub");
        assert_eq!(sub_node.size(), 100);
        assert_eq!(sub_node.children().len(), 1);
    }

    #[test]
    fn root_is_expanded_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        let progress = new_progress();
        let root = scan_directory(tmp.path(), progress);
        assert!(root.expanded());
    }

    #[test]
    fn children_not_expanded_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("child")).unwrap();
        let progress = new_progress();
        let root = scan_directory(tmp.path(), progress);
        assert!(!root.children()[0].expanded());
    }

    #[test]
    fn cancelled_scan_returns_empty_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file.bin"), vec![0u8; 100]).unwrap();
        fs::write(tmp.path().join("root.txt"), "data").unwrap();

        let progress = new_progress();
        // Cancel before scanning starts
        progress.cancelled.store(true, Ordering::Relaxed);
        let root = scan_directory(tmp.path(), progress.clone());

        // Cancelled scan should produce an empty root with no children
        assert!(root.children().is_empty());
        assert_eq!(root.size(), 0);
        assert_eq!(progress.file_count.load(Ordering::Relaxed), 0);
    }
}
