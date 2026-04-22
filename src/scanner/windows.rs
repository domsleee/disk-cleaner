//! Windows bulk metadata walker using `GetFileInformationByHandleEx`.
//!
//! Uses `FileIdExtdDirectoryInfo` to fetch many directory entries per syscall,
//! including name, file attributes, reparse tag, and file size.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

use rayon::iter::IntoParallelIterator;
use rayon::prelude::ParallelIterator;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_HANDLE_EOF, ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE,
    UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_EXTD_DIR_INFO, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdExtdDirectoryInfo, FileIdExtdDirectoryRestartInfo,
    GetFileInformationByHandleEx, OPEN_EXISTING,
};

use crate::tree::{DirNode, FileLeaf, FileNode};

use super::ScanProgress;

const BUF_SIZE: usize = 128 * 1024;
const FILE_ID_EXTD_DIR_INFO_HEADER: usize = std::mem::offset_of!(FILE_ID_EXTD_DIR_INFO, FileName);
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0000_0020;
const FILE_OPEN_FOR_BACKUP_INTENT: u32 = 0x0000_4000;
const OBJ_CASE_INSENSITIVE: u32 = 0x0000_0040;
const REPARSE_TAG_NAME_SURROGATE: u32 = 0x2000_0000;
const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;
const BACKSLASH: u16 = b'\\' as u16;
const COLON: u16 = b':' as u16;
const QUESTION: u16 = b'?' as u16;
const U: u16 = b'U' as u16;
const N: u16 = b'N' as u16;
const C: u16 = b'C' as u16;
const DOT: u16 = b'.' as u16;
const PAR_THRESHOLD: usize = 4;
const STATUS_ACCESS_DENIED: i32 = 0xC000_0022u32 as i32;

thread_local! {
    static DIR_BUF: RefCell<Vec<u8>> = RefCell::new(vec![0u8; BUF_SIZE]);
}

#[cfg(test)]
static FAIL_OPEN_RELATIVE_FOR: OnceLock<Mutex<Option<Box<str>>>> = OnceLock::new();

#[repr(C)]
struct IoStatusBlock {
    status: i32,
    information: usize,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: HANDLE,
    object_name: *mut UNICODE_STRING,
    attributes: u32,
    security_descriptor: *mut c_void,
    security_quality_of_service: *mut c_void,
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtOpenFile(
        file_handle: *mut HANDLE,
        desired_access: u32,
        object_attributes: *mut ObjectAttributes,
        io_status_block: *mut IoStatusBlock,
        share_access: u32,
        open_options: u32,
    ) -> i32;
}

pub struct DirectoryHandle(HANDLE);

unsafe impl Send for DirectoryHandle {}
unsafe impl Sync for DirectoryHandle {}

impl Drop for DirectoryHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

impl DirectoryHandle {
    pub fn open_root(path: &Path) -> io::Result<Self> {
        let wide = to_verbatim_wide(path);
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_LIST_DIRECTORY | SYNCHRONIZE_ACCESS,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                ptr::null_mut(),
            )
        };

        if handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }

    fn open_relative(&self, name: &str) -> io::Result<Self> {
        #[cfg(test)]
        if should_fail_open_relative(name) {
            return Err(io::Error::other("injected open_relative failure"));
        }

        let mut name_utf16: Vec<u16> = name.encode_utf16().collect();
        let mut unicode = UNICODE_STRING {
            Length: (name_utf16.len() * size_of::<u16>()) as u16,
            MaximumLength: (name_utf16.len() * size_of::<u16>()) as u16,
            Buffer: name_utf16.as_mut_ptr(),
        };
        let mut io_status = IoStatusBlock {
            status: 0,
            information: 0,
        };
        let mut attrs = ObjectAttributes {
            length: size_of::<ObjectAttributes>() as u32,
            root_directory: self.0,
            object_name: &mut unicode,
            attributes: OBJ_CASE_INSENSITIVE,
            security_descriptor: ptr::null_mut(),
            security_quality_of_service: ptr::null_mut(),
        };
        let mut handle = INVALID_HANDLE_VALUE;

        let status = unsafe {
            NtOpenFile(
                &mut handle,
                FILE_LIST_DIRECTORY | SYNCHRONIZE_ACCESS,
                &mut attrs,
                &mut io_status,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_DIRECTORY_FILE | FILE_OPEN_FOR_BACKUP_INTENT | FILE_SYNCHRONOUS_IO_NONALERT,
            )
        };

        if status >= 0 {
            Ok(Self(handle))
        } else if status == STATUS_ACCESS_DENIED {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("NtOpenFile failed with NTSTATUS 0x{status:08x}"),
            ))
        } else {
            Err(io::Error::other(format!(
                "NtOpenFile failed with NTSTATUS 0x{status:08x}"
            )))
        }
    }

    fn query_directory(&self, buffer: &mut [u8], restart: bool) -> io::Result<bool> {
        let class = if restart {
            FileIdExtdDirectoryRestartInfo
        } else {
            FileIdExtdDirectoryInfo
        };
        let result = unsafe {
            GetFileInformationByHandleEx(
                self.0,
                class,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
            )
        };

        if result != 0 {
            Ok(true)
        } else {
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                Some(code)
                    if code == ERROR_NO_MORE_FILES as i32 || code == ERROR_HANDLE_EOF as i32 =>
                {
                    Ok(false)
                }
                _ => Err(err),
            }
        }
    }
}

pub fn walk_dir_bulk(
    dir_handle: DirectoryHandle,
    dir_path: &Path,
    dir_name: Box<str>,
    dir_hidden: bool,
    progress: &Arc<ScanProgress>,
    _skip: &Arc<HashSet<PathBuf>>,
) -> io::Result<FileNode> {
    if progress.cancelled.load(Ordering::Relaxed) {
        return Ok(empty_dir(dir_name, dir_hidden));
    }

    let mut file_children: Vec<FileNode> = Vec::new();
    let mut sub_dirs: Vec<(Box<str>, bool)> = Vec::new();
    let mut batch_file_count = 0u64;
    let mut batch_total_size = 0u64;

    DIR_BUF.with(|cell| -> io::Result<()> {
        let mut buf = cell.borrow_mut();
        let mut restart = true;

        loop {
            if progress.cancelled.load(Ordering::Relaxed) {
                break;
            }

            if !dir_handle.query_directory(&mut buf, restart)? {
                break;
            }
            restart = false;

            {
                let mut offset = 0usize;
                let mut nfiles = 0usize;
                let mut ndirs = 0usize;

                loop {
                    if offset + FILE_ID_EXTD_DIR_INFO_HEADER > buf.len() {
                        break;
                    }

                    let parsed = unsafe {
                        let info = buf.as_ptr().add(offset).cast::<FILE_ID_EXTD_DIR_INFO>();
                        let next_entry =
                            (&raw const (*info).NextEntryOffset).read_unaligned() as usize;
                        let name_len =
                            (&raw const (*info).FileNameLength).read_unaligned() as usize;
                        let attrs = (&raw const (*info).FileAttributes).read_unaligned();
                        let reparse_tag = (&raw const (*info).ReparsePointTag).read_unaligned();
                        let entry_len = if next_entry == 0 {
                            buf.len() - offset
                        } else {
                            next_entry
                        };

                        if entry_len < FILE_ID_EXTD_DIR_INFO_HEADER
                            || name_len > entry_len - FILE_ID_EXTD_DIR_INFO_HEADER
                        {
                            None
                        } else {
                            let name_ptr = (&raw const (*info).FileName).cast::<u16>();
                            let name_wide =
                                from_maybe_unaligned(name_ptr, name_len / size_of::<u16>());
                            let is_directory = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
                            let is_symlink = attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0
                                && reparse_tag & REPARSE_TAG_NAME_SURROGATE != 0;
                            Some((next_entry, name_wide, is_directory, is_symlink))
                        }
                    };
                    let Some((next_entry, name, is_directory, is_symlink)) = parsed else {
                        break;
                    };

                    if !matches!(&name[..], [DOT] | [DOT, DOT]) && !is_symlink {
                        if is_directory {
                            ndirs += 1;
                        } else {
                            nfiles += 1;
                        }
                    }

                    if next_entry == 0 {
                        break;
                    }
                    offset += next_entry;
                }

                file_children.reserve(nfiles + ndirs);
                sub_dirs.reserve(ndirs);
            }

            let mut offset = 0usize;
            loop {
                if offset + FILE_ID_EXTD_DIR_INFO_HEADER > buf.len() {
                    break;
                }

                let parsed = unsafe {
                    let info = buf.as_ptr().add(offset).cast::<FILE_ID_EXTD_DIR_INFO>();

                    let next_entry = (&raw const (*info).NextEntryOffset).read_unaligned() as usize;
                    let name_len = (&raw const (*info).FileNameLength).read_unaligned() as usize;
                    let attrs = (&raw const (*info).FileAttributes).read_unaligned();
                    let reparse_tag = (&raw const (*info).ReparsePointTag).read_unaligned();
                    let logical_size = (&raw const (*info).EndOfFile).read_unaligned() as u64;
                    let entry_len = if next_entry == 0 {
                        buf.len() - offset
                    } else {
                        next_entry
                    };
                    if entry_len < FILE_ID_EXTD_DIR_INFO_HEADER
                        || name_len > entry_len - FILE_ID_EXTD_DIR_INFO_HEADER
                    {
                        None
                    } else {
                        let name_ptr = (&raw const (*info).FileName).cast::<u16>();
                        let name_wide = from_maybe_unaligned(name_ptr, name_len / size_of::<u16>());
                        let is_directory = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
                        let is_symlink = attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0
                            && reparse_tag & REPARSE_TAG_NAME_SURROGATE != 0;

                        Some((
                            next_entry,
                            name_wide,
                            attrs,
                            logical_size,
                            is_directory,
                            is_symlink,
                        ))
                    }
                };
                let Some((next_entry, name, attrs, logical_size, is_directory, is_symlink)) =
                    parsed
                else {
                    break;
                };

                if matches!(&name[..], [DOT] | [DOT, DOT]) {
                    if next_entry == 0 {
                        break;
                    }
                    offset += next_entry;
                    continue;
                }

                let name = String::from_utf16_lossy(&name).into_boxed_str();
                let hidden = name.starts_with('.') || attrs & FILE_ATTRIBUTE_HIDDEN != 0;

                if is_symlink {
                    // Match std's file_type semantics: name-surrogate reparse points
                    // are not treated as regular files/directories for scanning.
                } else if is_directory {
                    sub_dirs.push((name, hidden));
                } else {
                    batch_file_count += 1;
                    batch_total_size += logical_size;
                    file_children.push(FileNode::File(FileLeaf::new(name, logical_size, hidden)));
                }

                if next_entry == 0 {
                    break;
                }
                offset += next_entry;
            }
        }

        Ok(())
    })?;

    if batch_file_count > 0 {
        progress
            .file_count
            .fetch_add(batch_file_count, Ordering::Relaxed);
        progress
            .total_size
            .fetch_add(batch_total_size, Ordering::Relaxed);
    }

    file_children.reserve(sub_dirs.len());

    let dir_children: Vec<FileNode> = if sub_dirs.len() <= PAR_THRESHOLD {
        let mut children = Vec::with_capacity(sub_dirs.len());
        for (name, hidden) in sub_dirs {
            let child = walk_child_dir(&dir_handle, dir_path, name, hidden, progress, _skip);
            children.push(child);
        }
        children
    } else {
        sub_dirs
            .into_par_iter()
            .map(|(name, hidden)| walk_child_dir(&dir_handle, dir_path, name, hidden, progress, _skip))
            .collect()
    };

    file_children.extend(dir_children);
    let size = file_children.iter().map(|c| c.size()).sum();

    Ok(FileNode::Dir(Box::new(DirNode {
        name: dir_name,
        size,
        children: file_children,
        expanded: false,
        hidden: dir_hidden,
    })))
}

fn empty_dir(name: Box<str>, hidden: bool) -> FileNode {
    FileNode::Dir(Box::new(DirNode {
        name,
        size: 0,
        children: Vec::new(),
        expanded: false,
        hidden,
    }))
}

fn walk_child_dir(
    dir_handle: &DirectoryHandle,
    dir_path: &Path,
    name: Box<str>,
    hidden: bool,
    progress: &Arc<ScanProgress>,
    skip: &Arc<HashSet<PathBuf>>,
) -> FileNode {
    let child_path = dir_path.join(name.as_ref());
    match dir_handle.open_relative(&name) {
        Ok(child_handle) => match walk_dir_bulk(child_handle, &child_path, name, hidden, progress, skip) {
            Ok(node) => node,
            Err(err) => {
                progress.record_windows_bulk_scan_fallback("child bulk scan", &child_path, &err);
                super::walk_dir(&child_path, progress, skip)
            }
        },
        Err(err) => {
            progress.record_windows_child_open_fallback(&child_path, &err);
            super::walk_dir(&child_path, progress, skip)
        }
    }
}

#[cfg(test)]
pub(crate) struct OpenRelativeFailureGuard;

#[cfg(test)]
impl Drop for OpenRelativeFailureGuard {
    fn drop(&mut self) {
        clear_open_relative_failure();
    }
}

#[cfg(test)]
pub(crate) fn fail_open_relative_for_name(name: &str) -> OpenRelativeFailureGuard {
    let slot = FAIL_OPEN_RELATIVE_FOR.get_or_init(|| Mutex::new(None));
    *slot.lock().expect("open_relative failure lock poisoned") = Some(name.into());
    OpenRelativeFailureGuard
}

#[cfg(test)]
fn clear_open_relative_failure() {
    if let Some(slot) = FAIL_OPEN_RELATIVE_FOR.get() {
        *slot.lock().expect("open_relative failure lock poisoned") = None;
    }
}

#[cfg(test)]
fn should_fail_open_relative(name: &str) -> bool {
    FAIL_OPEN_RELATIVE_FOR
        .get()
        .and_then(|slot| {
            slot.lock()
                .expect("open_relative failure lock poisoned")
                .as_deref()
                .map(|target| target == name)
        })
        .unwrap_or(false)
}

fn to_verbatim_wide(path: &Path) -> Vec<u16> {
    let src: Vec<u16> = path.as_os_str().encode_wide().collect();
    let mut out = Vec::with_capacity(src.len() + 8);

    if src.starts_with(&[BACKSLASH, BACKSLASH, QUESTION, BACKSLASH]) {
        out.extend_from_slice(&src);
    } else if src.starts_with(&[BACKSLASH, BACKSLASH]) {
        out.extend_from_slice(&[
            BACKSLASH, BACKSLASH, QUESTION, BACKSLASH, U, N, C, BACKSLASH,
        ]);
        out.extend_from_slice(&src[2..]);
    } else if src.get(1) == Some(&COLON) {
        out.extend_from_slice(&[BACKSLASH, BACKSLASH, QUESTION, BACKSLASH]);
        out.extend_from_slice(&src);
    } else {
        out.extend_from_slice(&src);
    }

    out.push(0);
    out
}

unsafe fn from_maybe_unaligned<'a>(ptr: *const u16, len: usize) -> Cow<'a, [u16]> {
    if ptr.is_aligned() {
        Cow::Borrowed(unsafe { std::slice::from_raw_parts(ptr, len) })
    } else {
        Cow::Owned(
            (0..len)
                .map(|i| unsafe { ptr.add(i).read_unaligned() })
                .collect(),
        )
    }
}
