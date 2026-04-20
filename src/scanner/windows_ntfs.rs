use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::io::Read;
use std::io::{Seek, SeekFrom};
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::mpsc::sync_channel;
use std::thread;
use std::time::{Duration, Instant};

use rayon::prelude::{IndexedParallelIterator, ParallelIterator, ParallelSlice, ParallelSliceMut};
use smallvec::SmallVec;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_HANDLE_EOF, ERROR_JOURNAL_NOT_ACTIVE,
    ERROR_NO_MORE_FILES, ERROR_NOT_ALL_ASSIGNED, GENERIC_READ, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, LUID,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_SEQUENTIAL_SCAN, FILE_ID_DESCRIPTOR,
    FILE_ID_DESCRIPTOR_0, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_NAME_NORMALIZED, FileIdType, FileStandardInfo,
    GetDriveTypeW, GetFileInformationByHandleEx, GetFinalPathNameByHandleW,
    GetVolumeInformationW, GetVolumePathNameW, OPEN_EXISTING, OpenFileById, VOLUME_NAME_DOS,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FSCTL_ENUM_USN_DATA, FSCTL_GET_NTFS_FILE_RECORD, FSCTL_GET_NTFS_VOLUME_DATA,
    MFT_ENUM_DATA_V0, NTFS_FILE_RECORD_INPUT_BUFFER, NTFS_FILE_RECORD_OUTPUT_BUFFER,
    NTFS_VOLUME_DATA_BUFFER, USN_RECORD_V2, USN_RECORD_V3,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const DRIVE_FIXED: u32 = 3;
const OUT_BUF_SIZE: usize = 1024 * 1024;
const SAMPLE_LIMIT: usize = 16;
const RAW_MFT_PIPELINE_DEPTH: usize = 2;
const RAW_MFT_READ_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const RAW_MFT_VOLUME_READ_CHUNK_BYTES: usize = 16 * 1024 * 1024;
const RAW_MFT_MATERIALIZE_CHUNK: usize = 16_384;
const USN_PAGE_HEADER_SIZE: usize = size_of::<u64>();
const ATTR_TYPE_DATA: u32 = 0x80;
const ATTR_TYPE_FILE_NAME: u32 = 0x30;
const ATTR_TYPE_REPARSE_POINT: u32 = 0xC0;
const ATTR_TYPE_END: u32 = 0xFFFF_FFFF;
const FILE_RECORD_HEADER_SIZE: usize = 0x30;
const FILE_RECORD_ATTR_OFFSET: usize = 0x14;
const FILE_RECORD_LINK_COUNT_OFFSET: usize = 0x12;
const FILE_RECORD_FLAGS_OFFSET: usize = 0x16;
const FILE_RECORD_BASE_RECORD_OFFSET: usize = 0x20;
const NTFS_FILE_RECORD_OUTPUT_HEADER_SIZE: usize = size_of::<i64>() + size_of::<u32>();
const FILE_RECORD_FLAG_IN_USE: u16 = 0x0001;
const FILE_RECORD_FLAG_DIRECTORY: u16 = 0x0002;
const FILE_NAME_VALUE_MIN_SIZE: usize = 0x42;
const NTFS_VOLUME_ROOT_RECORD_NUMBER: u64 = 5;
const REPARSE_TAG_NAME_SURROGATE: u32 = 0x2000_0000;
const INVALID_INDEX: u32 = u32::MAX;

type ParsedFileNameList = SmallVec<[ParsedFileNameAttribute; 2]>;

#[derive(Debug, Clone)]
pub struct NtfsEligibility {
    pub original_path: PathBuf,
    pub volume_root: PathBuf,
    pub volume_device: String,
    pub filesystem_name: String,
    pub drive_type: u32,
    pub can_open_volume: bool,
    pub open_error: Option<i32>,
}

impl NtfsEligibility {
    pub fn is_ntfs(&self) -> bool {
        self.filesystem_name.eq_ignore_ascii_case("NTFS")
    }

    pub fn is_local_fixed_drive(&self) -> bool {
        self.drive_type == DRIVE_FIXED
    }

    pub fn is_eligible(&self) -> bool {
        self.is_ntfs() && self.is_local_fixed_drive() && self.can_open_volume
    }

    pub fn needs_elevation(&self) -> bool {
        self.open_error == Some(ERROR_ACCESS_DENIED as i32)
    }
}

#[derive(Debug, Clone)]
pub struct NtfsRecordSample {
    pub file_id: u128,
    pub parent_file_id: u128,
    pub attributes: u32,
    pub name: String,
    pub major_version: u16,
}

#[derive(Debug, Clone)]
pub struct OpenByIdSizeSample {
    pub sample: NtfsRecordSample,
    pub logical_size: u64,
    pub allocated_size: u64,
    pub links: u32,
}

#[derive(Debug, Clone)]
pub struct FileRecordSizeSample {
    pub logical_size: u64,
    pub allocated_size: u64,
    pub resident_data: bool,
}

#[derive(Debug, Clone, Default)]
pub struct NtfsEnumSummary {
    pub volume_root: PathBuf,
    pub volume_device: String,
    pub total_records: u64,
    pub files: u64,
    pub directories: u64,
    pub hidden: u64,
    pub reparse_points: u64,
    pub v2_records: u64,
    pub v3_records: u64,
    pub max_name_len: usize,
    pub next_file_reference_number: u64,
    pub truncated: bool,
    pub samples: Vec<NtfsRecordSample>,
}

#[derive(Debug, Clone)]
pub struct RawMftRecordSample {
    pub record_number: u64,
    pub parent_record_number: u64,
    pub attributes: u32,
    pub name: String,
    pub is_directory: bool,
    pub logical_size: Option<u64>,
    pub allocated_size: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct RawMftSummary {
    pub volume_root: PathBuf,
    pub volume_device: String,
    pub mft_path: PathBuf,
    pub bytes_per_file_record: u32,
    pub mft_valid_data_length: u64,
    pub records_scanned: u64,
    pub in_use_records: u64,
    pub invalid_records: u64,
    pub directories: u64,
    pub files: u64,
    pub entries_without_data_size: u64,
    pub files_without_data_size: u64,
    pub dirs_without_data_size: u64,
    pub hidden: u64,
    pub named_records: u64,
    pub parse_errors: u64,
    pub truncated: bool,
    pub samples: Vec<RawMftRecordSample>,
}

#[derive(Debug, Clone)]
pub struct RawMftIndexEntry {
    pub record_number: u32,
    pub parent_record_number: u32,
    pub attributes: u32,
    pub name: Box<str>,
    pub is_directory: bool,
    pub subtree_logical_size: u64,
    pub subtree_allocated_size: u64,
    pub subtree_file_count: u32,
    pub subtree_dir_count: u32,
}

#[derive(Debug, Clone, Default)]
pub struct RawMftIndexSummary {
    pub volume_root: PathBuf,
    pub volume_device: String,
    pub bytes_per_file_record: u32,
    pub records_scanned: u64,
    pub in_use_records: u64,
    pub invalid_records: u64,
    pub parse_errors: u64,
    pub indexed_entries: usize,
    pub root_entries: usize,
    pub total_file_entries: usize,
    pub total_dir_entries: usize,
    pub multi_name_entries: usize,
    pub extra_primary_names: u64,
    pub extra_primary_name_logical_size: u64,
    pub extra_primary_name_allocated_size: u64,
    pub entries_without_data_size: usize,
    pub files_without_data_size: usize,
    pub dirs_without_data_size: usize,
    pub total_logical_size: u64,
    pub total_allocated_size: u64,
    pub sample_entries: Vec<RawMftIndexEntry>,
}

pub struct RawMftIndex {
    pub summary: RawMftIndexSummary,
    pub entries: Vec<RawMftIndexEntry>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RawMftBuildTimings {
    pub setup: Duration,
    pub read: Duration,
    pub process: Duration,
    pub parse_fixup: Duration,
    pub merge: Duration,
    pub finish: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct RawMftWin32RootProjection {
    pub visible_root_entries: usize,
    pub filtered_root_entries: usize,
    pub blocked_root_dirs: usize,
    pub total_logical_size: u64,
    pub total_allocated_size: u64,
    pub total_file_entries: u64,
    pub total_dir_entries: u64,
}

#[derive(Debug, Clone, Copy)]
struct RootVisibilityEntry {
    can_recurse: bool,
}

pub fn ntfs_eligibility(path: &Path) -> io::Result<NtfsEligibility> {
    let volume_root = volume_root_for_path(path)?;
    let volume_device = volume_device_from_root(&volume_root).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!("unsupported volume root: {}", volume_root.display()),
        )
    })?;
    let filesystem_name = filesystem_name_for_root(&volume_root)?;
    let drive_type = drive_type_for_root(&volume_root);
    let open_result = open_volume(&volume_device);

    Ok(NtfsEligibility {
        original_path: path.to_path_buf(),
        volume_root,
        volume_device,
        filesystem_name,
        drive_type,
        can_open_volume: open_result.is_ok(),
        open_error: open_result.err().and_then(|e| e.raw_os_error()),
    })
}

pub fn enumerate_volume_for_path(path: &Path, limit: Option<usize>) -> io::Result<NtfsEnumSummary> {
    let eligibility = ntfs_eligibility(path)?;
    if !eligibility.is_ntfs() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "{} is on {}, not NTFS",
                path.display(),
                eligibility.filesystem_name
            ),
        ));
    }
    let handle = open_volume(&eligibility.volume_device)?;
    enumerate_volume_handle(
        handle,
        eligibility.volume_root,
        eligibility.volume_device,
        limit,
    )
}

pub fn collect_file_samples_for_path(
    path: &Path,
    limit: usize,
) -> io::Result<Vec<NtfsRecordSample>> {
    let eligibility = ntfs_eligibility(path)?;
    if !eligibility.is_ntfs() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "{} is on {}, not NTFS",
                path.display(),
                eligibility.filesystem_name
            ),
        ));
    }
    let handle = open_volume(&eligibility.volume_device)?;
    collect_file_samples_from_handle(handle, limit)
}

pub fn query_sizes_open_by_id_for_path(
    path: &Path,
    limit: usize,
) -> io::Result<Vec<OpenByIdSizeSample>> {
    let eligibility = ntfs_eligibility(path)?;
    if !eligibility.is_ntfs() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "{} is on {}, not NTFS",
                path.display(),
                eligibility.filesystem_name
            ),
        ));
    }

    let samples = {
        let enum_handle = open_volume(&eligibility.volume_device)?;
        collect_file_samples_from_handle(enum_handle, limit)?
    };

    let volume_handle = open_volume(&eligibility.volume_device)?;
    let mut results = Vec::with_capacity(samples.len());
    for sample in samples {
        if let Ok((logical_size, allocated_size, links)) =
            query_file_standard_info_by_id(volume_handle.0, &sample)
        {
            results.push(OpenByIdSizeSample {
                sample,
                logical_size,
                allocated_size,
                links,
            });
        }
    }
    Ok(results)
}

pub fn resolve_paths_open_by_id_for_samples(
    path: &Path,
    samples: &[NtfsRecordSample],
) -> io::Result<Vec<Option<PathBuf>>> {
    let eligibility = ntfs_eligibility(path)?;
    if !eligibility.is_ntfs() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "{} is on {}, not NTFS",
                path.display(),
                eligibility.filesystem_name
            ),
        ));
    }

    let volume_handle = open_volume(&eligibility.volume_device)?;
    let mut results = Vec::with_capacity(samples.len());
    for sample in samples {
        let resolved = match open_handle_by_id(volume_handle.0, sample) {
            Ok(file_handle) => final_dos_path_for_handle(file_handle.0).ok(),
            Err(_) => None,
        };
        results.push(resolved);
    }
    Ok(results)
}

pub fn query_sizes_from_file_records_for_samples(
    path: &Path,
    samples: &[NtfsRecordSample],
) -> io::Result<Vec<Option<FileRecordSizeSample>>> {
    let eligibility = ntfs_eligibility(path)?;
    if !eligibility.is_ntfs() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "{} is on {}, not NTFS",
                path.display(),
                eligibility.filesystem_name
            ),
        ));
    }

    let volume_handle = open_volume(&eligibility.volume_device)?;
    let volume_data = ntfs_volume_data(volume_handle.0)?;
    let record_size = volume_data.BytesPerFileRecordSegment;
    let mut output_buf =
        vec![0u8; NTFS_FILE_RECORD_OUTPUT_HEADER_SIZE + record_size as usize];
    let mut results = Vec::with_capacity(samples.len());
    for sample in samples {
        let parsed = query_sizes_from_file_record(volume_handle.0, sample, &mut output_buf).ok();
        results.push(parsed);
    }
    Ok(results)
}

pub fn query_size_from_file_record_for_sample(
    path: &Path,
    sample: &NtfsRecordSample,
) -> io::Result<FileRecordSizeSample> {
    let eligibility = ntfs_eligibility(path)?;
    if !eligibility.is_ntfs() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "{} is on {}, not NTFS",
                path.display(),
                eligibility.filesystem_name
            ),
        ));
    }

    let volume_handle = open_volume(&eligibility.volume_device)?;
    let volume_data = ntfs_volume_data(volume_handle.0)?;
    let record_size = volume_data.BytesPerFileRecordSegment;
    let mut output_buf =
        vec![0u8; NTFS_FILE_RECORD_OUTPUT_HEADER_SIZE + record_size as usize];
    query_sizes_from_file_record(volume_handle.0, sample, &mut output_buf)
}

pub fn probe_raw_mft_for_path(path: &Path, limit: Option<usize>) -> io::Result<RawMftSummary> {
    let eligibility = ntfs_eligibility(path)?;
    if !eligibility.is_ntfs() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "{} is on {}, not NTFS",
                path.display(),
                eligibility.filesystem_name
            ),
        ));
    }

    let volume_handle = open_volume(&eligibility.volume_device)?;
    let volume_data = ntfs_volume_data(volume_handle.0)?;
    let record_size = volume_data.BytesPerFileRecordSegment as usize;
    if record_size < FILE_RECORD_HEADER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "invalid NTFS file record size: {}",
                volume_data.BytesPerFileRecordSegment
            ),
        ));
    }

    let mft_path = eligibility.volume_root.join("$MFT");
    let valid_len = u64::try_from(volume_data.MftValidDataLength).unwrap_or_default();
    let total_records = (valid_len / record_size as u64) as usize;
    let target_records = limit.unwrap_or(total_records).min(total_records);

    let mut summary = RawMftSummary {
        volume_root: eligibility.volume_root,
        volume_device: eligibility.volume_device.clone(),
        mft_path,
        bytes_per_file_record: volume_data.BytesPerFileRecordSegment,
        mft_valid_data_length: valid_len,
        ..RawMftSummary::default()
    };

    let mft_valid_data_length = summary.mft_valid_data_length;
    if let Ok(mut file) = open_raw_ntfs_file(&summary.mft_path) {
        probe_raw_mft_from_file(
            &mut file,
            record_size,
            volume_data.BytesPerSector as usize,
            target_records,
            &mut summary,
            mft_valid_data_length,
        )?;
    } else {
        probe_raw_mft_via_volume(
            &eligibility.volume_device,
            volume_handle.0,
            &volume_data,
            record_size,
            volume_data.BytesPerSector as usize,
            target_records,
            &mut summary,
        )?;
    }

    summary.truncated = limit.is_some_and(|cap| cap < total_records);
    Ok(summary)
}

pub fn build_raw_mft_index_for_path(path: &Path, limit: Option<usize>) -> io::Result<RawMftIndex> {
    Ok(build_raw_mft_index_for_path_impl(path, limit, true)?.0)
}

pub fn build_raw_mft_index_for_path_profiled(
    path: &Path,
    limit: Option<usize>,
) -> io::Result<(RawMftIndex, RawMftBuildTimings)> {
    build_raw_mft_index_for_path_impl(path, limit, true)
}

pub fn build_raw_mft_index_for_path_profiled_with_preference(
    path: &Path,
    limit: Option<usize>,
    prefer_volume: bool,
) -> io::Result<(RawMftIndex, RawMftBuildTimings)> {
    build_raw_mft_index_for_path_impl(path, limit, prefer_volume)
}

fn build_raw_mft_index_for_path_impl(
    path: &Path,
    limit: Option<usize>,
    prefer_volume: bool,
) -> io::Result<(RawMftIndex, RawMftBuildTimings)> {
    let start = Instant::now();
    let eligibility = ntfs_eligibility(path)?;
    if !eligibility.is_ntfs() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "{} is on {}, not NTFS",
                path.display(),
                eligibility.filesystem_name
            ),
        ));
    }

    let volume_handle = open_volume(&eligibility.volume_device)?;
    let volume_data = ntfs_volume_data(volume_handle.0)?;
    let record_size = volume_data.BytesPerFileRecordSegment as usize;
    if record_size < FILE_RECORD_HEADER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "invalid NTFS file record size: {}",
                volume_data.BytesPerFileRecordSegment
            ),
        ));
    }

    let mft_path = eligibility.volume_root.join("$MFT");
    let valid_len = u64::try_from(volume_data.MftValidDataLength).unwrap_or_default();
    let total_records = (valid_len / record_size as u64) as usize;
    let target_records = limit.unwrap_or(total_records).min(total_records);

    let mut raw = RawMftIndexBuild::new(
        eligibility.volume_root.clone(),
        eligibility.volume_device.clone(),
        volume_data.BytesPerFileRecordSegment,
        target_records,
    );
    let mut timings = RawMftBuildTimings {
        setup: start.elapsed(),
        ..RawMftBuildTimings::default()
    };

    if !prefer_volume {
        if let Ok(mut file) = open_raw_ntfs_file(&mft_path) {
            build_raw_mft_index_from_file(
                &mut file,
                record_size,
                volume_data.BytesPerSector as usize,
                target_records,
                valid_len,
                &mut raw,
                &mut timings,
            )?;
        } else {
            build_raw_mft_index_via_volume(
                &eligibility.volume_device,
                volume_handle.0,
                &volume_data,
                record_size,
                volume_data.BytesPerSector as usize,
                target_records,
                valid_len,
                &mut raw,
                &mut timings,
            )?;
        }
    } else {
        build_raw_mft_index_via_volume(
            &eligibility.volume_device,
            volume_handle.0,
            &volume_data,
            record_size,
            volume_data.BytesPerSector as usize,
            target_records,
            valid_len,
            &mut raw,
            &mut timings,
        )?;
    }

    let finish_start = Instant::now();
    let index = raw.finish();
    timings.finish += finish_start.elapsed();

    Ok((index, timings))
}

pub fn project_raw_mft_index_to_win32_root(
    index: &RawMftIndex,
    volume_root: &Path,
) -> io::Result<RawMftWin32RootProjection> {
    let _ = disable_backup_privilege();
    let visibility = collect_win32_root_visibility(volume_root)?;
    Ok(project_raw_mft_index_with_root_visibility(index, &visibility))
}

fn collect_win32_root_visibility(root: &Path) -> io::Result<HashMap<String, RootVisibilityEntry>> {
    let mut visibility = HashMap::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let can_recurse = if file_type.is_dir() {
            std::fs::read_dir(entry.path()).is_ok()
        } else {
            false
        };
        visibility.insert(name, RootVisibilityEntry { can_recurse });
    }
    Ok(visibility)
}

fn project_raw_mft_index_with_root_visibility(
    index: &RawMftIndex,
    visibility: &HashMap<String, RootVisibilityEntry>,
) -> RawMftWin32RootProjection {
    let mut projection = RawMftWin32RootProjection {
        total_dir_entries: 1,
        ..RawMftWin32RootProjection::default()
    };

    for entry in index.entries.iter().filter(|entry| {
        u64::from(entry.parent_record_number) == NTFS_VOLUME_ROOT_RECORD_NUMBER
            && u64::from(entry.record_number) != NTFS_VOLUME_ROOT_RECORD_NUMBER
    }) {
        let key = entry.name.to_lowercase();
        let Some(root_state) = visibility.get(&key) else {
            projection.filtered_root_entries += 1;
            continue;
        };

        projection.visible_root_entries += 1;
        if entry.is_directory && !root_state.can_recurse {
            projection.blocked_root_dirs += 1;
            projection.total_dir_entries = projection.total_dir_entries.saturating_add(1);
            continue;
        }

        projection.total_logical_size = projection
            .total_logical_size
            .saturating_add(entry.subtree_logical_size);
        projection.total_allocated_size = projection
            .total_allocated_size
            .saturating_add(entry.subtree_allocated_size);
        projection.total_file_entries = projection
            .total_file_entries
            .saturating_add(u64::from(entry.subtree_file_count));
        projection.total_dir_entries = projection
            .total_dir_entries
            .saturating_add(u64::from(entry.subtree_dir_count));
    }

    projection
}

struct HandleGuard(HANDLE);

impl Drop for HandleGuard {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn enumerate_volume_handle(
    handle: HandleGuard,
    volume_root: PathBuf,
    volume_device: String,
    limit: Option<usize>,
) -> io::Result<NtfsEnumSummary> {
    let mut enum_data = MFT_ENUM_DATA_V0 {
        StartFileReferenceNumber: 0,
        LowUsn: 0,
        HighUsn: i64::MAX,
    };
    let mut out_buf = vec![0u8; OUT_BUF_SIZE];
    let mut summary = NtfsEnumSummary {
        volume_root,
        volume_device,
        ..NtfsEnumSummary::default()
    };
    let cap = limit.unwrap_or(usize::MAX);

    loop {
        let mut bytes_returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                handle.0,
                FSCTL_ENUM_USN_DATA,
                (&mut enum_data as *mut MFT_ENUM_DATA_V0).cast(),
                size_of::<MFT_ENUM_DATA_V0>() as u32,
                out_buf.as_mut_ptr().cast(),
                out_buf.len() as u32,
                &mut bytes_returned,
                ptr::null_mut(),
            )
        };

        if ok == 0 {
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                Some(code)
                    if code == ERROR_HANDLE_EOF as i32
                        || code == ERROR_NO_MORE_FILES as i32
                        || code == ERROR_JOURNAL_NOT_ACTIVE as i32 =>
                {
                    break;
                }
                Some(code) if code == ERROR_ACCESS_DENIED as i32 => {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!(
                            "access denied enumerating USN data for {}",
                            summary.volume_device
                        ),
                    ));
                }
                _ => return Err(err),
            }
        }

        if bytes_returned < USN_PAGE_HEADER_SIZE as u32 {
            break;
        }

        enum_data.StartFileReferenceNumber =
            u64::from_le_bytes(out_buf[..USN_PAGE_HEADER_SIZE].try_into().unwrap());
        summary.next_file_reference_number = enum_data.StartFileReferenceNumber;

        let mut offset = USN_PAGE_HEADER_SIZE;
        while offset < bytes_returned as usize {
            if summary.total_records as usize >= cap {
                summary.truncated = true;
                return Ok(summary);
            }

            let record_length = read_record_length(&out_buf[offset..bytes_returned as usize])?;
            if record_length == 0 || offset + record_length > bytes_returned as usize {
                break;
            }

            let record = &out_buf[offset..offset + record_length];
            let major_version = u16::from_le_bytes([record[4], record[5]]);
            match major_version {
                2 => {
                    let parsed = unsafe { parse_v2_record(record)? };
                    update_summary(&mut summary, parsed);
                }
                3 => {
                    let parsed = unsafe { parse_v3_record(record)? };
                    update_summary(&mut summary, parsed);
                }
                _ => {}
            }

            offset += record_length;
        }
    }

    Ok(summary)
}

fn collect_file_samples_from_handle(
    handle: HandleGuard,
    limit: usize,
) -> io::Result<Vec<NtfsRecordSample>> {
    let mut enum_data = MFT_ENUM_DATA_V0 {
        StartFileReferenceNumber: 0,
        LowUsn: 0,
        HighUsn: i64::MAX,
    };
    let mut out_buf = vec![0u8; OUT_BUF_SIZE];
    let mut samples = Vec::with_capacity(limit);

    while samples.len() < limit {
        let mut bytes_returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                handle.0,
                FSCTL_ENUM_USN_DATA,
                (&mut enum_data as *mut MFT_ENUM_DATA_V0).cast(),
                size_of::<MFT_ENUM_DATA_V0>() as u32,
                out_buf.as_mut_ptr().cast(),
                out_buf.len() as u32,
                &mut bytes_returned,
                ptr::null_mut(),
            )
        };

        if ok == 0 {
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                Some(code)
                    if code == ERROR_HANDLE_EOF as i32
                        || code == ERROR_NO_MORE_FILES as i32
                        || code == ERROR_JOURNAL_NOT_ACTIVE as i32 =>
                {
                    break;
                }
                _ => return Err(err),
            }
        }

        if bytes_returned < USN_PAGE_HEADER_SIZE as u32 {
            break;
        }

        enum_data.StartFileReferenceNumber =
            u64::from_le_bytes(out_buf[..USN_PAGE_HEADER_SIZE].try_into().unwrap());

        let mut offset = USN_PAGE_HEADER_SIZE;
        while offset < bytes_returned as usize && samples.len() < limit {
            let record_length = read_record_length(&out_buf[offset..bytes_returned as usize])?;
            if record_length == 0 || offset + record_length > bytes_returned as usize {
                break;
            }

            let record = &out_buf[offset..offset + record_length];
            let major_version = u16::from_le_bytes([record[4], record[5]]);
            let parsed = match major_version {
                2 => unsafe { parse_v2_record(record) },
                3 => unsafe { parse_v3_record(record) },
                _ => {
                    offset += record_length;
                    continue;
                }
            }?;

            if parsed.attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
                samples.push(NtfsRecordSample {
                    file_id: parsed.file_id,
                    parent_file_id: parsed.parent_file_id,
                    attributes: parsed.attributes,
                    name: parsed.name,
                    major_version: parsed.major_version,
                });
            }

            offset += record_length;
        }
    }

    Ok(samples)
}

#[derive(Debug)]
struct ParsedRecord {
    file_id: u128,
    parent_file_id: u128,
    attributes: u32,
    name: String,
    major_version: u16,
}

fn update_summary(summary: &mut NtfsEnumSummary, record: ParsedRecord) {
    summary.total_records += 1;
    if record.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        summary.directories += 1;
    } else {
        summary.files += 1;
    }
    if record.attributes & FILE_ATTRIBUTE_HIDDEN != 0 {
        summary.hidden += 1;
    }
    if record.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        summary.reparse_points += 1;
    }
    match record.major_version {
        2 => summary.v2_records += 1,
        3 => summary.v3_records += 1,
        _ => {}
    }
    summary.max_name_len = summary.max_name_len.max(record.name.len());
    if summary.samples.len() < SAMPLE_LIMIT {
        summary.samples.push(NtfsRecordSample {
            file_id: record.file_id,
            parent_file_id: record.parent_file_id,
            attributes: record.attributes,
            name: record.name,
            major_version: record.major_version,
        });
    }
}

fn query_file_standard_info_by_id(
    volume_handle: HANDLE,
    sample: &NtfsRecordSample,
) -> io::Result<(u64, u64, u32)> {
    let file_handle = open_handle_by_id(volume_handle, sample)?;
    let mut info = FILE_STANDARD_INFO::default();
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file_handle.0,
            FileStandardInfo,
            (&mut info as *mut FILE_STANDARD_INFO).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    Ok((
        info.EndOfFile as u64,
        info.AllocationSize as u64,
        info.NumberOfLinks,
    ))
}

fn ntfs_volume_data(volume_handle: HANDLE) -> io::Result<NTFS_VOLUME_DATA_BUFFER> {
    let mut volume_data = NTFS_VOLUME_DATA_BUFFER::default();
    let mut bytes_returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            volume_handle,
            FSCTL_GET_NTFS_VOLUME_DATA,
            ptr::null_mut(),
            0,
            (&mut volume_data as *mut NTFS_VOLUME_DATA_BUFFER).cast(),
            size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32,
            &mut bytes_returned,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if bytes_returned < size_of::<NTFS_VOLUME_DATA_BUFFER>() as u32 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "short NTFS volume data response",
        ));
    }
    Ok(volume_data)
}

fn open_raw_ntfs_file(path: &Path) -> io::Result<File> {
    let wide = wide_null(path.as_os_str());
    let handle = open_raw_ntfs_file_handle(&wide);
    if handle == INVALID_HANDLE_VALUE {
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(ERROR_ACCESS_DENIED as i32) {
            return Err(err);
        }

        enable_backup_privilege()?;
        let handle = open_raw_ntfs_file_handle(&wide);
        if handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_handle(handle) })
        }
    } else {
        Ok(unsafe { File::from_raw_handle(handle) })
    }
}

fn open_raw_volume_file(device: &str) -> io::Result<File> {
    let wide = wide_null(OsStr::new(device));
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_SEQUENTIAL_SCAN,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_handle(handle) })
    }
}

fn open_raw_ntfs_file_handle(wide: &[u16]) -> HANDLE {
    unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_SEQUENTIAL_SCAN | FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    }
}

fn enable_backup_privilege() -> io::Result<()> {
    let mut token = INVALID_HANDLE_VALUE;
    let ok = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = HandleGuard(token);

    let mut luid = LUID::default();
    let privilege = wide_null(OsStr::new("SeBackupPrivilege"));
    let ok = unsafe { LookupPrivilegeValueW(ptr::null(), privilege.as_ptr(), &mut luid) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    let ok = unsafe {
        AdjustTokenPrivileges(
            token.0,
            0,
            &mut privileges,
            size_of::<TOKEN_PRIVILEGES>() as u32,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let err = unsafe { GetLastError() };
    if err == ERROR_NOT_ALL_ASSIGNED {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SeBackupPrivilege is not available on this token",
        ));
    }

    Ok(())
}

fn disable_backup_privilege() -> io::Result<()> {
    let mut token = INVALID_HANDLE_VALUE;
    let ok = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = HandleGuard(token);

    let mut luid = LUID::default();
    let privilege = wide_null(OsStr::new("SeBackupPrivilege"));
    let ok = unsafe { LookupPrivilegeValueW(ptr::null(), privilege.as_ptr(), &mut luid) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: 0,
        }],
    };
    let ok = unsafe {
        AdjustTokenPrivileges(
            token.0,
            0,
            &mut privileges,
            size_of::<TOKEN_PRIVILEGES>() as u32,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

#[derive(Debug)]
struct ParsedRawMftRecord {
    record_number: u64,
    parent_record_number: u64,
    attributes: u32,
    name: String,
    is_directory: bool,
    logical_size: Option<u64>,
    allocated_size: Option<u64>,
}

#[derive(Debug)]
struct RawMftRecordFragment {
    record_number: u64,
    owner_record_number: u64,
    file_names: ParsedFileNameList,
    is_directory: bool,
    logical_size: Option<u64>,
    allocated_size: Option<u64>,
    link_count: Option<u16>,
    is_name_surrogate_reparse: bool,
}

#[derive(Debug)]
struct RawMftIndexAggregate {
    record_number: u64,
    file_names: ParsedFileNameList,
    is_directory: bool,
    logical_size: Option<u64>,
    allocated_size: Option<u64>,
    size_from_owner: bool,
    link_count: Option<u16>,
    is_name_surrogate_reparse: bool,
}

struct RawMftIndexBuild {
    summary: RawMftIndexSummary,
    entries: Vec<RawMftIndexAggregate>,
    entry_index: Vec<u32>,
}

#[derive(Default)]
struct MaterializedRawMftSummary {
    materialized_entries: usize,
    entries_without_data_size: usize,
    files_without_data_size: usize,
    dirs_without_data_size: usize,
    multi_name_entries: usize,
    extra_primary_names: u64,
    extra_primary_name_logical_size: u64,
    extra_primary_name_allocated_size: u64,
}

struct RawMftReadChunk {
    data: Vec<u8>,
    read_duration: Duration,
}

impl RawMftIndexBuild {
    fn new(
        volume_root: PathBuf,
        volume_device: String,
        bytes_per_file_record: u32,
        expected_records: usize,
    ) -> Self {
        let mut entries = Vec::new();
        let _ = entries.try_reserve_exact(expected_records);
        Self {
            summary: RawMftIndexSummary {
                volume_root,
                volume_device,
                bytes_per_file_record,
                ..RawMftIndexSummary::default()
            },
            entries,
            entry_index: vec![INVALID_INDEX; expected_records.saturating_add(1)],
        }
    }

    fn push_fragment(&mut self, fragment: RawMftRecordFragment) {
        self.summary.in_use_records += 1;
        let RawMftRecordFragment {
            record_number,
            owner_record_number,
            file_names,
            is_directory,
            logical_size,
            allocated_size,
            link_count,
            is_name_surrogate_reparse,
        } = fragment;
        let from_owner = record_number == owner_record_number;
        let owner_idx = owner_record_number as usize;
        if owner_idx >= self.entry_index.len() {
            self.entry_index.resize(owner_idx + 1, INVALID_INDEX);
        }
        if self.entry_index[owner_idx] == INVALID_INDEX {
            let idx = self.entries.len();
            let idx_u32 = u32::try_from(idx).expect("raw MFT entry index overflow");
            let (logical_size, allocated_size, size_from_owner) =
                if let (Some(logical_size), Some(allocated_size)) = (logical_size, allocated_size) {
                    (Some(logical_size), Some(allocated_size), from_owner)
                } else {
                    (None, None, false)
                };
            self.entries.push(RawMftIndexAggregate {
                record_number: owner_record_number,
                file_names,
                is_directory,
                logical_size,
                allocated_size,
                size_from_owner,
                link_count: if from_owner { link_count } else { None },
                is_name_surrogate_reparse,
            });
            self.entry_index[owner_idx] = idx_u32;
            return;
        }

        let entry = &mut self.entries[self.entry_index[owner_idx] as usize];
        entry.is_directory |= is_directory;
        entry.is_name_surrogate_reparse |= is_name_surrogate_reparse;
        if from_owner {
            entry.link_count = link_count.or(entry.link_count);
        }

        entry.file_names.extend(file_names);

        if let (Some(logical_size), Some(allocated_size)) = (logical_size, allocated_size) {
            if entry.logical_size.is_none() || (from_owner && !entry.size_from_owner) {
                entry.logical_size = Some(logical_size);
                entry.allocated_size = Some(allocated_size);
                entry.size_from_owner = from_owner;
            }
        }
    }

    fn finish(mut self) -> RawMftIndex {
        let record_index_len = self.entry_index.len();
        let materialized_summaries: Vec<_> = self
            .entries
            .par_chunks(RAW_MFT_MATERIALIZE_CHUNK)
            .map(|chunk| {
                let mut out = MaterializedRawMftSummary::default();
                for entry in chunk {
                    accumulate_materialized_entry_summary(entry, &mut out);
                }
                out
            })
            .collect();

        let total_materialized = materialized_summaries
            .iter()
            .map(|chunk| chunk.materialized_entries)
            .sum();
        let mut final_entries = Vec::with_capacity(total_materialized);
        for chunk in materialized_summaries {
            self.summary.entries_without_data_size += chunk.entries_without_data_size;
            self.summary.files_without_data_size += chunk.files_without_data_size;
            self.summary.dirs_without_data_size += chunk.dirs_without_data_size;
            self.summary.multi_name_entries += chunk.multi_name_entries;
            self.summary.extra_primary_names = self
                .summary
                .extra_primary_names
                .saturating_add(chunk.extra_primary_names);
            self.summary.extra_primary_name_logical_size = self
                .summary
                .extra_primary_name_logical_size
                .saturating_add(chunk.extra_primary_name_logical_size);
            self.summary.extra_primary_name_allocated_size = self
                .summary
                .extra_primary_name_allocated_size
                .saturating_add(chunk.extra_primary_name_allocated_size);
        }

        for entry in self.entries.into_iter() {
            let pushed =
                materialize_raw_mft_entry(entry, &mut final_entries, total_materialized);
            debug_assert!(pushed <= total_materialized);
        }

        let mut canonical_parent_index = vec![INVALID_INDEX; record_index_len];
        for (idx, entry) in final_entries.iter().enumerate() {
            let record_number = entry.record_number as usize;
            if entry.is_directory && canonical_parent_index[record_number] == INVALID_INDEX {
                canonical_parent_index[record_number] =
                    u32::try_from(idx).expect("raw MFT canonical parent index overflow");
            }
        }
        let mut parent_indices = vec![INVALID_INDEX; final_entries.len()];
        let mut pending_children = vec![0u32; final_entries.len()];
        let mut root_entries = 0usize;

        for (idx, entry) in final_entries.iter().enumerate() {
            if entry.record_number == entry.parent_record_number {
                root_entries += 1;
                continue;
            }
            let parent_record_number = entry.parent_record_number as usize;
            if parent_record_number < canonical_parent_index.len()
                && canonical_parent_index[parent_record_number] != INVALID_INDEX
            {
                let parent_idx = canonical_parent_index[parent_record_number];
                parent_indices[idx] = parent_idx;
                pending_children[parent_idx as usize] += 1;
            } else {
                root_entries += 1;
            }
        }

        let mut ready = Vec::with_capacity(final_entries.len());
        for (idx, child_count) in pending_children.iter().enumerate() {
            if *child_count == 0 {
                ready.push(u32::try_from(idx).expect("raw MFT ready index overflow"));
            }
        }

        while let Some(child_idx) = ready.pop() {
            let child_idx = child_idx as usize;
            let parent_idx = parent_indices[child_idx];
            if parent_idx == INVALID_INDEX {
                continue;
            }
            let parent_idx = parent_idx as usize;

            let child_logical = final_entries[child_idx].subtree_logical_size;
            let child_allocated = final_entries[child_idx].subtree_allocated_size;
            let child_files = final_entries[child_idx].subtree_file_count;
            let child_dirs = final_entries[child_idx].subtree_dir_count;

            let parent = &mut final_entries[parent_idx];
            parent.subtree_logical_size = parent.subtree_logical_size.saturating_add(child_logical);
            parent.subtree_allocated_size =
                parent.subtree_allocated_size.saturating_add(child_allocated);
            parent.subtree_file_count = parent.subtree_file_count.saturating_add(child_files);
            parent.subtree_dir_count = parent.subtree_dir_count.saturating_add(child_dirs);

            pending_children[parent_idx] -= 1;
            if pending_children[parent_idx] == 0 {
                ready.push(u32::try_from(parent_idx).expect("raw MFT ready parent index overflow"));
            }
        }

        self.summary.indexed_entries = final_entries.len();
        self.summary.total_file_entries = final_entries.iter().filter(|entry| !entry.is_directory).count();
        self.summary.total_dir_entries = final_entries.iter().filter(|entry| entry.is_directory).count();
        self.summary.root_entries = root_entries;

        let root_entry = final_entries
            .iter()
            .find(|entry| {
                entry.is_directory
                    && u64::from(entry.record_number) == NTFS_VOLUME_ROOT_RECORD_NUMBER
            });

        self.summary.total_logical_size = if let Some(root) = root_entry {
            root.subtree_logical_size
        } else {
            final_entries
                .iter()
                .enumerate()
                .filter(|(idx, _)| parent_indices[*idx] == INVALID_INDEX)
                .map(|(_, entry)| entry.subtree_logical_size)
                .sum()
        };
        self.summary.total_allocated_size = if let Some(root) = root_entry {
            root.subtree_allocated_size
        } else {
            final_entries
                .iter()
                .enumerate()
                .filter(|(idx, _)| parent_indices[*idx] == INVALID_INDEX)
                .map(|(_, entry)| entry.subtree_allocated_size)
                .sum()
        };
        self.summary.sample_entries = final_entries.iter().take(SAMPLE_LIMIT).cloned().collect();

        RawMftIndex {
            summary: self.summary,
            entries: final_entries,
        }
    }
}

fn materialize_raw_mft_entry(
    entry: RawMftIndexAggregate,
    out: &mut Vec<RawMftIndexEntry>,
    total_materialized: usize,
) -> usize {
    if entry.is_name_surrogate_reparse {
        return 0;
    }
    let link_limit = if entry.is_directory {
        1
    } else {
        usize::from(entry.link_count.unwrap_or(1).max(1))
    };
    let file_names = materialized_file_names(entry.file_names, link_limit);
    if file_names.is_empty() {
        return 0;
    }
    if out.len().saturating_add(file_names.len()) > total_materialized {
        panic!(
            "raw MFT materialization overflow: len={} + pushed={} > total={} record={} link_limit={} dir={}",
            out.len(),
            file_names.len(),
            total_materialized,
            entry.record_number,
            link_limit,
            entry.is_directory
        );
    }
    let logical_size = entry.logical_size.unwrap_or(0);
    let allocated_size = entry.allocated_size.unwrap_or(0);
    let pushed = file_names.len();
    for file_name in file_names {
        out.push(RawMftIndexEntry {
            record_number: u32::try_from(entry.record_number)
                .expect("raw MFT record number exceeds u32"),
            parent_record_number: u32::try_from(file_name.parent_record_number)
                .expect("raw MFT parent record number exceeds u32"),
            attributes: file_name.attributes,
            name: file_name.name.into_boxed_str(),
            is_directory: entry.is_directory,
            subtree_logical_size: logical_size,
            subtree_allocated_size: allocated_size,
            subtree_file_count: u32::from(!entry.is_directory),
            subtree_dir_count: u32::from(entry.is_directory),
        });
    }
    pushed
}

fn accumulate_materialized_entry_summary(
    entry: &RawMftIndexAggregate,
    out: &mut MaterializedRawMftSummary,
) {
    if entry.is_name_surrogate_reparse {
        return;
    }
    let link_limit = if entry.is_directory {
        1
    } else {
        usize::from(entry.link_count.unwrap_or(1).max(1))
    };
    let materialized_count = materialized_file_name_count(&entry.file_names, link_limit);
    if materialized_count == 0 {
        return;
    }

    out.materialized_entries += materialized_count;
    if materialized_count > 1 {
        out.multi_name_entries += 1;
        let extra_names = (materialized_count - 1) as u64;
        out.extra_primary_names += extra_names;
        if !entry.is_directory {
            out.extra_primary_name_logical_size = out
                .extra_primary_name_logical_size
                .saturating_add(extra_names.saturating_mul(entry.logical_size.unwrap_or(0)));
            out.extra_primary_name_allocated_size = out
                .extra_primary_name_allocated_size
                .saturating_add(extra_names.saturating_mul(entry.allocated_size.unwrap_or(0)));
        }
    }
    if entry.logical_size.is_none() {
        out.entries_without_data_size += materialized_count;
        if entry.is_directory {
            out.dirs_without_data_size += materialized_count;
        } else {
            out.files_without_data_size += materialized_count;
        }
    }
}

fn materialized_file_name_count(
    file_names: &[ParsedFileNameAttribute],
    max_names: usize,
) -> usize {
    if max_names == 0 || file_names.is_empty() {
        return 0;
    }
    if file_names.len() == 1 {
        return 1;
    }
    if file_names.len() == 2 {
        let a = &file_names[0];
        let b = &file_names[1];
        if a.parent_record_number == b.parent_record_number && a.name == b.name {
            return 1;
        }
        let mut count = usize::from(a.namespace_rank > 1) + usize::from(b.namespace_rank > 1);
        if count == 0 {
            count = 1;
        }
        return count.min(max_names);
    }

    let cloned: ParsedFileNameList = file_names.iter().cloned().collect();
    materialized_file_names(cloned, max_names).len()
}

fn parse_raw_mft_record(record: &[u8], record_number: u64) -> io::Result<Option<ParsedRawMftRecord>> {
    if record.len() < FILE_RECORD_HEADER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "short raw MFT record",
        ));
    }
    if &record[..4] != b"FILE" {
        return Ok(None);
    }

    let flags = u16::from_le_bytes(
        record[FILE_RECORD_FLAGS_OFFSET..FILE_RECORD_FLAGS_OFFSET + 2]
            .try_into()
            .unwrap(),
    );
    if flags & FILE_RECORD_FLAG_IN_USE == 0 {
        return Ok(None);
    }

    let scanned = scan_raw_mft_attributes(record)?;
    let file_name = choose_best_file_name_attribute(&scanned.file_names);
    let Some(file_name) = file_name else {
        return Ok(None);
    };

    Ok(Some(ParsedRawMftRecord {
        record_number,
        parent_record_number: file_name.parent_record_number,
        attributes: file_name.attributes,
        name: file_name.name,
        is_directory: flags & FILE_RECORD_FLAG_DIRECTORY != 0,
        logical_size: scanned.data_sizes.as_ref().map(|sizes| sizes.0),
        allocated_size: scanned.data_sizes.as_ref().map(|sizes| sizes.1),
    }))
}

fn parse_raw_mft_record_fragment(
    record: &[u8],
    record_number: u64,
) -> io::Result<Option<RawMftRecordFragment>> {
    if record.len() < FILE_RECORD_HEADER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "short raw MFT record",
        ));
    }
    if &record[..4] != b"FILE" {
        return Ok(None);
    }

    let flags = u16::from_le_bytes(
        record[FILE_RECORD_FLAGS_OFFSET..FILE_RECORD_FLAGS_OFFSET + 2]
            .try_into()
            .unwrap(),
    );
    if flags & FILE_RECORD_FLAG_IN_USE == 0 {
        return Ok(None);
    }

    let base_record_number = u64::from_le_bytes(
        record[FILE_RECORD_BASE_RECORD_OFFSET..FILE_RECORD_BASE_RECORD_OFFSET + 8]
            .try_into()
            .unwrap(),
    ) & 0x0000_FFFF_FFFF_FFFF;

    let scanned = scan_raw_mft_attributes(record)?;
    let link_count = u16::from_le_bytes(
        record[FILE_RECORD_LINK_COUNT_OFFSET..FILE_RECORD_LINK_COUNT_OFFSET + 2]
            .try_into()
            .unwrap(),
    );
    Ok(Some(RawMftRecordFragment {
        record_number,
        owner_record_number: if base_record_number == 0 {
            record_number
        } else {
            base_record_number
        },
        file_names: scanned.file_names,
        is_directory: flags & FILE_RECORD_FLAG_DIRECTORY != 0,
        logical_size: scanned.data_sizes.as_ref().map(|sizes| sizes.0),
        allocated_size: scanned.data_sizes.as_ref().map(|sizes| sizes.1),
        link_count: Some(link_count),
        is_name_surrogate_reparse: scanned.is_name_surrogate_reparse,
    }))
}

#[derive(Debug, Clone)]
struct ParsedFileNameAttribute {
    parent_record_number: u64,
    attributes: u32,
    name: String,
    namespace_rank: u8,
}

struct ScannedRawMftAttributes {
    file_names: ParsedFileNameList,
    data_sizes: Option<(u64, u64, bool)>,
    is_name_surrogate_reparse: bool,
}

fn choose_best_file_name_attribute(
    file_names: &[ParsedFileNameAttribute],
) -> Option<ParsedFileNameAttribute> {
    file_names
        .iter()
        .max_by_key(|candidate| candidate.namespace_rank)
        .cloned()
}

fn materialized_file_names(
    mut file_names: ParsedFileNameList,
    max_names: usize,
) -> ParsedFileNameList {
    if file_names.is_empty() {
        return file_names;
    }
    if file_names.len() == 1 {
        return if max_names == 0 {
            SmallVec::new()
        } else {
            file_names
        };
    }
    if file_names.len() == 2 {
        return materialized_file_names_pair(file_names, max_names);
    }

    let fallback_best = choose_best_file_name_attribute(&file_names);
    file_names.sort_unstable_by(|a, b| {
        (a.parent_record_number, &a.name, std::cmp::Reverse(a.namespace_rank)).cmp(&(
            b.parent_record_number,
            &b.name,
            std::cmp::Reverse(b.namespace_rank),
        ))
    });

    let mut materialized: ParsedFileNameList = SmallVec::with_capacity(file_names.len());
    for candidate in file_names.into_iter() {
        if let Some(existing) = materialized.last_mut() {
            if existing.parent_record_number == candidate.parent_record_number
                && existing.name == candidate.name
            {
                if candidate.namespace_rank > existing.namespace_rank {
                    *existing = candidate;
                }
                continue;
            }
        }
        if candidate.namespace_rank > 1 {
            materialized.push(candidate);
        }
    }

    if materialized.is_empty() {
        if let Some(best) = fallback_best {
            materialized.push(best);
        }
    }

    if materialized.len() > max_names {
        materialized.sort_unstable_by(|a, b| {
            std::cmp::Reverse(a.namespace_rank)
                .cmp(&std::cmp::Reverse(b.namespace_rank))
                .then_with(|| a.parent_record_number.cmp(&b.parent_record_number))
                .then_with(|| a.name.cmp(&b.name))
        });
        materialized.truncate(max_names);
    }

    materialized
}

fn materialized_file_names_pair(
    mut file_names: ParsedFileNameList,
    max_names: usize,
) -> ParsedFileNameList {
    if max_names == 0 {
        return SmallVec::new();
    }

    let b = file_names.pop().unwrap();
    let a = file_names.pop().unwrap();
    let fallback_best = if compare_materialized_priority(&a, &b).is_lt() {
        a.clone()
    } else {
        b.clone()
    };

    if a.parent_record_number == b.parent_record_number && a.name == b.name {
        let mut out = ParsedFileNameList::new();
        out.push(if a.namespace_rank >= b.namespace_rank { a } else { b });
        return out;
    }

    let mut materialized = ParsedFileNameList::new();
    if a.namespace_rank > 1 {
        materialized.push(a);
    }
    if b.namespace_rank > 1 {
        materialized.push(b);
    }

    if materialized.is_empty() {
        let mut out = ParsedFileNameList::new();
        out.push(fallback_best);
        return out;
    }
    if materialized.len() == 1 {
        return materialized;
    }

    if compare_materialized_order(&materialized[0], &materialized[1]).is_gt() {
        materialized.swap(0, 1);
    }
    if materialized.len() > max_names {
        let best_idx = if compare_materialized_priority(&materialized[0], &materialized[1]).is_lt() {
            0
        } else {
            1
        };
        let mut out = ParsedFileNameList::new();
        out.push(materialized.swap_remove(best_idx));
        return out;
    }

    materialized
}

fn compare_materialized_order(a: &ParsedFileNameAttribute, b: &ParsedFileNameAttribute) -> std::cmp::Ordering {
    (a.parent_record_number, &a.name, std::cmp::Reverse(a.namespace_rank)).cmp(&(
        b.parent_record_number,
        &b.name,
        std::cmp::Reverse(b.namespace_rank),
    ))
}

fn compare_materialized_priority(
    a: &ParsedFileNameAttribute,
    b: &ParsedFileNameAttribute,
) -> std::cmp::Ordering {
    std::cmp::Reverse(a.namespace_rank)
        .cmp(&std::cmp::Reverse(b.namespace_rank))
        .then_with(|| a.parent_record_number.cmp(&b.parent_record_number))
        .then_with(|| a.name.cmp(&b.name))
}

fn scan_raw_mft_attributes(record: &[u8]) -> io::Result<ScannedRawMftAttributes> {
    if record.len() < FILE_RECORD_HEADER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "short NTFS file record header",
        ));
    }

    let mut file_names = ParsedFileNameList::new();
    let mut data_sizes = None;
    let mut is_name_surrogate_reparse = false;
    let mut offset = u16::from_le_bytes(
        record[FILE_RECORD_ATTR_OFFSET..FILE_RECORD_ATTR_OFFSET + 2]
            .try_into()
            .unwrap(),
    ) as usize;

    while offset + 8 <= record.len() {
        let attr_type = u32::from_le_bytes(record[offset..offset + 4].try_into().unwrap());
        if attr_type == ATTR_TYPE_END {
            break;
        }

        let attr_len = u32::from_le_bytes(record[offset + 4..offset + 8].try_into().unwrap()) as usize;
        if attr_len < 0x18 || offset + attr_len > record.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid NTFS attribute length in raw MFT record",
            ));
        }

        let nonresident = record[offset + 8] != 0;
        let name_len = record[offset + 9];
        match attr_type {
            ATTR_TYPE_FILE_NAME if !nonresident => {
                let value_len =
                    u32::from_le_bytes(record[offset + 0x10..offset + 0x14].try_into().unwrap())
                        as usize;
                let value_offset =
                    u16::from_le_bytes(record[offset + 0x14..offset + 0x16].try_into().unwrap())
                        as usize;
                if value_offset + value_len <= attr_len {
                    let value = &record[offset + value_offset..offset + value_offset + value_len];
                    if value.len() >= FILE_NAME_VALUE_MIN_SIZE {
                        let name_len = value[0x40] as usize;
                        let namespace = value[0x41];
                        let name_bytes = name_len * size_of::<u16>();
                        if FILE_NAME_VALUE_MIN_SIZE + name_bytes <= value.len() {
                            let name =
                                wide_name_from_record(value, FILE_NAME_VALUE_MIN_SIZE, name_bytes)?;
                            let parent_ref =
                                u64::from_le_bytes(value[..8].try_into().unwrap())
                                    & 0x0000_FFFF_FFFF_FFFF;
                            let attributes =
                                u32::from_le_bytes(value[0x38..0x3C].try_into().unwrap());
                            file_names.push(ParsedFileNameAttribute {
                                parent_record_number: parent_ref,
                                attributes,
                                name,
                                namespace_rank: file_name_namespace_rank(namespace),
                            });
                        }
                    }
                }
            }
            ATTR_TYPE_REPARSE_POINT if !nonresident && !is_name_surrogate_reparse => {
                let value_len =
                    u32::from_le_bytes(record[offset + 0x10..offset + 0x14].try_into().unwrap())
                        as usize;
                let value_offset =
                    u16::from_le_bytes(record[offset + 0x14..offset + 0x16].try_into().unwrap())
                        as usize;
                if value_len >= size_of::<u32>() && value_offset + value_len <= attr_len {
                    let value = &record[offset + value_offset..offset + value_offset + value_len];
                    let reparse_tag = u32::from_le_bytes(value[..4].try_into().unwrap());
                    is_name_surrogate_reparse = reparse_tag & REPARSE_TAG_NAME_SURROGATE != 0;
                }
            }
            ATTR_TYPE_DATA if name_len == 0 && data_sizes.is_none() => {
                if nonresident {
                    if attr_len < 0x48 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "short nonresident NTFS data attribute",
                        ));
                    }
                    let lowest_vcn = u64::from_le_bytes(
                        record[offset + 0x10..offset + 0x18].try_into().unwrap(),
                    );
                    if lowest_vcn == 0 {
                        let allocated_size = u64::from_le_bytes(
                            record[offset + 0x28..offset + 0x30].try_into().unwrap(),
                        );
                        let logical_size = u64::from_le_bytes(
                            record[offset + 0x30..offset + 0x38].try_into().unwrap(),
                        );
                        data_sizes = Some((logical_size, allocated_size, false));
                    }
                } else {
                    let logical_size = u32::from_le_bytes(
                        record[offset + 0x10..offset + 0x14].try_into().unwrap(),
                    ) as u64;
                    data_sizes = Some((logical_size, align_up(logical_size, 8), true));
                }
            }
            _ => {}
        }

        offset += attr_len;
    }

    Ok(ScannedRawMftAttributes {
        file_names,
        data_sizes,
        is_name_surrogate_reparse,
    })
}

fn file_name_namespace_rank(namespace: u8) -> u8 {
    match namespace {
        3 => 4, // Win32 + DOS
        1 => 3, // Win32
        0 => 2, // POSIX
        2 => 1, // DOS only
        _ => 0,
    }
}

fn update_raw_mft_summary(summary: &mut RawMftSummary, record: ParsedRawMftRecord) {
    summary.in_use_records += 1;
    summary.named_records += 1;
    if record.logical_size.is_none() {
        summary.entries_without_data_size += 1;
        if record.is_directory {
            summary.dirs_without_data_size += 1;
        } else {
            summary.files_without_data_size += 1;
        }
    }
    if record.attributes & FILE_ATTRIBUTE_HIDDEN != 0 {
        summary.hidden += 1;
    }
    if record.is_directory {
        summary.directories += 1;
    } else {
        summary.files += 1;
    }

    if summary.samples.len() < SAMPLE_LIMIT {
        summary.samples.push(RawMftRecordSample {
            record_number: record.record_number,
            parent_record_number: record.parent_record_number,
            attributes: record.attributes,
            name: record.name,
            is_directory: record.is_directory,
            logical_size: record.logical_size,
            allocated_size: record.allocated_size,
        });
    }
}

fn probe_raw_mft_from_file(
    file: &mut File,
    record_size: usize,
    bytes_per_sector: usize,
    target_records: usize,
    summary: &mut RawMftSummary,
    mut bytes_remaining: u64,
) -> io::Result<()> {
    let records_per_chunk = (RAW_MFT_READ_CHUNK_BYTES / record_size).max(1);
    let mut buf = vec![0u8; records_per_chunk * record_size];
    let mut next_record_number = 0u64;
    let mut remaining_records = target_records;

    while remaining_records > 0 && bytes_remaining > 0 {
        let records_this_chunk = remaining_records.min(records_per_chunk);
        let bytes_this_chunk = records_this_chunk * record_size;
        let bytes_to_read = bytes_this_chunk.min(bytes_remaining as usize);
        let mut filled = 0usize;

        while filled < bytes_to_read {
            let read = file.read(&mut buf[filled..bytes_to_read])?;
            if read == 0 {
                break;
            }
            filled += read;
        }

        let complete_records = filled / record_size;
        if complete_records == 0 {
            break;
        }

        process_raw_mft_records(
            &mut buf[..complete_records * record_size],
            bytes_per_sector,
            &mut next_record_number,
            summary,
        );

        let consumed = complete_records * record_size;
        remaining_records = remaining_records.saturating_sub(complete_records);
        bytes_remaining = bytes_remaining.saturating_sub(consumed as u64);
        if complete_records * record_size < bytes_to_read {
            break;
        }
    }

    Ok(())
}

fn build_raw_mft_index_from_file(
    file: &mut File,
    record_size: usize,
    bytes_per_sector: usize,
    target_records: usize,
    mut bytes_remaining: u64,
    build: &mut RawMftIndexBuild,
    timings: &mut RawMftBuildTimings,
) -> io::Result<()> {
    let records_per_chunk = (RAW_MFT_READ_CHUNK_BYTES / record_size).max(1);
    let (tx, rx) = sync_channel::<io::Result<RawMftReadChunk>>(RAW_MFT_PIPELINE_DEPTH);
    let mut read_file = file.try_clone()?;
    let reader = thread::spawn(move || {
        let mut remaining_records = target_records;
        while remaining_records > 0 && bytes_remaining > 0 {
            let records_this_chunk = remaining_records.min(records_per_chunk);
            let bytes_this_chunk = records_this_chunk * record_size;
            let bytes_to_read = bytes_this_chunk.min(bytes_remaining as usize);
            let mut buf = vec![0u8; bytes_to_read];
            let mut filled = 0usize;
            let mut read_duration = Duration::default();

            while filled < bytes_to_read {
                let read_start = Instant::now();
                let read = match read_file.read(&mut buf[filled..bytes_to_read]) {
                    Ok(read) => read,
                    Err(err) => {
                        let _ = tx.send(Err(err));
                        return;
                    }
                };
                read_duration += read_start.elapsed();
                if read == 0 {
                    break;
                }
                filled += read;
            }

            let complete_records = filled / record_size;
            if complete_records == 0 {
                break;
            }

            let consumed = complete_records * record_size;
            buf.truncate(consumed);
            if tx.send(Ok(RawMftReadChunk { data: buf, read_duration })).is_err() {
                return;
            }

            remaining_records = remaining_records.saturating_sub(complete_records);
            bytes_remaining = bytes_remaining.saturating_sub(consumed as u64);
            if consumed < bytes_to_read {
                break;
            }
        }
    });

    let mut next_record_number = 0u64;
    for chunk in rx {
        let mut chunk = chunk?;
        timings.read += chunk.read_duration;
        let process_start = Instant::now();
        process_raw_mft_index_records(
            &mut chunk.data,
            record_size,
            bytes_per_sector,
            &mut next_record_number,
            build,
            timings,
        );
        timings.process += process_start.elapsed();
    }

    let _ = reader.join();

    Ok(())
}

fn probe_raw_mft_via_volume(
    volume_device: &str,
    volume_handle: HANDLE,
    volume_data: &NTFS_VOLUME_DATA_BUFFER,
    record_size: usize,
    bytes_per_sector: usize,
    target_records: usize,
    summary: &mut RawMftSummary,
) -> io::Result<()> {
    let mut output_buf =
        vec![0u8; NTFS_FILE_RECORD_OUTPUT_HEADER_SIZE + volume_data.BytesPerFileRecordSegment as usize];
    let mft_record = query_file_record_bytes(volume_handle, 0, &mut output_buf)?;
    let data_runs = parse_nonresident_data_runs(mft_record)?;
    let bytes_per_cluster = volume_data.BytesPerCluster as u64;
    let mut volume_file = open_raw_volume_file(volume_device)?;
    let mut next_record_number = 0u64;
    let mut bytes_remaining = summary.mft_valid_data_length;
    let mut partial = Vec::<u8>::new();
    let mut io_buf = vec![0u8; RAW_MFT_VOLUME_READ_CHUNK_BYTES];

    for run in data_runs {
        if bytes_remaining == 0 || next_record_number as usize >= target_records {
            break;
        }
        if run.start_lcn < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("negative LCN in MFT runlist: {}", run.start_lcn),
            ));
        }

        let run_offset = run.start_lcn as u64 * bytes_per_cluster;
        let mut run_bytes_left = run.cluster_len.saturating_mul(bytes_per_cluster);
        volume_file.seek(SeekFrom::Start(run_offset))?;

        while run_bytes_left > 0
            && bytes_remaining > 0
            && (next_record_number as usize) < target_records
        {
            let chunk = usize::try_from(run_bytes_left.min(bytes_remaining))
                .unwrap_or(usize::MAX)
                .min(io_buf.len());
            let read = volume_file.read(&mut io_buf[..chunk])?;
            if read == 0 {
                break;
            }

            run_bytes_left = run_bytes_left.saturating_sub(read as u64);
            bytes_remaining = bytes_remaining.saturating_sub(read as u64);

            if partial.is_empty() {
                let ready_records =
                    (read / record_size).min(target_records.saturating_sub(next_record_number as usize));
                let ready_bytes = ready_records * record_size;
                if ready_bytes > 0 {
                    process_raw_mft_records(
                        &mut io_buf[..ready_bytes],
                        bytes_per_sector,
                        &mut next_record_number,
                        summary,
                    );
                }
                if ready_bytes < read {
                    partial.extend_from_slice(&io_buf[ready_bytes..read]);
                }
                continue;
            }

            partial.extend_from_slice(&io_buf[..read]);

            let ready_records =
                (partial.len() / record_size).min(target_records.saturating_sub(next_record_number as usize));
            if ready_records == 0 {
                continue;
            }

            let ready_bytes = ready_records * record_size;
            process_raw_mft_records(
                &mut partial[..ready_bytes],
                bytes_per_sector,
                &mut next_record_number,
                summary,
            );
            let leftover = partial.len() - ready_bytes;
            if leftover == 0 {
                partial.clear();
            } else {
                partial.copy_within(ready_bytes.., 0);
                partial.truncate(leftover);
            }
        }
    }

    Ok(())
}

fn build_raw_mft_index_via_volume(
    volume_device: &str,
    volume_handle: HANDLE,
    volume_data: &NTFS_VOLUME_DATA_BUFFER,
    record_size: usize,
    bytes_per_sector: usize,
    target_records: usize,
    valid_len: u64,
    build: &mut RawMftIndexBuild,
    timings: &mut RawMftBuildTimings,
) -> io::Result<()> {
    let mut output_buf =
        vec![0u8; NTFS_FILE_RECORD_OUTPUT_HEADER_SIZE + volume_data.BytesPerFileRecordSegment as usize];
    let mft_record = query_file_record_bytes(volume_handle, 0, &mut output_buf)?;
    let data_runs = parse_nonresident_data_runs(mft_record)?;
    let bytes_per_cluster = volume_data.BytesPerCluster as u64;
    let mut volume_file = open_raw_volume_file(volume_device)?;
    let mut next_record_number = 0u64;
    let mut bytes_remaining = valid_len;
    let mut partial = Vec::<u8>::new();
    let mut io_buf = vec![0u8; RAW_MFT_VOLUME_READ_CHUNK_BYTES];

    for run in data_runs {
        if bytes_remaining == 0 || next_record_number as usize >= target_records {
            break;
        }
        if run.start_lcn < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("negative LCN in MFT runlist: {}", run.start_lcn),
            ));
        }

        let run_offset = run.start_lcn as u64 * bytes_per_cluster;
        let mut run_bytes_left = run.cluster_len.saturating_mul(bytes_per_cluster);
        volume_file.seek(SeekFrom::Start(run_offset))?;

        while run_bytes_left > 0
            && bytes_remaining > 0
            && (next_record_number as usize) < target_records
        {
            let chunk = usize::try_from(run_bytes_left.min(bytes_remaining))
                .unwrap_or(usize::MAX)
                .min(io_buf.len());
            let read_start = Instant::now();
            let read = volume_file.read(&mut io_buf[..chunk])?;
            timings.read += read_start.elapsed();
            if read == 0 {
                break;
            }

            run_bytes_left = run_bytes_left.saturating_sub(read as u64);
            bytes_remaining = bytes_remaining.saturating_sub(read as u64);

            if partial.is_empty() {
                let ready_records =
                    (read / record_size).min(target_records.saturating_sub(next_record_number as usize));
                let ready_bytes = ready_records * record_size;
                if ready_bytes > 0 {
                    let process_start = Instant::now();
                    process_raw_mft_index_records(
                        &mut io_buf[..ready_bytes],
                        record_size,
                        bytes_per_sector,
                        &mut next_record_number,
                        build,
                        timings,
                    );
                    timings.process += process_start.elapsed();
                }
                if ready_bytes < read {
                    partial.extend_from_slice(&io_buf[ready_bytes..read]);
                }
                continue;
            }

            partial.extend_from_slice(&io_buf[..read]);

            let ready_records =
                (partial.len() / record_size).min(target_records.saturating_sub(next_record_number as usize));
            if ready_records == 0 {
                continue;
            }

            let ready_bytes = ready_records * record_size;
            let process_start = Instant::now();
            process_raw_mft_index_records(
                &mut partial[..ready_bytes],
                record_size,
                bytes_per_sector,
                &mut next_record_number,
                build,
                timings,
            );
            timings.process += process_start.elapsed();
            let leftover = partial.len() - ready_bytes;
            if leftover == 0 {
                partial.clear();
            } else {
                partial.copy_within(ready_bytes.., 0);
                partial.truncate(leftover);
            }
        }
    }

    Ok(())
}

fn process_raw_mft_records(
    buf: &mut [u8],
    bytes_per_sector: usize,
    next_record_number: &mut u64,
    summary: &mut RawMftSummary,
) {
    for record in buf.chunks_exact_mut(summary.bytes_per_file_record as usize) {
        summary.records_scanned += 1;
        let record_number = *next_record_number;
        *next_record_number += 1;

        match apply_update_sequence_fixup(record, bytes_per_sector)
            .and_then(|fixed| parse_raw_mft_record(fixed, record_number))
        {
            Ok(Some(parsed)) => update_raw_mft_summary(summary, parsed),
            Ok(None) => {}
            Err(_) => summary.parse_errors += 1,
        }
    }
}

fn process_raw_mft_index_records(
    buf: &mut [u8],
    record_size: usize,
    bytes_per_sector: usize,
    next_record_number: &mut u64,
    build: &mut RawMftIndexBuild,
    timings: &mut RawMftBuildTimings,
) {
    let records_scanned = buf.len() / record_size;
    let base_record_number = *next_record_number;
    *next_record_number += records_scanned as u64;
    build.summary.records_scanned += records_scanned as u64;

    let parse_start = Instant::now();
    let parsed: Vec<_> = buf
        .par_chunks_exact_mut(record_size)
        .enumerate()
        .map(|(idx, record)| {
            let record_number = base_record_number + idx as u64;
            apply_update_sequence_fixup(record, bytes_per_sector)
                .and_then(|fixed| parse_raw_mft_record_fragment(fixed, record_number))
        })
        .collect();
    timings.parse_fixup += parse_start.elapsed();

    let merge_start = Instant::now();
    for result in parsed {
        match result {
            Ok(Some(fragment)) => build.push_fragment(fragment),
            Ok(None) => {}
            Err(_) => build.summary.parse_errors += 1,
        }
    }
    timings.merge += merge_start.elapsed();
}

fn apply_update_sequence_fixup(record: &mut [u8], bytes_per_sector: usize) -> io::Result<&[u8]> {
    if bytes_per_sector == 512 && record.len() == 1024 {
        return apply_update_sequence_fixup_1k(record);
    }
    apply_update_sequence_fixup_generic(record, bytes_per_sector)
}

fn apply_update_sequence_fixup_1k(record: &mut [u8]) -> io::Result<&[u8]> {
    let usa_offset = u16::from_le_bytes(record[4..6].try_into().unwrap()) as usize;
    let usa_count = u16::from_le_bytes(record[6..8].try_into().unwrap()) as usize;
    if usa_count == 3 && usa_offset + 6 <= record.len() {
        let sequence_value = [record[usa_offset], record[usa_offset + 1]];
        if record[510..512] != sequence_value || record[1022..1024] != sequence_value {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "NTFS update sequence mismatch",
            ));
        }

        record[510] = record[usa_offset + 2];
        record[511] = record[usa_offset + 3];
        record[1022] = record[usa_offset + 4];
        record[1023] = record[usa_offset + 5];
        return Ok(record);
    }

    apply_update_sequence_fixup_generic(record, 512)
}

fn apply_update_sequence_fixup_generic(
    record: &mut [u8],
    bytes_per_sector: usize,
) -> io::Result<&[u8]> {
    if bytes_per_sector < size_of::<u16>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid NTFS sector size for fixup",
        ));
    }
    if record.len() < 8 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "short NTFS record header for fixup",
        ));
    }

    let usa_offset = u16::from_le_bytes(record[4..6].try_into().unwrap()) as usize;
    let usa_count = u16::from_le_bytes(record[6..8].try_into().unwrap()) as usize;
    if usa_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid NTFS USA count",
        ));
    }

    let usa_len = usa_count
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "NTFS USA length overflow"))?;
    if usa_offset + usa_len > record.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "NTFS USA extends past record end",
        ));
    }

    let sequence_value = [record[usa_offset], record[usa_offset + 1]];
    for sector_idx in 0..usa_count.saturating_sub(1) {
        let fixup_pos = (sector_idx + 1)
            .checked_mul(bytes_per_sector)
            .and_then(|pos| pos.checked_sub(size_of::<u16>()))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "NTFS fixup offset overflow"))?;
        if fixup_pos + size_of::<u16>() > record.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "NTFS fixup position extends past record end",
            ));
        }
        if record[fixup_pos..fixup_pos + 2] != sequence_value {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "NTFS update sequence mismatch",
            ));
        }

        let replacement_offset = usa_offset + size_of::<u16>() * (sector_idx + 1);
        let replacement = [record[replacement_offset], record[replacement_offset + 1]];
        record[fixup_pos..fixup_pos + 2].copy_from_slice(&replacement);
    }

    Ok(record)
}

#[derive(Debug, Clone, Copy)]
struct DataRun {
    start_lcn: i64,
    cluster_len: u64,
}

fn parse_nonresident_data_runs(record: &[u8]) -> io::Result<Vec<DataRun>> {
    let mut offset = u16::from_le_bytes(
        record[FILE_RECORD_ATTR_OFFSET..FILE_RECORD_ATTR_OFFSET + 2]
            .try_into()
            .unwrap(),
    ) as usize;

    while offset + 8 <= record.len() {
        let attr_type = u32::from_le_bytes(record[offset..offset + 4].try_into().unwrap());
        if attr_type == ATTR_TYPE_END {
            break;
        }

        let attr_len = u32::from_le_bytes(record[offset + 4..offset + 8].try_into().unwrap()) as usize;
        if attr_len < 0x18 || offset + attr_len > record.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid NTFS attribute length in runlist parse",
            ));
        }

        let nonresident = record[offset + 8] != 0;
        let name_len = record[offset + 9];
        if attr_type == ATTR_TYPE_DATA && nonresident && name_len == 0 {
            let data_run_offset =
                u16::from_le_bytes(record[offset + 0x20..offset + 0x22].try_into().unwrap()) as usize;
            if data_run_offset >= attr_len {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid NTFS data run offset",
                ));
            }
            return parse_data_runs(&record[offset + data_run_offset..offset + attr_len]);
        }

        offset += attr_len;
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "default nonresident NTFS data attribute not found",
    ))
}

fn parse_data_runs(buf: &[u8]) -> io::Result<Vec<DataRun>> {
    let mut runs = Vec::new();
    let mut offset = 0usize;
    let mut current_lcn = 0i64;

    while offset < buf.len() {
        let header = buf[offset];
        offset += 1;
        if header == 0 {
            break;
        }

        let len_size = (header & 0x0F) as usize;
        let off_size = (header >> 4) as usize;
        if len_size == 0 || offset + len_size + off_size > buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid NTFS data run header",
            ));
        }

        let cluster_len = le_unsigned(&buf[offset..offset + len_size]);
        offset += len_size;
        let lcn_delta = le_signed(&buf[offset..offset + off_size]);
        offset += off_size;
        current_lcn = current_lcn.saturating_add(lcn_delta);

        runs.push(DataRun {
            start_lcn: current_lcn,
            cluster_len,
        });
    }

    Ok(runs)
}

fn le_unsigned(buf: &[u8]) -> u64 {
    let mut out = 0u64;
    for (idx, byte) in buf.iter().copied().enumerate() {
        out |= (byte as u64) << (idx * 8);
    }
    out
}

fn le_signed(buf: &[u8]) -> i64 {
    if buf.is_empty() {
        return 0;
    }
    let mut out = 0i64;
    for (idx, byte) in buf.iter().copied().enumerate() {
        out |= (byte as i64) << (idx * 8);
    }
    let shift = (8 - buf.len()) * 8;
    (out << shift) >> shift
}

fn query_sizes_from_file_record(
    volume_handle: HANDLE,
    sample: &NtfsRecordSample,
    output_buf: &mut [u8],
) -> io::Result<FileRecordSizeSample> {
    if sample.major_version != 2 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "FSCTL_GET_NTFS_FILE_RECORD probe currently supports only V2 IDs, got v{}",
                sample.major_version
            ),
        ));
    }

    let file_id = i64::try_from(sample.file_id & 0x0000_FFFF_FFFF_FFFF).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file id does not fit in i64: 0x{:x}", sample.file_id),
        )
    })?;
    let record = query_file_record_bytes(volume_handle, file_id, output_buf)?;
    let (logical_size, allocated_size, resident_data) = parse_data_attribute_sizes(record)?;
    Ok(FileRecordSizeSample {
        logical_size,
        allocated_size,
        resident_data,
    })
}

fn query_file_record_bytes<'a>(
    volume_handle: HANDLE,
    file_reference_number: i64,
    output_buf: &'a mut [u8],
) -> io::Result<&'a [u8]> {
    let mut input = NTFS_FILE_RECORD_INPUT_BUFFER {
        FileReferenceNumber: file_reference_number,
    };
    let mut bytes_returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            volume_handle,
            FSCTL_GET_NTFS_FILE_RECORD,
            (&mut input as *mut NTFS_FILE_RECORD_INPUT_BUFFER).cast(),
            size_of::<NTFS_FILE_RECORD_INPUT_BUFFER>() as u32,
            output_buf.as_mut_ptr().cast(),
            output_buf.len() as u32,
            &mut bytes_returned,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if bytes_returned < NTFS_FILE_RECORD_OUTPUT_HEADER_SIZE as u32 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "short NTFS file record response",
        ));
    }

    let output = unsafe {
        output_buf
            .as_ptr()
            .cast::<NTFS_FILE_RECORD_OUTPUT_BUFFER>()
            .read_unaligned()
    };
    let record_length = output.FileRecordLength as usize;
    if record_length == 0
        || NTFS_FILE_RECORD_OUTPUT_HEADER_SIZE + record_length > bytes_returned as usize
    {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "invalid NTFS file record length: record_length={record_length} bytes_returned={bytes_returned} header_size={NTFS_FILE_RECORD_OUTPUT_HEADER_SIZE}"
            ),
        ));
    }

    Ok(&output_buf
        [NTFS_FILE_RECORD_OUTPUT_HEADER_SIZE..NTFS_FILE_RECORD_OUTPUT_HEADER_SIZE + record_length])
}

fn open_handle_by_id(volume_handle: HANDLE, sample: &NtfsRecordSample) -> io::Result<HandleGuard> {
    if sample.major_version != 2 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "OpenFileById probe currently supports only V2 IDs, got v{}",
                sample.major_version
            ),
        ));
    }

    let file_id = i64::try_from(sample.file_id).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file id does not fit in i64: 0x{:x}", sample.file_id),
        )
    })?;
    let descriptor = FILE_ID_DESCRIPTOR {
        dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
        Type: FileIdType,
        Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: file_id },
    };
    let handle = unsafe {
        OpenFileById(
            volume_handle,
            &descriptor,
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    Ok(HandleGuard(handle))
}

fn final_dos_path_for_handle(handle: HANDLE) -> io::Result<PathBuf> {
    let mut buf = vec![0u16; 260];
    loop {
        let len = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                buf.as_mut_ptr(),
                buf.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        if len < buf.len() as u32 {
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            return Ok(PathBuf::from(strip_verbatim_prefix(&path)));
        }
        buf.resize(len as usize + 1, 0);
    }
}

fn strip_verbatim_prefix(path: &str) -> String {
    if let Some(stripped) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{stripped}")
    } else if let Some(stripped) = path.strip_prefix(r"\\?\") {
        stripped.to_owned()
    } else {
        path.to_owned()
    }
}

fn parse_data_attribute_sizes(record: &[u8]) -> io::Result<(u64, u64, bool)> {
    if record.len() < FILE_RECORD_HEADER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "short NTFS file record header",
        ));
    }
    if &record[..4] != b"FILE" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid NTFS file record signature",
        ));
    }

    let mut offset =
        u16::from_le_bytes(record[FILE_RECORD_ATTR_OFFSET..FILE_RECORD_ATTR_OFFSET + 2].try_into().unwrap())
            as usize;
    while offset + 8 <= record.len() {
        let attr_type = u32::from_le_bytes(record[offset..offset + 4].try_into().unwrap());
        if attr_type == ATTR_TYPE_END {
            break;
        }

        let attr_len = u32::from_le_bytes(record[offset + 4..offset + 8].try_into().unwrap()) as usize;
        if attr_len < 0x18 || offset + attr_len > record.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid NTFS attribute length",
            ));
        }

        let nonresident = record[offset + 8] != 0;
        let name_len = record[offset + 9];
        if attr_type == ATTR_TYPE_DATA && name_len == 0 {
            if nonresident {
                if attr_len < 0x48 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "short nonresident NTFS data attribute",
                    ));
                }
                let lowest_vcn =
                    u64::from_le_bytes(record[offset + 0x10..offset + 0x18].try_into().unwrap());
                if lowest_vcn != 0 {
                    offset += attr_len;
                    continue;
                }
                let allocated_size =
                    u64::from_le_bytes(record[offset + 0x28..offset + 0x30].try_into().unwrap());
                let logical_size =
                    u64::from_le_bytes(record[offset + 0x30..offset + 0x38].try_into().unwrap());
                return Ok((logical_size, allocated_size, false));
            }

            let logical_size =
                u32::from_le_bytes(record[offset + 0x10..offset + 0x14].try_into().unwrap()) as u64;
            let allocated_size = align_up(logical_size, 8);
            return Ok((logical_size, allocated_size, true));
        }

        offset += attr_len;
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "default NTFS data attribute not found",
    ))
}

fn align_up(value: u64, alignment: u64) -> u64 {
    if alignment == 0 {
        value
    } else {
        ((value + alignment - 1) / alignment) * alignment
    }
}

fn read_record_length(buf: &[u8]) -> io::Result<usize> {
    if buf.len() < 4 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "short USN record header",
        ));
    }
    Ok(u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize)
}

unsafe fn parse_v2_record(record: &[u8]) -> io::Result<ParsedRecord> {
    if record.len() < size_of::<USN_RECORD_V2>() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "short USN_RECORD_V2",
        ));
    }
    let usn = unsafe { record.as_ptr().cast::<USN_RECORD_V2>().read_unaligned() };
    let name = wide_name_from_record(
        record,
        usn.FileNameOffset as usize,
        usn.FileNameLength as usize,
    )?;
    Ok(ParsedRecord {
        file_id: usn.FileReferenceNumber as u128,
        parent_file_id: usn.ParentFileReferenceNumber as u128,
        attributes: usn.FileAttributes,
        name,
        major_version: usn.MajorVersion,
    })
}

unsafe fn parse_v3_record(record: &[u8]) -> io::Result<ParsedRecord> {
    if record.len() < size_of::<USN_RECORD_V3>() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "short USN_RECORD_V3",
        ));
    }
    let usn = unsafe { record.as_ptr().cast::<USN_RECORD_V3>().read_unaligned() };
    let name = wide_name_from_record(
        record,
        usn.FileNameOffset as usize,
        usn.FileNameLength as usize,
    )?;
    Ok(ParsedRecord {
        file_id: u128::from_le_bytes(usn.FileReferenceNumber.Identifier),
        parent_file_id: u128::from_le_bytes(usn.ParentFileReferenceNumber.Identifier),
        attributes: usn.FileAttributes,
        name,
        major_version: usn.MajorVersion,
    })
}

fn wide_name_from_record(record: &[u8], offset: usize, len_bytes: usize) -> io::Result<String> {
    if len_bytes % size_of::<u16>() != 0 || offset + len_bytes > record.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid USN filename range",
        ));
    }
    let bytes = &record[offset..offset + len_bytes];
    if let Some(ascii) = wide_ascii_name_from_record(bytes) {
        return Ok(ascii);
    }
    let mut wide = Vec::with_capacity(len_bytes / size_of::<u16>());
    for chunk in bytes.chunks_exact(size_of::<u16>()) {
        wide.push(u16::from_le_bytes([chunk[0], chunk[1]]));
    }
    Ok(String::from_utf16_lossy(&wide))
}

fn wide_ascii_name_from_record(bytes: &[u8]) -> Option<String> {
    let mut ascii = Vec::with_capacity(bytes.len() / size_of::<u16>());
    for chunk in bytes.chunks_exact(size_of::<u16>()) {
        if chunk[1] != 0 || chunk[0] >= 0x80 {
            return None;
        }
        ascii.push(chunk[0]);
    }
    Some(unsafe { String::from_utf8_unchecked(ascii) })
}

fn volume_root_for_path(path: &Path) -> io::Result<PathBuf> {
    let wide = wide_null(path.as_os_str());
    let mut buf = vec![0u16; 512];
    let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PathBuf::from(trim_wide_nul(&buf)))
}

fn filesystem_name_for_root(root: &Path) -> io::Result<String> {
    let wide = wide_null(root.as_os_str());
    let mut fs_buf = vec![0u16; 64];
    let ok = unsafe {
        GetVolumeInformationW(
            wide.as_ptr(),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            fs_buf.as_mut_ptr(),
            fs_buf.len() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(trim_wide_nul(&fs_buf))
}

fn drive_type_for_root(root: &Path) -> u32 {
    let wide = wide_null(root.as_os_str());
    unsafe { GetDriveTypeW(wide.as_ptr()) }
}

fn volume_device_from_root(root: &Path) -> Option<String> {
    let s = root.as_os_str().to_string_lossy();
    let mut chars = s.chars();
    let drive = chars.next()?;
    if chars.next()? != ':' {
        return None;
    }
    Some(format!(r"\\.\{}:", drive.to_ascii_uppercase()))
}

fn open_volume(device: &str) -> io::Result<HandleGuard> {
    let wide = wide_null(OsStr::new(device));
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(HandleGuard(handle))
    }
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn trim_wide_nul(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_name(parent_record_number: u64, name: &str, namespace_rank: u8) -> ParsedFileNameAttribute {
        ParsedFileNameAttribute {
            parent_record_number,
            attributes: 0,
            name: name.to_owned(),
            namespace_rank,
        }
    }

    #[test]
    fn volume_device_from_drive_root() {
        assert_eq!(
            volume_device_from_root(Path::new(r"C:\")),
            Some(String::from(r"\\.\C:"))
        );
    }

    #[test]
    fn volume_device_rejects_unc_root() {
        assert_eq!(
            volume_device_from_root(Path::new("\\\\server\\share\\")),
            None
        );
    }

    #[test]
    fn raw_mft_index_merges_extension_record_sizes_into_base() {
        let mut build = RawMftIndexBuild::new(PathBuf::from(r"C:\"), String::from(r"\\.\C:"), 1024, 1024);
        build.push_fragment(RawMftRecordFragment {
            record_number: 42,
            owner_record_number: 42,
            file_names: vec![file_name(5, "base.txt", 3)].into(),
            is_directory: false,
            logical_size: None,
            allocated_size: None,
            link_count: Some(1),
            is_name_surrogate_reparse: false,
        });
        build.push_fragment(RawMftRecordFragment {
            record_number: 400,
            owner_record_number: 42,
            file_names: SmallVec::new(),
            is_directory: false,
            logical_size: Some(1234),
            allocated_size: Some(4096),
            link_count: None,
            is_name_surrogate_reparse: false,
        });

        let index = build.finish();
        assert_eq!(index.entries.len(), 1);
        let entry = &index.entries[0];
        assert_eq!(entry.record_number, 42);
        assert_eq!(entry.name.as_ref(), "base.txt");
        assert_eq!(entry.subtree_logical_size, 1234);
        assert_eq!(entry.subtree_allocated_size, 4096);
        assert_eq!(index.summary.files_without_data_size, 0);
    }

    #[test]
    fn raw_mft_index_uses_extension_name_when_base_record_has_none() {
        let mut build = RawMftIndexBuild::new(PathBuf::from(r"C:\"), String::from(r"\\.\C:"), 1024, 1024);
        build.push_fragment(RawMftRecordFragment {
            record_number: 77,
            owner_record_number: 77,
            file_names: SmallVec::new(),
            is_directory: false,
            logical_size: Some(7),
            allocated_size: Some(8),
            link_count: Some(1),
            is_name_surrogate_reparse: false,
        });
        build.push_fragment(RawMftRecordFragment {
            record_number: 88,
            owner_record_number: 77,
            file_names: vec![file_name(5, "fallback.bin", 3)].into(),
            is_directory: false,
            logical_size: None,
            allocated_size: None,
            link_count: None,
            is_name_surrogate_reparse: false,
        });

        let index = build.finish();
        assert_eq!(index.entries.len(), 1);
        let entry = &index.entries[0];
        assert_eq!(entry.record_number, 77);
        assert_eq!(entry.name.as_ref(), "fallback.bin");
        assert_eq!(entry.subtree_logical_size, 7);
        assert_eq!(entry.subtree_allocated_size, 8);
    }

    #[test]
    fn raw_mft_index_materializes_multiple_primary_names() {
        let mut build = RawMftIndexBuild::new(PathBuf::from(r"C:\"), String::from(r"\\.\C:"), 1024, 1024);
        build.push_fragment(RawMftRecordFragment {
            record_number: 99,
            owner_record_number: 99,
            file_names: vec![file_name(5, "one.txt", 3), file_name(6, "two.txt", 3)].into(),
            is_directory: false,
            logical_size: Some(10),
            allocated_size: Some(16),
            link_count: Some(2),
            is_name_surrogate_reparse: false,
        });

        let index = build.finish();
        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.summary.total_file_entries, 2);
        assert_eq!(index.summary.extra_primary_names, 1);
        assert_eq!(index.summary.total_logical_size, 20);
    }

    #[test]
    fn raw_mft_index_skips_name_surrogate_reparse_entries() {
        let mut build = RawMftIndexBuild::new(PathBuf::from(r"C:\"), String::from(r"\\.\C:"), 1024, 1024);
        build.push_fragment(RawMftRecordFragment {
            record_number: 120,
            owner_record_number: 120,
            file_names: vec![file_name(5, "junction", 3)].into(),
            is_directory: true,
            logical_size: None,
            allocated_size: None,
            link_count: Some(1),
            is_name_surrogate_reparse: true,
        });

        let index = build.finish();
        assert!(index.entries.is_empty());
    }

    #[test]
    fn raw_mft_index_limits_materialized_names_to_link_count() {
        let mut build = RawMftIndexBuild::new(PathBuf::from(r"C:\"), String::from(r"\\.\C:"), 1024, 1024);
        build.push_fragment(RawMftRecordFragment {
            record_number: 140,
            owner_record_number: 140,
            file_names: vec![
                file_name(5, "one.txt", 3),
                file_name(6, "two.txt", 3),
                file_name(7, "three.txt", 2),
            ]
            .into(),
            is_directory: false,
            logical_size: Some(10),
            allocated_size: Some(16),
            link_count: Some(2),
            is_name_surrogate_reparse: false,
        });

        let index = build.finish();
        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.summary.total_file_entries, 2);
    }

    #[test]
    fn root_projection_filters_invisible_root_entries() {
        let index = RawMftIndex {
            summary: RawMftIndexSummary::default(),
            entries: vec![
                RawMftIndexEntry {
                    record_number: 5,
                    parent_record_number: 5,
                    attributes: 0,
                    name: ".".into(),
                    is_directory: true,
                    subtree_logical_size: 300,
                    subtree_allocated_size: 400,
                    subtree_file_count: 2,
                    subtree_dir_count: 3,
                },
                RawMftIndexEntry {
                    record_number: 10,
                    parent_record_number: 5,
                    attributes: 0,
                    name: "Users".into(),
                    is_directory: true,
                    subtree_logical_size: 200,
                    subtree_allocated_size: 300,
                    subtree_file_count: 2,
                    subtree_dir_count: 2,
                },
                RawMftIndexEntry {
                    record_number: 11,
                    parent_record_number: 5,
                    attributes: 0,
                    name: "$MFT".into(),
                    is_directory: false,
                    subtree_logical_size: 100,
                    subtree_allocated_size: 100,
                    subtree_file_count: 1,
                    subtree_dir_count: 0,
                },
            ],
        };
        let visibility = HashMap::from([(
            String::from("users"),
            RootVisibilityEntry { can_recurse: true },
        )]);

        let projection = project_raw_mft_index_with_root_visibility(&index, &visibility);
        assert_eq!(projection.visible_root_entries, 1);
        assert_eq!(projection.filtered_root_entries, 1);
        assert_eq!(projection.total_logical_size, 200);
        assert_eq!(projection.total_allocated_size, 300);
        assert_eq!(projection.total_file_entries, 2);
        assert_eq!(projection.total_dir_entries, 3);
    }

    #[test]
    fn root_projection_blocks_access_denied_root_dir_subtree() {
        let index = RawMftIndex {
            summary: RawMftIndexSummary::default(),
            entries: vec![RawMftIndexEntry {
                record_number: 20,
                parent_record_number: 5,
                attributes: 0,
                name: "System Volume Information".into(),
                is_directory: true,
                subtree_logical_size: 500,
                subtree_allocated_size: 600,
                subtree_file_count: 9,
                subtree_dir_count: 4,
            }],
        };
        let visibility = HashMap::from([(
            String::from("system volume information"),
            RootVisibilityEntry { can_recurse: false },
        )]);

        let projection = project_raw_mft_index_with_root_visibility(&index, &visibility);
        assert_eq!(projection.visible_root_entries, 1);
        assert_eq!(projection.blocked_root_dirs, 1);
        assert_eq!(projection.total_logical_size, 0);
        assert_eq!(projection.total_allocated_size, 0);
        assert_eq!(projection.total_file_entries, 0);
        assert_eq!(projection.total_dir_entries, 2);
    }
}
