#![cfg(target_os = "windows")]

use disk_cleaner::scanner::windows_ntfs;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut limit: Option<usize> = None;
    let mut path: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        if arg == "--limit" {
            let value = args.next().unwrap_or_else(|| {
                eprintln!("missing value after --limit");
                std::process::exit(2);
            });
            limit = Some(value.parse::<usize>().unwrap_or_else(|_| {
                eprintln!("invalid --limit value: {value}");
                std::process::exit(2);
            }));
        } else if path.is_none() {
            path = Some(PathBuf::from(arg));
        } else {
            eprintln!("unexpected argument: {arg}");
            std::process::exit(2);
        }
    }

    let path = path.unwrap_or_else(|| std::env::current_dir().expect("current dir"));
    let start = Instant::now();
    let summary = match windows_ntfs::probe_raw_mft_for_path(&path, limit) {
        Ok(summary) => summary,
        Err(err) => {
            eprintln!("raw MFT probe failed for {}: {err}", path.display());
            std::process::exit(1);
        }
    };
    let elapsed = start.elapsed();

    println!("Path                 : {}", path.display());
    println!("Volume root          : {}", summary.volume_root.display());
    println!("Volume device        : {}", summary.volume_device);
    println!("MFT path             : {}", summary.mft_path.display());
    println!("Bytes/file record    : {}", summary.bytes_per_file_record);
    println!("MFT valid data len   : {}", summary.mft_valid_data_length);
    println!("Elapsed              : {:.3?}", elapsed);
    if summary.records_scanned > 0 {
        println!(
            "Avg/record           : {:.3?}",
            elapsed / summary.records_scanned as u32
        );
    }
    println!();
    println!("Records scanned      : {}", summary.records_scanned);
    println!("In-use records       : {}", summary.in_use_records);
    println!("Directories          : {}", summary.directories);
    println!("Files                : {}", summary.files);
    println!("Hidden               : {}", summary.hidden);
    println!("Named records        : {}", summary.named_records);
    println!("Invalid records      : {}", summary.invalid_records);
    println!("Parse errors         : {}", summary.parse_errors);
    println!("Truncated            : {}", summary.truncated);

    if !summary.samples.is_empty() {
        println!();
        println!("Samples:");
        for sample in &summary.samples {
            println!(
                "  frn={} parent={} attrs=0x{:08x} dir={} logical={:?} allocated={:?} {}",
                sample.record_number,
                sample.parent_record_number,
                sample.attributes,
                sample.is_directory,
                sample.logical_size,
                sample.allocated_size,
                sample.name
            );
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("ntfs_mft_probe is only available on Windows");
    std::process::exit(1);
}
