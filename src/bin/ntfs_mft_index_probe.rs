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
    let index = match windows_ntfs::build_raw_mft_index_for_path(&path, limit) {
        Ok(index) => index,
        Err(err) => {
            eprintln!("raw MFT index probe failed for {}: {err}", path.display());
            std::process::exit(1);
        }
    };
    let elapsed = start.elapsed();
    let summary = &index.summary;

    println!("Path                 : {}", path.display());
    println!("Volume root          : {}", summary.volume_root.display());
    println!("Volume device        : {}", summary.volume_device);
    println!("Bytes/file record    : {}", summary.bytes_per_file_record);
    println!("Elapsed              : {:.3?}", elapsed);
    if summary.indexed_entries > 0 {
        println!(
            "Avg/indexed entry    : {:.3?}",
            elapsed / summary.indexed_entries as u32
        );
    }
    println!();
    println!("Records scanned      : {}", summary.records_scanned);
    println!("In-use records       : {}", summary.in_use_records);
    println!("Parse errors         : {}", summary.parse_errors);
    println!("Indexed entries      : {}", summary.indexed_entries);
    println!("Root entries         : {}", summary.root_entries);
    println!("Directories          : {}", summary.total_dir_entries);
    println!("Files                : {}", summary.total_file_entries);
    println!("No default size      : {}", summary.entries_without_data_size);
    println!("No size files        : {}", summary.files_without_data_size);
    println!("No size dirs         : {}", summary.dirs_without_data_size);
    println!("Total logical        : {}", summary.total_logical_size);
    println!("Total allocated      : {}", summary.total_allocated_size);

    if !summary.sample_entries.is_empty() {
        println!();
        println!("Samples:");
        for entry in &summary.sample_entries {
            println!(
                "  frn={} parent={} dir={} logical={} subtree={} files={} dirs={} {}",
                entry.record_number,
                entry.parent_record_number,
                entry.is_directory,
                entry.logical_size,
                entry.subtree_logical_size,
                entry.subtree_file_count,
                entry.subtree_dir_count,
                entry.name
            );
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("ntfs_mft_index_probe is only available on Windows");
    std::process::exit(1);
}
