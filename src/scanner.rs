use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use walkdir::WalkDir;

use crate::tree::FileNode;

pub struct ScanProgress {
    pub file_count: AtomicU64,
    pub total_size: AtomicU64,
}

pub fn scan_directory(root: &Path, progress: Arc<ScanProgress>) -> FileNode {
    let mut sizes: HashMap<PathBuf, u64> = HashMap::new();
    let mut dirs: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();

    // First pass: collect all entries and file sizes
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path().to_path_buf();
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        if metadata.is_file() {
            let len = metadata.len();
            sizes.insert(path.clone(), len);
            progress.file_count.fetch_add(1, Ordering::Relaxed);
            progress.total_size.fetch_add(len, Ordering::Relaxed);
        } else if metadata.is_dir() {
            dirs.entry(path.clone()).or_default();
        }

        // Register this entry under its parent
        if let Some(parent) = path.parent() {
            if parent.starts_with(root) || parent == root {
                dirs.entry(parent.to_path_buf()).or_default().push(path);
            }
        }
    }

    // Build tree recursively
    fn build_node(
        path: &Path,
        sizes: &HashMap<PathBuf, u64>,
        dirs: &HashMap<PathBuf, Vec<PathBuf>>,
    ) -> FileNode {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());

        if let Some(children_paths) = dirs.get(path) {
            // It's a directory
            let mut children: Vec<FileNode> = children_paths
                .iter()
                .filter(|p| p.as_path() != path) // skip self-reference
                .map(|p| build_node(p, sizes, dirs))
                .collect();

            children.sort_by(|a, b| b.size.cmp(&a.size));

            let size = children.iter().map(|c| c.size).sum();

            FileNode {
                name,
                path: path.to_path_buf(),
                size,
                is_dir: true,
                children,
                expanded: false,
                selected: false,
            }
        } else {
            // It's a file
            let size = sizes.get(path).copied().unwrap_or(0);
            FileNode {
                name,
                path: path.to_path_buf(),
                size,
                is_dir: false,
                children: Vec::new(),
                expanded: false,
                selected: false,
            }
        }
    }

    let mut root_node = build_node(root, &sizes, &dirs);
    root_node.expanded = true; // expand root by default
    root_node
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn new_progress() -> Arc<ScanProgress> {
        Arc::new(ScanProgress {
            file_count: AtomicU64::new(0),
            total_size: AtomicU64::new(0),
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
