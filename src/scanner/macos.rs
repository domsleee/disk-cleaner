//! macOS bulk metadata walker using `getattrlistbulk(2)`.
//!
//! Returns name, type, flags, and allocation size for ALL entries in a
//! directory via batched syscalls.  Reduces per-directory cost from
//! ~2N+2 syscalls (`readdir` + per-file `lstat`) to ~⌈N/256⌉+1.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use rayon::iter::IntoParallelIterator;
use rayon::prelude::ParallelIterator;

use crate::tree::{DirNode, FileLeaf, FileNode};

use super::ScanProgress;

/// macOS UF_HIDDEN flag constant.
pub const UF_HIDDEN: u32 = 0x8000;

/// Check the hidden bit from metadata already fetched by the caller.
/// Checks both the dotfile convention and the UF_HIDDEN flag via
/// `st_flags()`, avoiding a second `lstat` per entry.
pub fn is_hidden_from_metadata(name: &str, metadata: &std::fs::Metadata) -> bool {
    use std::os::darwin::fs::MetadataExt;
    name.starts_with('.') || metadata.st_flags() & UF_HIDDEN != 0
}

// ---------------------------------------------------------------------------
// RAII fd wrapper
// ---------------------------------------------------------------------------

/// RAII wrapper for a raw file descriptor. Calls `libc::close` on drop
/// so fds are released even on early returns or panics.
struct DropFd(std::os::unix::io::RawFd);

impl Drop for DropFd {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.0);
        }
    }
}

// ---------------------------------------------------------------------------
// FFI declarations for getattrlistbulk(2)
// ---------------------------------------------------------------------------

mod bulk_attrs {
    use std::os::raw::{c_int, c_void};

    pub const ATTR_BIT_MAP_COUNT: u16 = 5;
    pub const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
    pub const ATTR_CMN_NAME: u32 = 0x0000_0001;
    pub const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
    pub const ATTR_CMN_FLAGS: u32 = 0x0004_0000;
    pub const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
    pub const FSOPT_NOFOLLOW: u64 = 0x0000_0001;
    /// `VREG` — regular file.
    pub const VREG: u32 = 1;
    /// `VDIR` — directory.
    pub const VDIR: u32 = 2;

    #[repr(C)]
    pub struct AttrList {
        pub bitmapcount: u16,
        pub reserved: u16,
        pub commonattr: u32,
        pub volattr: u32,
        pub dirattr: u32,
        pub fileattr: u32,
        pub forkattr: u32,
    }

    unsafe extern "C" {
        pub fn getattrlistbulk(
            dirfd: c_int,
            alist: *const AttrList,
            attribute_buffer: *mut c_void,
            buffer_size: usize,
            options: u64,
        ) -> c_int;
    }
}

// ---------------------------------------------------------------------------
// Bulk directory walker
// ---------------------------------------------------------------------------

/// Bulk metadata directory walker using `getattrlistbulk(2)`.
///
/// `dirfd` is an already-open file descriptor for the directory. This
/// function takes ownership and will close it via `DropFd` on return.
/// Children are opened with `openat(dirfd, name, ...)` to avoid full
/// path resolution from `/` on every syscall.
///
/// `dir_hidden` is pre-computed by the caller (the parent directory's
/// bulk call already returned this directory's flags).
pub fn walk_dir_bulk(
    dirfd: std::os::unix::io::RawFd,
    dir: &Path,
    dir_name: Box<str>,
    dir_hidden: bool,
    progress: &Arc<ScanProgress>,
    skip: &Arc<HashSet<PathBuf>>,
) -> FileNode {
    use bulk_attrs::*;
    use std::ffi::CString;

    let _dirfd_guard = DropFd(dirfd);

    let empty_dir = |name: Box<str>| {
        FileNode::Dir(Box::new(DirNode {
            name,
            size: 0,
            children: Vec::new(),
            expanded: false,
            hidden: dir_hidden,
        }))
    };

    if progress.cancelled.load(Ordering::Relaxed) {
        return empty_dir(dir_name);
    }

    static ATTRLIST: AttrList = AttrList {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_OBJTYPE | ATTR_CMN_FLAGS,
        volattr: 0,
        dirattr: 0,
        fileattr: ATTR_FILE_ALLOCSIZE,
        forkattr: 0,
    };

    // 128 KB holds ~1400 entries per batch — large enough that the vast
    // majority of directories complete in a single getattrlistbulk call.
    // Memory impact is modest: one thread-local buffer per rayon worker.
    const BUF_SIZE: usize = 128 * 1024;

    // Thread-local buffer reused across recursive calls on the same rayon
    // thread.  Avoids allocating a buffer per stack frame — without this,
    // ~8 threads × ~25 recursion depth = many MB of live buffers.
    thread_local! {
        static ATTR_BUF: std::cell::RefCell<Vec<u8>> =
            std::cell::RefCell::new(vec![0u8; BUF_SIZE]);
    }

    let mut file_children: Vec<FileNode> = Vec::new();
    // Store (name, hidden) instead of (PathBuf, hidden) — defers full
    // path construction to recursion time, avoiding the dir prefix copy
    // for every subdir entry held concurrently across rayon call stacks.
    let mut sub_dirs: Vec<(Box<str>, bool)> = Vec::new();
    // Batch progress counters per-directory to reduce atomic ops from
    // 2 per file (~6.6M total) to 2 per directory (~200K total).
    let mut batch_file_count: u64 = 0;
    let mut batch_total_size: u64 = 0;

    // Borrow the thread-local buffer, fill it via getattrlistbulk, and
    // parse all entries.  The borrow is released before we recurse into
    // subdirectories, so the same buffer can be reused by the next call
    // on this thread.
    ATTR_BUF.with(|cell| {
        let mut buf = cell.borrow_mut();

        // Each getattrlistbulk call fills the buffer with as many entries
        // as fit.  Returns entry count (>0), 0 when done, or -1 on error.
        loop {
            if progress.cancelled.load(Ordering::Relaxed) {
                break;
            }

            let count = unsafe {
                getattrlistbulk(
                    dirfd,
                    &ATTRLIST,
                    buf.as_mut_ptr().cast::<std::os::raw::c_void>(),
                    BUF_SIZE,
                    FSOPT_NOFOLLOW,
                )
            };

            if count <= 0 {
                break;
            }

            // --- Pre-count pass: count files vs dirs to pre-allocate ---
            //
            // Per-entry layout (offsets from entry start):
            //   +0   u32  length          total bytes for this entry
            //   +32  u32  objtype           (VREG=1, VDIR=2, …)
            //
            // Full layout parsed in the main pass below.
            {
                let mut off = 0usize;
                let (mut nfiles, mut ndirs) = (0usize, 0usize);
                for _ in 0..count as usize {
                    if off + 40 > BUF_SIZE {
                        break;
                    }
                    unsafe {
                        let base = buf.as_ptr().add(off);
                        let entry_len = *(base as *const u32) as usize;
                        if entry_len == 0 || off + entry_len > BUF_SIZE {
                            break;
                        }
                        let objtype = *(base.add(32) as *const u32);
                        match objtype {
                            VREG => nfiles += 1,
                            VDIR => ndirs += 1,
                            _ => {}
                        }
                        off += entry_len;
                    }
                }
                file_children.reserve(nfiles + ndirs);
                sub_dirs.reserve(ndirs);
            }

            // --- Parse returned entries ---
            //
            // Per-entry layout (offsets from entry start):
            //   +0   u32  length          total bytes for this entry
            //   +4   u32  returned common attrs
            //   +8   u32  returned vol attrs
            //  +12   u32  returned dir attrs
            //  +16   u32  returned file attrs
            //  +20   u32  returned fork attrs
            //  +24   i32  name attr_dataoff (relative to +24)
            //  +28   u32  name attr_length  (includes NUL)
            //  +32   u32  objtype           (VREG=1, VDIR=2, …)
            //  +36   u32  flags             (UF_HIDDEN = 0x8000)
            //  +40   i64  allocsize         (only if returned_file & ALLOCSIZE)
            //
            // Variable-length name data lives at +24 + attr_dataoff.
            let mut offset = 0usize;
            for _ in 0..count as usize {
                // Safety: getattrlistbulk guarantees entries are 4-byte
                // aligned and fit within `count` entries in the buffer.
                if offset + 40 > BUF_SIZE {
                    break;
                }

                unsafe {
                    let base = buf.as_ptr().add(offset);
                    let entry_len = *(base as *const u32) as usize;
                    if entry_len == 0 || offset + entry_len > BUF_SIZE {
                        break;
                    }

                    // Which file attrs were actually returned for this entry?
                    let returned_file = *(base.add(16) as *const u32);

                    // Name — attrreference_t at +24
                    let name_dataoff = *(base.add(24) as *const i32);
                    let name_bytelen = *(base.add(28) as *const u32) as usize;

                    let name_ptr = base.add(24).offset(name_dataoff as isize);
                    let name_end =
                        (name_ptr as usize).wrapping_sub(buf.as_ptr() as usize) + name_bytelen;
                    if name_bytelen == 0 || name_end > offset + entry_len {
                        offset += entry_len;
                        continue;
                    }

                    // Strip the NUL terminator.
                    let name_slice =
                        std::slice::from_raw_parts(name_ptr, name_bytelen.saturating_sub(1));
                    let name: Box<str> = match std::str::from_utf8(name_slice) {
                        Ok(s) => Box::from(s),
                        Err(_) => String::from_utf8_lossy(name_slice)
                            .into_owned()
                            .into_boxed_str(),
                    };

                    let objtype = *(base.add(32) as *const u32);
                    let flags = *(base.add(36) as *const u32);
                    let hidden = name.starts_with('.') || (flags & UF_HIDDEN != 0);

                    match objtype {
                        VDIR => {
                            sub_dirs.push((name, hidden));
                        }
                        VREG => {
                            let allocsize = if returned_file & ATTR_FILE_ALLOCSIZE != 0 {
                                *(base.add(40) as *const i64) as u64
                            } else {
                                0
                            };
                            batch_file_count += 1;
                            batch_total_size += allocsize;
                            file_children
                                .push(FileNode::File(FileLeaf::new(name, allocsize, hidden)));
                        }
                        _ => {} // skip symlinks, sockets, etc.
                    }

                    offset += entry_len;
                }
            }
        }
    }); // thread-local borrow released — safe to recurse now.

    // Flush batched progress counters (one atomic pair per directory
    // instead of per file).
    if batch_file_count > 0 {
        progress
            .file_count
            .fetch_add(batch_file_count, Ordering::Relaxed);
        progress
            .total_size
            .fetch_add(batch_total_size, Ordering::Relaxed);
    }

    // Recurse into subdirectories.  Each child opens itself with
    // openat(dirfd, name) — one-component kernel lookup instead of
    // resolving the full absolute path from /.
    let dir_children: Vec<FileNode> = sub_dirs
        .into_par_iter()
        .filter_map(|(name, hidden)| {
            let path = dir.join(&*name);
            if skip.contains(&path) {
                return None;
            }
            let child_fd = CString::new(name.as_bytes())
                .ok()
                .map(|c| unsafe {
                    libc::openat(dirfd, c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY)
                })
                .unwrap_or(-1);
            if child_fd < 0 {
                return Some(FileNode::Dir(Box::new(DirNode {
                    name,
                    size: 0,
                    children: Vec::new(),
                    expanded: false,
                    hidden,
                })));
            }
            Some(walk_dir_bulk(child_fd, &path, name, hidden, progress, skip))
        })
        .collect();

    file_children.extend(dir_children);
    file_children.shrink_to_fit();
    let size = file_children.iter().map(|c| c.size()).sum();

    FileNode::Dir(Box::new(DirNode {
        name: dir_name,
        size,
        children: file_children,
        expanded: false,
        hidden: dir_hidden,
    }))
}
