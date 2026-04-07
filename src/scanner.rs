use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use rayon::iter::ParallelBridge;
use rayon::prelude::ParallelIterator;

use crate::tree::{self, FileTree, ScanDirNode, ScanFileLeaf, ScanFileNode};

/// Information about a mounted volume.
pub struct VolumeInfo {
    pub name: String,
    pub path: PathBuf,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// Lossless widening to u64 for statvfs fields whose concrete type
/// varies across platforms (e.g. `fsblkcnt_t` is u32 on macOS, u64 on Linux).
#[cfg(unix)]
#[inline(always)]
fn widen(v: impl Into<u64>) -> u64 {
    v.into()
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
    let block_size = widen(stat.f_frsize);
    let total = widen(stat.f_blocks) * block_size;
    let available = widen(stat.f_bavail) * block_size;
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

/// Build a set of paths to skip during scanning to avoid double-counting.
fn build_skip_set(root: &Path) -> Arc<HashSet<PathBuf>> {
    Arc::new(platform_skip_paths(root))
}


#[cfg(target_os = "macos")]
fn platform_skip_paths(root: &Path) -> HashSet<PathBuf> {
    let mut skip = HashSet::new();

    let data_vol = Path::new("/System/Volumes/Data");
    if !root.starts_with(data_vol) {
        skip.insert(data_vol.to_path_buf());

        for sub in &[
            "Preboot",
            "Recovery",
            "VM",
            "Update",
            "BaseSystem",
            "FieldService",
            "FieldServiceDiagnostic",
            "FieldServiceRepair",
            "iSCPreboot",
            "xarts",
            "Hardware",
        ] {
            let p = Path::new("/System/Volumes").join(sub);
            if p.exists() && !root.starts_with(&p) {
                skip.insert(p);
            }
        }
    }

    if !root.starts_with("/Volumes/") && root != Path::new("/Volumes") {
        if let Ok(entries) = std::fs::read_dir("/Volumes") {
            for entry in entries.flatten() {
                let path = entry.path();
                if path != root {
                    skip.insert(path);
                }
            }
        }
    }

    skip
}

#[cfg(not(target_os = "macos"))]
fn platform_skip_paths(_root: &Path) -> HashSet<PathBuf> {
    HashSet::new()
}

pub fn scan_directory(root: &Path, progress: Arc<ScanProgress>) -> FileTree {
    let skip = build_skip_set(root);
    let mut scan_root = walk_dir(root, &progress, &skip);
    tree::sort_scan_children(&mut scan_root);

    // Override name to be the full path
    if let ScanFileNode::Dir(d) = &mut scan_root {
        d.name = root.to_string_lossy().into_owned().into_boxed_str();
    }

    // Convert to arena
    let mut tree = tree::from_scan_tree(scan_root);
    let root_id = tree.root();
    tree.set_expanded(root_id, true);
    tree
}

/// Convert an OsString to Box<str>, reusing the OsString allocation when
/// the name is valid UTF-8 (the common case on macOS/Linux).
fn os_name_to_boxed(name: std::ffi::OsString) -> Box<str> {
    match name.into_string() {
        Ok(s) => s.into_boxed_str(),
        Err(os) => os.to_string_lossy().into_owned().into_boxed_str(),
    }
}

/// macOS UF_HIDDEN flag constant.
#[cfg(target_os = "macos")]
const UF_HIDDEN: u32 = 0x8000;

#[cfg(target_os = "macos")]
fn is_hidden_from_metadata(name: &str, metadata: &std::fs::Metadata) -> bool {
    use std::os::darwin::fs::MetadataExt;
    name.starts_with('.') || metadata.st_flags() & UF_HIDDEN != 0
}

#[cfg(not(target_os = "macos"))]
fn is_hidden_from_metadata(name: &str, _metadata: &std::fs::Metadata) -> bool {
    name.starts_with('.')
}

/// Parallel recursive directory walk using rayon par_bridge().
fn walk_dir(
    dir: &Path,
    progress: &Arc<ScanProgress>,
    skip: &Arc<HashSet<PathBuf>>,
) -> ScanFileNode {
    let dir_name: Box<str> = dir
        .file_name()
        .map(|n| os_name_to_boxed(n.to_os_string()))
        .unwrap_or_else(|| dir.to_string_lossy().into_owned().into_boxed_str());

    let dir_hidden = std::fs::symlink_metadata(dir)
        .map(|m| is_hidden_from_metadata(&dir_name, &m))
        .unwrap_or_else(|_| dir_name.starts_with('.'));

    if progress.cancelled.load(Ordering::Relaxed) {
        return ScanFileNode::Dir(Box::new(ScanDirNode {
            name: dir_name,
            size: 0,
            children: Vec::new(),
            hidden: dir_hidden,
        }));
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => {
            return ScanFileNode::Dir(Box::new(ScanDirNode {
                name: dir_name,
                size: 0,
                children: Vec::new(),
                hidden: dir_hidden,
            }));
        }
    };

    let mut children: Vec<ScanFileNode> = entries
        .into_iter()
        .par_bridge()
        .filter_map(|entry| {
            if progress.cancelled.load(Ordering::Relaxed) {
                return None;
            }
            let entry = entry.ok()?;
            let ft = entry.file_type().ok()?;

            if ft.is_dir() {
                let path = entry.path();
                if skip.contains(&path) {
                    return None;
                }
                Some(walk_dir(&path, progress, skip))
            } else if ft.is_file() {
                let metadata = entry.metadata().ok()?;
                #[cfg(unix)]
                let len = metadata.blocks() * 512;
                #[cfg(not(unix))]
                let len = metadata.len();
                progress.file_count.fetch_add(1, Ordering::Relaxed);
                progress.total_size.fetch_add(len, Ordering::Relaxed);
                let name = os_name_to_boxed(entry.file_name());
                let hidden = is_hidden_from_metadata(&name, &metadata);
                Some(ScanFileNode::File(ScanFileLeaf::new(name, len, hidden)))
            } else {
                None
            }
        })
        .collect();

    children.shrink_to_fit();
    let size = children.iter().map(|c| c.size()).sum();

    ScanFileNode::Dir(Box::new(ScanDirNode {
        name: dir_name,
        size,
        children,
        hidden: dir_hidden,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::Ordering;

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
        let tree = scan_directory(tmp.path(), progress.clone());

        let root = tree.root();
        assert!(tree.is_dir(root));
        assert!(tree.children(root).is_empty());
        assert_eq!(tree.size(root), 0);
        assert!(tree.expanded(root));
        assert_eq!(progress.file_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn scan_flat_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.txt"), "hello").unwrap();
        fs::write(tmp.path().join("b.txt"), "hi").unwrap();

        let progress = new_progress();
        let tree = scan_directory(tmp.path(), progress.clone());

        let root = tree.root();
        assert_eq!(tree.children(root).len(), 2);
        assert_eq!(progress.file_count.load(Ordering::Relaxed), 2);
        #[cfg(unix)]
        {
            let expected_per_file = fs::metadata(tmp.path().join("a.txt")).unwrap().blocks() * 512;
            assert_eq!(tree.size(root), expected_per_file * 2);
        }
        #[cfg(not(unix))]
        {
            assert_eq!(tree.size(root), 7);
        }
    }

    #[test]
    fn scan_nested_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file.bin"), vec![0u8; 100]).unwrap();
        fs::write(tmp.path().join("root.txt"), "r").unwrap();

        let progress = new_progress();
        let tree = scan_directory(tmp.path(), progress.clone());

        let root = tree.root();
        assert_eq!(tree.children(root).len(), 2);
        assert_eq!(progress.file_count.load(Ordering::Relaxed), 2);

        #[cfg(unix)]
        {
            let block_size = fs::metadata(sub.join("file.bin")).unwrap().blocks() * 512;
            assert_eq!(tree.size(root), block_size * 2);
        }
        #[cfg(not(unix))]
        {
            assert_eq!(tree.size(root), 101);
        }

        let sub_node = tree
            .children(root)
            .iter()
            .find(|&&c| tree.name(c) == "sub")
            .unwrap();
        assert!(tree.is_dir(*sub_node));
        assert_eq!(tree.children(*sub_node).len(), 1);
    }

    #[test]
    fn root_is_expanded_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        let progress = new_progress();
        let tree = scan_directory(tmp.path(), progress);
        assert!(tree.expanded(tree.root()));
    }

    #[test]
    fn children_not_expanded_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("child")).unwrap();
        let progress = new_progress();
        let tree = scan_directory(tmp.path(), progress);
        let child = tree.children(tree.root())[0];
        assert!(!tree.expanded(child));
    }

    #[test]
    #[cfg(unix)]
    fn sparse_file_reports_apparent_size_not_disk_usage() {
        use std::os::unix::fs::MetadataExt;

        let tmp = tempfile::tempdir().unwrap();
        let sparse_path = tmp.path().join("sparse.raw");

        let file = fs::File::create(&sparse_path).unwrap();
        use std::io::{Seek, Write};
        let mut writer = std::io::BufWriter::new(file);
        writer
            .seek(std::io::SeekFrom::Start(1_000_000_000))
            .unwrap();
        writer.write_all(b"\0").unwrap();
        writer.flush().unwrap();
        drop(writer);

        let meta = fs::metadata(&sparse_path).unwrap();
        let apparent = meta.len();
        let on_disk = meta.blocks() * 512;

        assert_eq!(apparent, 1_000_000_001);
        assert!(
            on_disk < 1_000_000,
            "expected sparse file to use <1MB on disk, got {on_disk}"
        );

        let progress = new_progress();
        let tree = scan_directory(tmp.path(), progress.clone());
        let scanned_size = tree.size(tree.root());

        assert_eq!(
            scanned_size, on_disk,
            "scanner should report on-disk size, not apparent size"
        );
    }

    #[cfg(target_os = "macos")]
    fn set_uf_hidden(path: &Path) {
        let c_path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let rc = unsafe { libc::lstat(c_path.as_ptr(), stat.as_mut_ptr()) };
        assert_eq!(rc, 0, "lstat failed");
        let stat = unsafe { stat.assume_init() };
        let rc = unsafe { libc::chflags(c_path.as_ptr(), stat.st_flags | 0x8000) };
        assert_eq!(rc, 0, "chflags failed");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn uf_hidden_file_detected_as_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let hidden_file = tmp.path().join("visible_name.txt");
        fs::write(&hidden_file, "secret").unwrap();
        set_uf_hidden(&hidden_file);
        fs::write(tmp.path().join("normal.txt"), "hello").unwrap();

        let progress = new_progress();
        let tree = scan_directory(tmp.path(), progress);

        let root = tree.root();
        let hidden_node = tree
            .children(root)
            .iter()
            .find(|&&c| tree.name(c) == "visible_name.txt")
            .expect("hidden file should appear in scan");
        assert!(tree.is_hidden(*hidden_node), "UF_HIDDEN file should be marked hidden");

        let normal_node = tree
            .children(root)
            .iter()
            .find(|&&c| tree.name(c) == "normal.txt")
            .expect("normal file should appear in scan");
        assert!(!tree.is_hidden(*normal_node), "normal file should not be hidden");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn uf_hidden_dir_detected_as_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let hidden_dir = tmp.path().join("secret_dir");
        fs::create_dir(&hidden_dir).unwrap();
        fs::write(hidden_dir.join("child.txt"), "data").unwrap();
        set_uf_hidden(&hidden_dir);

        let normal_dir = tmp.path().join("normal_dir");
        fs::create_dir(&normal_dir).unwrap();

        let progress = new_progress();
        let tree = scan_directory(tmp.path(), progress);

        let root = tree.root();
        let hidden_node = tree
            .children(root)
            .iter()
            .find(|&&c| tree.name(c) == "secret_dir")
            .expect("hidden dir should appear in scan");
        assert!(tree.is_hidden(*hidden_node), "UF_HIDDEN dir should be marked hidden");
        assert_eq!(tree.children(*hidden_node).len(), 1, "hidden dir contents should still be scanned");

        let normal_node = tree
            .children(root)
            .iter()
            .find(|&&c| tree.name(c) == "normal_dir")
            .expect("normal dir should appear in scan");
        assert!(!tree.is_hidden(*normal_node), "normal dir should not be hidden");
    }

    #[test]
    fn cancelled_scan_returns_empty_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file.bin"), vec![0u8; 100]).unwrap();
        fs::write(tmp.path().join("root.txt"), "data").unwrap();

        let progress = new_progress();
        progress.cancelled.store(true, Ordering::Relaxed);
        let tree = scan_directory(tmp.path(), progress.clone());

        let root = tree.root();
        assert!(tree.children(root).is_empty());
        assert_eq!(tree.size(root), 0);
        assert_eq!(progress.file_count.load(Ordering::Relaxed), 0);
    }
}
