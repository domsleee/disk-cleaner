#![cfg(target_os = "windows")]

use disk_cleaner::scanner::windows_ntfs;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut limit: Option<usize> = None;
    let mut top_root: Option<usize> = None;
    let mut project_win32_root = false;
    let mut show_timings = false;
    let mut prefer_volume = true;
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
        } else if arg == "--top-root" {
            let value = args.next().unwrap_or_else(|| {
                eprintln!("missing value after --top-root");
                std::process::exit(2);
            });
            top_root = Some(value.parse::<usize>().unwrap_or_else(|_| {
                eprintln!("invalid --top-root value: {value}");
                std::process::exit(2);
            }));
        } else if arg == "--project-win32-root" {
            project_win32_root = true;
        } else if arg == "--timings" {
            show_timings = true;
        } else if arg == "--prefer-file" {
            prefer_volume = false;
        } else if path.is_none() {
            path = Some(PathBuf::from(arg));
        } else {
            eprintln!("unexpected argument: {arg}");
            std::process::exit(2);
        }
    }

    let path = path.unwrap_or_else(|| std::env::current_dir().expect("current dir"));
    let start = Instant::now();
    let (index, timings) = match windows_ntfs::build_raw_mft_index_for_path_profiled_with_preference(
        &path,
        limit,
        prefer_volume,
    ) {
        Ok(result) => result,
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
    if show_timings {
        println!("Setup                : {:.3?}", timings.setup);
        println!("Read                 : {:.3?}", timings.read);
        println!("Process              : {:.3?}", timings.process);
        println!("Parse/fixup          : {:.3?}", timings.parse_fixup);
        println!("Merge                : {:.3?}", timings.merge);
        println!("Finish               : {:.3?}", timings.finish);
    }
    println!();
    println!("Records scanned      : {}", summary.records_scanned);
    println!("In-use records       : {}", summary.in_use_records);
    println!("Parse errors         : {}", summary.parse_errors);
    println!("Indexed entries      : {}", summary.indexed_entries);
    println!("Root entries         : {}", summary.root_entries);
    println!("Directories          : {}", summary.total_dir_entries);
    println!("Files                : {}", summary.total_file_entries);
    println!("Multi-name entries   : {}", summary.multi_name_entries);
    println!("Extra names          : {}", summary.extra_primary_names);
    println!("Extra logical names  : {}", summary.extra_primary_name_logical_size);
    println!("Extra alloc names    : {}", summary.extra_primary_name_allocated_size);
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

    if let Some(count) = top_root {
        let mut root_entries: Vec<_> = index
            .entries
            .iter()
            .filter(|entry| entry.parent_record_number == 5 && entry.record_number != 5)
            .collect();
        root_entries.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.subtree_logical_size));
        println!();
        println!("Top root entries:");
        for entry in root_entries.into_iter().take(count) {
            println!(
                "  subtree={} logical={} dir={} files={} dirs={} frn={} {}",
                entry.subtree_logical_size,
                entry.logical_size,
                entry.is_directory,
                entry.subtree_file_count,
                entry.subtree_dir_count,
                entry.record_number,
                entry.name
            );
        }
    }

    if project_win32_root {
        match windows_ntfs::project_raw_mft_index_to_win32_root(&index, &summary.volume_root) {
            Ok(projection) => {
                println!();
                println!("Win32 root projection:");
                println!("  visible roots   : {}", projection.visible_root_entries);
                println!("  filtered roots  : {}", projection.filtered_root_entries);
                println!("  blocked roots   : {}", projection.blocked_root_dirs);
                println!("  logical total   : {}", projection.total_logical_size);
                println!("  allocated total : {}", projection.total_allocated_size);
                println!("  file count      : {}", projection.total_file_entries);
                println!("  dir count       : {}", projection.total_dir_entries);
            }
            Err(err) => {
                eprintln!("Win32 root projection failed: {err}");
                std::process::exit(1);
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("ntfs_mft_index_probe is only available on Windows");
    std::process::exit(1);
}
