//! Arena-backed directory scanner — prototype for DIS-220.
//!
//! Uses `bumpalo::Bump` to bump-allocate `DirNode` boxes and children
//! `Vec`s during the scan phase, avoiding per-directory heap allocs.
//!
//! **Limitation:** `Bump` is `!Send`, so this scanner runs single-threaded.
//! The benchmark measures whether eliminating thousands of small heap
//! allocations compensates for losing rayon parallelism.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use bumpalo::Bump;

use crate::scanner::ScanProgress;
use crate::tree::{DirNode, FileLeaf, FileNode};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

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

/// Convert an OsString to a bump-allocated `&str`.
fn os_name_to_bump_str<'b>(bump: &'b Bump, name: std::ffi::OsString) -> &'b str {
    match name.into_string() {
        Ok(s) => bump.alloc_str(&s),
        Err(os) => bump.alloc_str(&os.to_string_lossy()),
    }
}

/// Arena-backed file node. Lives entirely within the bump arena's lifetime.
pub enum ArenaFileNode<'bump> {
    File {
        name: &'bump str,
        size: u64,
        hidden: bool,
    },
    Dir {
        name: &'bump str,
        size: u64,
        children: bumpalo::collections::Vec<'bump, ArenaFileNode<'bump>>,
        hidden: bool,
    },
}

impl<'bump> ArenaFileNode<'bump> {
    pub fn size(&self) -> u64 {
        match self {
            ArenaFileNode::File { size, .. } => *size,
            ArenaFileNode::Dir { size, .. } => *size,
        }
    }

    /// Convert arena-backed tree into standard `FileNode` tree (heap-allocated).
    /// This is the "exit ramp" from the arena — called once after scan completes.
    pub fn into_standard(self) -> FileNode {
        match self {
            ArenaFileNode::File { name, size, hidden } => {
                FileNode::File(FileLeaf::new(name.into(), size, hidden))
            }
            ArenaFileNode::Dir {
                name,
                size,
                children,
                hidden,
            } => {
                let std_children: Vec<FileNode> =
                    children.into_iter().map(|c| c.into_standard()).collect();
                FileNode::Dir(Box::new(DirNode {
                    name: name.into(),
                    size,
                    children: std_children,
                    expanded: false,
                    hidden,
                }))
            }
        }
    }
}

/// Scan a directory tree using bumpalo arena allocation.
///
/// All intermediate tree nodes are bump-allocated. The arena is scoped to
/// the scan lifetime and freed as a single block when the `Bump` is dropped.
///
/// Returns the arena and root node. Caller converts to standard `FileNode`
/// via `into_standard()` before dropping the arena.
pub fn arena_scan_directory(root: &Path, progress: Arc<ScanProgress>) -> FileNode {
    let bump = Bump::new();
    let skip = crate::scanner::build_skip_set_pub(root);
    let arena_root = arena_walk_dir(&bump, root, &progress, &skip);

    // Sort children recursively before conversion
    fn sort_arena_children(node: &mut ArenaFileNode<'_>) {
        if let ArenaFileNode::Dir { children, .. } = node {
            children.sort_by_key(|c| std::cmp::Reverse(c.size()));
            for child in children.iter_mut() {
                sort_arena_children(child);
            }
        }
    }

    let mut arena_root = arena_root;
    sort_arena_children(&mut arena_root);

    // Convert to standard FileNode (leaves the arena)
    let mut std_root = arena_root.into_standard();
    std_root.set_expanded(true);

    // Override name to full path (same as standard scanner)
    if let FileNode::Dir(d) = &mut std_root {
        d.name = root.to_string_lossy().into_owned().into_boxed_str();
    }

    // Arena freed here when `bump` is dropped
    std_root
}

/// Single-threaded recursive directory walk using bump allocation.
fn arena_walk_dir<'bump>(
    bump: &'bump Bump,
    dir: &Path,
    progress: &Arc<ScanProgress>,
    skip: &Arc<HashSet<std::path::PathBuf>>,
) -> ArenaFileNode<'bump> {
    let dir_name = dir
        .file_name()
        .map(|n| os_name_to_bump_str(bump, n.to_os_string()))
        .unwrap_or_else(|| bump.alloc_str(&dir.to_string_lossy()));

    let dir_hidden = std::fs::symlink_metadata(dir)
        .map(|m| is_hidden_from_metadata(dir_name, &m))
        .unwrap_or_else(|_| dir_name.starts_with('.'));

    if progress.cancelled.load(Ordering::Relaxed) {
        return ArenaFileNode::Dir {
            name: dir_name,
            size: 0,
            children: bumpalo::collections::Vec::new_in(bump),
            hidden: dir_hidden,
        };
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => {
            return ArenaFileNode::Dir {
                name: dir_name,
                size: 0,
                children: bumpalo::collections::Vec::new_in(bump),
                hidden: dir_hidden,
            };
        }
    };

    let mut children = bumpalo::collections::Vec::new_in(bump);

    for entry in entries {
        if progress.cancelled.load(Ordering::Relaxed) {
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        if ft.is_dir() {
            let path = entry.path();
            if skip.contains(&path) {
                continue;
            }
            children.push(arena_walk_dir(bump, &path, progress, skip));
        } else if ft.is_file() {
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            #[cfg(unix)]
            let len = metadata.blocks() * 512;
            #[cfg(not(unix))]
            let len = metadata.len();
            progress.file_count.fetch_add(1, Ordering::Relaxed);
            progress.total_size.fetch_add(len, Ordering::Relaxed);
            let name = os_name_to_bump_str(bump, entry.file_name());
            let hidden = is_hidden_from_metadata(name, &metadata);
            children.push(ArenaFileNode::File {
                name,
                size: len,
                hidden,
            });
        }
    }

    let size: u64 = children.iter().map(|c| c.size()).sum();

    ArenaFileNode::Dir {
        name: dir_name,
        size,
        children,
        hidden: dir_hidden,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicBool, AtomicU64};

    fn new_progress() -> Arc<ScanProgress> {
        Arc::new(ScanProgress {
            file_count: AtomicU64::new(0),
            total_size: AtomicU64::new(0),
            cancelled: AtomicBool::new(false),
        })
    }

    #[test]
    fn arena_scan_matches_standard_scan() {
        let tmp = tempfile::tempdir().unwrap();
        // Create a small tree
        for i in 0..5 {
            let dir = tmp.path().join(format!("dir_{i}"));
            fs::create_dir(&dir).unwrap();
            for j in 0..3 {
                fs::write(dir.join(format!("file_{j}.bin")), vec![0u8; 1024]).unwrap();
            }
        }

        let progress_std = new_progress();
        let std_root = crate::scanner::scan_directory(tmp.path(), progress_std.clone());

        let progress_arena = new_progress();
        let arena_root = arena_scan_directory(tmp.path(), progress_arena.clone());

        assert_eq!(std_root.size(), arena_root.size());
        assert_eq!(std_root.children().len(), arena_root.children().len());
        assert_eq!(
            progress_std.file_count.load(Ordering::Relaxed),
            progress_arena.file_count.load(Ordering::Relaxed),
        );
    }

    #[test]
    fn arena_scan_empty_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let progress = new_progress();
        let root = arena_scan_directory(tmp.path(), progress.clone());

        assert!(root.is_dir());
        assert!(root.children().is_empty());
        assert_eq!(root.size(), 0);
        assert_eq!(progress.file_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn arena_scan_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("file.txt"), "data").unwrap();

        let progress = new_progress();
        progress.cancelled.store(true, Ordering::Relaxed);
        let root = arena_scan_directory(tmp.path(), progress);

        assert!(root.children().is_empty());
        assert_eq!(root.size(), 0);
    }
}
