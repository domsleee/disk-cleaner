#![cfg(target_os = "windows")]

use disk_cleaner::scanner::windows_ntfs;
use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut limit: Option<usize> = Some(50_000);
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
        } else if arg == "--full" {
            limit = None;
        } else if path.is_none() {
            path = Some(PathBuf::from(arg));
        } else {
            eprintln!("unexpected argument: {arg}");
            std::process::exit(2);
        }
    }

    let path = path.unwrap_or_else(|| std::env::current_dir().expect("current dir"));
    let eligibility = match windows_ntfs::ntfs_eligibility(&path) {
        Ok(info) => info,
        Err(err) => {
            eprintln!("eligibility probe failed for {}: {err}", path.display());
            std::process::exit(1);
        }
    };

    println!("Path             : {}", eligibility.original_path.display());
    println!("Volume root      : {}", eligibility.volume_root.display());
    println!("Volume device    : {}", eligibility.volume_device);
    println!("Filesystem       : {}", eligibility.filesystem_name);
    println!("Drive type       : {}", eligibility.drive_type);
    println!("Can open volume  : {}", eligibility.can_open_volume);
    if let Some(code) = eligibility.open_error {
        println!("Open error       : {code}");
        if eligibility.needs_elevation() {
            println!("Open hint        : likely requires elevated/admin rights");
        }
    }
    println!("NTFS eligible    : {}", eligibility.is_eligible());

    if !eligibility.is_ntfs() {
        eprintln!("not an NTFS path");
        std::process::exit(1);
    }
    if !eligibility.can_open_volume {
        eprintln!("cannot open NTFS volume handle");
        std::process::exit(1);
    }

    let summary = match windows_ntfs::enumerate_volume_for_path(&path, limit) {
        Ok(summary) => summary,
        Err(err) => {
            eprintln!("NTFS enumeration failed: {err}");
            std::process::exit(1);
        }
    };

    println!();
    println!("Records          : {}", summary.total_records);
    println!("Files            : {}", summary.files);
    println!("Directories      : {}", summary.directories);
    println!("Hidden           : {}", summary.hidden);
    println!("Reparse points   : {}", summary.reparse_points);
    println!("V2 records       : {}", summary.v2_records);
    println!("V3 records       : {}", summary.v3_records);
    println!("Max name len     : {}", summary.max_name_len);
    println!(
        "Next FRN         : 0x{:016x}",
        summary.next_file_reference_number
    );
    println!("Truncated        : {}", summary.truncated);

    if !summary.samples.is_empty() {
        println!();
        println!("Samples:");
        for sample in &summary.samples {
            println!(
                "  v{} frn=0x{:x} parent=0x{:x} attrs=0x{:08x} {}",
                sample.major_version,
                sample.file_id,
                sample.parent_file_id,
                sample.attributes,
                sample.name
            );
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("ntfs_probe is only available on Windows");
    std::process::exit(1);
}
