#![cfg(target_os = "windows")]

use disk_cleaner::scanner::windows_ntfs;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut count = 256usize;
    let mut path: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        if arg == "--count" {
            let value = args.next().unwrap_or_else(|| {
                eprintln!("missing value after --count");
                std::process::exit(2);
            });
            count = value.parse::<usize>().unwrap_or_else(|_| {
                eprintln!("invalid --count value: {value}");
                std::process::exit(2);
            });
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
    println!("Can open volume  : {}", eligibility.can_open_volume);
    println!("NTFS eligible    : {}", eligibility.is_eligible());

    if !eligibility.is_eligible() {
        eprintln!("NTFS size probe requires an eligible NTFS volume");
        std::process::exit(1);
    }

    let started = Instant::now();
    let samples = match windows_ntfs::query_sizes_open_by_id_for_path(&path, count) {
        Ok(samples) => samples,
        Err(err) => {
            eprintln!("OpenFileById size probe failed: {err}");
            std::process::exit(1);
        }
    };
    let elapsed = started.elapsed();

    let total_logical: u64 = samples.iter().map(|s| s.logical_size).sum();
    let total_allocated: u64 = samples.iter().map(|s| s.allocated_size).sum();
    let sampled_records: Vec<_> = samples.iter().map(|sample| sample.sample.clone()).collect();

    let file_record_started = Instant::now();
    let file_record_sizes =
        match windows_ntfs::query_sizes_from_file_records_for_samples(&path, &sampled_records) {
            Ok(results) => results,
            Err(err) => {
                eprintln!("FSCTL_GET_NTFS_FILE_RECORD probe failed: {err}");
                std::process::exit(1);
            }
        };
    let file_record_elapsed = file_record_started.elapsed();
    let file_record_first_error = if file_record_sizes.iter().all(|result| result.is_none()) {
        sampled_records
            .first()
            .and_then(|sample| {
                windows_ntfs::query_size_from_file_record_for_sample(&path, sample).err()
            })
            .map(|err| format!("{err:?}"))
    } else {
        None
    };

    let resolve_started = Instant::now();
    let resolved_paths =
        match windows_ntfs::resolve_paths_open_by_id_for_samples(&path, &sampled_records) {
            Ok(paths) => paths,
            Err(err) => {
                eprintln!("OpenFileById path resolution failed: {err}");
                std::process::exit(1);
            }
        };
    let resolve_elapsed = resolve_started.elapsed();

    let metadata_started = Instant::now();
    let mut compared = 0usize;
    let mut metadata_errors = 0usize;
    let mut metadata_mismatches = Vec::new();
    let mut file_record_compared = 0usize;
    let mut file_record_resident = 0usize;
    let mut file_record_mismatches = Vec::new();
    for ((sample, resolved_path), file_record) in samples
        .iter()
        .zip(resolved_paths.iter())
        .zip(file_record_sizes.iter())
    {
        if let Some(file_record) = file_record {
            file_record_compared += 1;
            if file_record.resident_data {
                file_record_resident += 1;
            }
            if (file_record.logical_size != sample.logical_size
                || file_record.allocated_size != sample.allocated_size)
                && file_record_mismatches.len() < 12
            {
                file_record_mismatches.push(format!(
                    "frn=0x{:x} open_by_id=({},{}) file_record=({},{}) {}",
                    sample.sample.file_id,
                    sample.logical_size,
                    sample.allocated_size,
                    file_record.logical_size,
                    file_record.allocated_size,
                    sample.sample.name
                ));
            }
        }

        let Some(resolved_path) = resolved_path else {
            continue;
        };

        match std::fs::metadata(resolved_path) {
            Ok(metadata) => {
                compared += 1;
                let actual_logical = metadata.len();
                if actual_logical != sample.logical_size && metadata_mismatches.len() < 12 {
                    metadata_mismatches.push(format!(
                        "{} logical={} metadata={}",
                        resolved_path.display(),
                        sample.logical_size,
                        actual_logical
                    ));
                }
            }
            Err(_) => {
                metadata_errors += 1;
            }
        }
    }
    let metadata_elapsed = metadata_started.elapsed();
    let resolved_count = resolved_paths.iter().filter(|path| path.is_some()).count();

    println!();
    println!("Requested count  : {count}");
    println!("Resolved count   : {}", samples.len());
    println!("ID size elapsed  : {:.3?}", elapsed);
    if !samples.is_empty() {
        println!("ID size avg/file : {:.3?}", elapsed / samples.len() as u32);
    }
    println!("Total logical    : {total_logical}");
    println!("Total allocated  : {total_allocated}");
    println!("File record hits : {file_record_compared}");
    println!("File record      : {:.3?}", file_record_elapsed);
    if file_record_compared > 0 {
        println!(
            "Record avg/file  : {:.3?}",
            file_record_elapsed / file_record_compared as u32
        );
    }
    println!("Record resident  : {file_record_resident}");
    println!("Record mismatch  : {}", file_record_mismatches.len());
    if let Some(err) = &file_record_first_error {
        println!("Record error     : {err}");
    }
    println!("Resolved paths   : {resolved_count}");
    println!("Path resolve     : {:.3?}", resolve_elapsed);
    if resolved_count > 0 {
        println!(
            "Path avg/file    : {:.3?}",
            resolve_elapsed / resolved_count as u32
        );
    }
    println!("Metadata checks  : {compared}");
    println!("Metadata elapsed : {:.3?}", metadata_elapsed);
    if compared > 0 {
        println!(
            "Metadata avg/file: {:.3?}",
            metadata_elapsed / compared as u32
        );
    }
    println!("Metadata errors  : {metadata_errors}");
    println!("Size mismatches  : {}", metadata_mismatches.len());

    if !samples.is_empty() {
        println!();
        println!("Samples:");
        for sample in samples.iter().take(12) {
            println!(
                "  frn=0x{:x} links={} logical={} allocated={} {}",
                sample.sample.file_id,
                sample.links,
                sample.logical_size,
                sample.allocated_size,
                sample.sample.name
            );
        }
    }

    if !file_record_mismatches.is_empty() {
        println!();
        println!("File Record Mismatches:");
        for mismatch in file_record_mismatches {
            println!("  {mismatch}");
        }
    }

    if !metadata_mismatches.is_empty() {
        println!();
        println!("Metadata Mismatches:");
        for mismatch in metadata_mismatches {
            println!("  {mismatch}");
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("ntfs_size_probe is only available on Windows");
    std::process::exit(1);
}
