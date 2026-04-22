//! Minimal CLI that runs the disk-cleaner scanner and exits.
//! Used for benchmarking scan speed against tools like `dust`.

use disk_cleaner::scanner::{self, ScanProgress};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

fn format_fallback_summary(total: u64, access_denied: u64, bulk_scan: u64) -> Option<String> {
    if total == 0 {
        return None;
    }

    let other_open = total
        .saturating_sub(access_denied)
        .saturating_sub(bulk_scan);

    if access_denied > 0 && other_open == 0 && bulk_scan == 0 {
        return Some(format!(
            "compatibility mode used for {} protected folder{}",
            access_denied,
            if access_denied == 1 { "" } else { "s" }
        ));
    }

    let mut parts = Vec::new();
    if access_denied > 0 {
        parts.push(format!("{access_denied} protected"));
    }
    if other_open > 0 {
        parts.push(format!("{other_open} open issue"));
    }
    if bulk_scan > 0 {
        parts.push(format!("{bulk_scan} scan issue"));
    }

    Some(format!(
        "compatibility mode used for {} folder{} ({})",
        total,
        if total == 1 { "" } else { "s" },
        parts.join(", ")
    ))
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            eprintln!("Usage: scan_only <PATH>");
            std::process::exit(1);
        });

    if !path.is_dir() {
        eprintln!("Error: not a directory: {}", path.display());
        std::process::exit(1);
    }

    let progress = Arc::new(ScanProgress {
        file_count: AtomicU64::new(0),
        total_size: AtomicU64::new(0),
        fallback_count: AtomicU64::new(0),
        access_denied_fallback_count: AtomicU64::new(0),
        bulk_scan_fallback_count: AtomicU64::new(0),
        fallback_details: std::sync::Mutex::new(Vec::new()),
        cancelled: AtomicBool::new(false),
    });

    let tree = scanner::scan_directory(&path, progress.clone());

    let files = progress.file_count.load(Ordering::Relaxed);
    let size = progress.total_size.load(Ordering::Relaxed);
    let fallbacks = progress.fallback_count.load(Ordering::Relaxed);
    let access_denied = progress
        .access_denied_fallback_count
        .load(Ordering::Relaxed);
    let bulk_scan = progress.bulk_scan_fallback_count.load(Ordering::Relaxed);
    if let Some(fallback_summary) = format_fallback_summary(fallbacks, access_denied, bulk_scan) {
        println!(
            "{} files, {} bytes ({}) [{}]",
            files,
            size,
            bytesize::ByteSize::b(size),
            fallback_summary,
        );
    } else {
        println!(
            "{} files, {} bytes ({})",
            files,
            size,
            bytesize::ByteSize::b(size)
        );
    }
    std::hint::black_box(tree);
}
