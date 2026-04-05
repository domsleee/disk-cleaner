//! Minimal CLI that runs the disk-cleaner scanner and exits.
//! Used for benchmarking scan speed against tools like `dust`.

use disk_cleaner::scanner::{self, ScanProgress};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

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
        cancelled: AtomicBool::new(false),
        permission_denied: AtomicU64::new(0),
    });

    let tree = scanner::scan_directory(&path, progress.clone());

    let files = progress.file_count.load(Ordering::Relaxed);
    let size = progress.total_size.load(Ordering::Relaxed);
    println!(
        "{} files, {} bytes ({})",
        files,
        size,
        bytesize::ByteSize::b(size)
    );
    std::hint::black_box(tree);
}
