use std::ffi::OsStr;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_HANDLE_EOF, ERROR_JOURNAL_NOT_ACTIVE,
    ERROR_NO_MORE_FILES, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FileIdType,
    FileStandardInfo, GetDriveTypeW, GetFileInformationByHandleEx, GetFinalPathNameByHandleW,
    GetVolumeInformationW, GetVolumePathNameW, OPEN_EXISTING, OpenFileById, VOLUME_NAME_DOS,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    FSCTL_ENUM_USN_DATA, FSCTL_GET_NTFS_FILE_RECORD, FSCTL_GET_NTFS_VOLUME_DATA, MFT_ENUM_DATA_V0,
    NTFS_FILE_RECORD_INPUT_BUFFER, NTFS_FILE_RECORD_OUTPUT_BUFFER, NTFS_VOLUME_DATA_BUFFER,
    USN_RECORD_V2, USN_RECORD_V3,
};

const DRIVE_FIXED: u32 = 3;
const OUT_BUF_SIZE: usize = 1024 * 1024;
const SAMPLE_LIMIT: usize = 16;
const USN_PAGE_HEADER_SIZE: usize = size_of::<u64>();
const ATTR_TYPE_DATA: u32 = 0x80;
const ATTR_TYPE_END: u32 = 0xFFFF_FFFF;
const FILE_RECORD_HEADER_SIZE: usize = 0x30;
const FILE_RECORD_ATTR_OFFSET: usize = 0x14;
const NTFS_FILE_RECORD_OUTPUT_HEADER_SIZE: usize = size_of::<i64>() + size_of::<u32>();

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
    let mut output_buf = vec![0u8; NTFS_FILE_RECORD_OUTPUT_HEADER_SIZE + record_size as usize];
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
    let mut output_buf = vec![0u8; NTFS_FILE_RECORD_OUTPUT_HEADER_SIZE + record_size as usize];
    query_sizes_from_file_record(volume_handle.0, sample, &mut output_buf)
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
    let mut input = NTFS_FILE_RECORD_INPUT_BUFFER {
        FileReferenceNumber: file_id,
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

    let record = &output_buf
        [NTFS_FILE_RECORD_OUTPUT_HEADER_SIZE..NTFS_FILE_RECORD_OUTPUT_HEADER_SIZE + record_length];
    let (logical_size, allocated_size, resident_data) = parse_data_attribute_sizes(record)?;
    Ok(FileRecordSizeSample {
        logical_size,
        allocated_size,
        resident_data,
    })
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

        let attr_len =
            u32::from_le_bytes(record[offset + 4..offset + 8].try_into().unwrap()) as usize;
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
    let mut wide = Vec::with_capacity(len_bytes / size_of::<u16>());
    for chunk in record[offset..offset + len_bytes].chunks_exact(size_of::<u16>()) {
        wide.push(u16::from_le_bytes([chunk[0], chunk[1]]));
    }
    Ok(String::from_utf16_lossy(&wide))
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
}
