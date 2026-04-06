//! "Big bench" — scans a real directory (default: ~) and measures scan time,
//! collect_cached_rows time, peak RSS, and file count.
//!
//! Usage: big_bench [PATH]
//!   Defaults to $HOME.

use disk_cleaner::scanner::{self, ScanProgress};
use disk_cleaner::tree;
use disk_cleaner::ui;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Expand all directories down to `max_depth` so the benchmark exercises a
/// realistic number of visible rows.
fn expand_to_depth(node: &mut tree::FileNode, depth: usize, max_depth: usize) {
    if depth >= max_depth {
        return;
    }
    node.set_expanded(true);
    if let Some(d) = node.as_dir_mut() {
        for child in &mut d.children {
            if child.is_dir() {
                expand_to_depth(child, depth + 1, max_depth);
            }
        }
    }
}

fn peak_rss_bytes() -> u64 {
    #[cfg(target_os = "macos")]
    {
        use std::mem::MaybeUninit;
        unsafe {
            let mut info = MaybeUninit::<libc::rusage>::uninit();
            if libc::getrusage(libc::RUSAGE_SELF, info.as_mut_ptr()) == 0 {
                // macOS reports maxrss in bytes
                return info.assume_init().ru_maxrss as u64;
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        use std::mem::MaybeUninit;
        unsafe {
            let mut info = MaybeUninit::<libc::rusage>::uninit();
            if libc::getrusage(libc::RUSAGE_SELF, info.as_mut_ptr()) == 0 {
                // Linux reports maxrss in KB
                return info.assume_init().ru_maxrss as u64 * 1024;
            }
        }
    }
    0
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().expect("cannot determine home directory"));

    if !path.is_dir() {
        eprintln!("Error: not a directory: {}", path.display());
        std::process::exit(1);
    }

    eprintln!("=== Big Bench ===");
    eprintln!("Target: {}", path.display());

    // Phase 1: scan
    let progress = Arc::new(ScanProgress {
        file_count: AtomicU64::new(0),
        total_size: AtomicU64::new(0),
        cancelled: AtomicBool::new(false),
    });

    let scan_start = Instant::now();
    let tree = scanner::scan_directory(&path, progress.clone());
    let scan_elapsed = scan_start.elapsed();

    let file_count = progress.file_count.load(Ordering::Relaxed);
    let total_size = progress.total_size.load(Ordering::Relaxed);

    eprintln!(
        "Scan: {file_count} files, {} in {scan_elapsed:.2?}",
        bytesize::ByteSize::b(total_size)
    );

    let rss_after_scan = peak_rss_bytes();

    // Phase 2: expand top 3 levels so we exercise a realistic row count
    let mut tree = tree;
    expand_to_depth(&mut tree, 0, 3);

    let expanded_groups: HashSet<PathBuf> = HashSet::new();
    let iterations = 10;

    // Warm-up: one throw-away call to prime caches / page in memory
    let mut warmup_buf: Vec<ui::CachedRow> = Vec::new();
    ui::collect_cached_rows_into(
        &mut warmup_buf,
        &tree,
        "",
        None,
        true,
        None,
        None,
        Some(&expanded_groups),
    );
    let row_count = warmup_buf.len();
    std::hint::black_box(&warmup_buf);

    eprintln!("Visible rows (3-level expand): {row_count}");

    // Phase 3a: fresh Vec each call (baseline)
    let mut fresh_times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        let mut buf: Vec<ui::CachedRow> = Vec::new();
        ui::collect_cached_rows_into(
            &mut buf,
            &tree,
            "",
            None,
            true,
            None,
            None,
            Some(&expanded_groups),
        );
        fresh_times.push(start.elapsed());
        std::hint::black_box(&buf);
    }
    fresh_times.sort();

    // Phase 3b: reuse Vec across calls (collect_cached_rows_into)
    let mut reuse_buf: Vec<ui::CachedRow> = Vec::new();
    // prime the buffer
    ui::collect_cached_rows_into(
        &mut reuse_buf,
        &tree,
        "",
        None,
        true,
        None,
        None,
        Some(&expanded_groups),
    );
    let mut reuse_times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        ui::collect_cached_rows_into(
            &mut reuse_buf,
            &tree,
            "",
            None,
            true,
            None,
            None,
            Some(&expanded_groups),
        );
        reuse_times.push(start.elapsed());
        std::hint::black_box(&reuse_buf);
    }
    reuse_times.sort();

    let rss_after_rows = peak_rss_bytes();

    let fresh_median = fresh_times[iterations / 2];
    let reuse_median = reuse_times[iterations / 2];

    eprintln!(
        "collect_cached_rows  fresh ({iterations} iters, {row_count} rows): median={:.2?} min={:.2?} max={:.2?}",
        fresh_median, fresh_times[0], fresh_times[iterations - 1]
    );
    eprintln!(
        "collect_cached_rows  reuse ({iterations} iters, {row_count} rows): median={:.2?} min={:.2?} max={:.2?}",
        reuse_median, reuse_times[0], reuse_times[iterations - 1]
    );
    eprintln!(
        "Peak RSS after scan:  {:.1} MB",
        rss_after_scan as f64 / 1_048_576.0
    );
    eprintln!(
        "Peak RSS after rows:  {:.1} MB",
        rss_after_rows as f64 / 1_048_576.0
    );

    // Machine-readable output for PR descriptions
    println!("file_count={file_count}");
    println!("total_size_bytes={total_size}");
    println!("visible_rows={row_count}");
    println!("scan_ms={:.1}", scan_elapsed.as_secs_f64() * 1000.0);
    println!(
        "collect_rows_fresh_median_ms={:.3}",
        fresh_median.as_secs_f64() * 1000.0
    );
    println!(
        "collect_rows_reuse_median_ms={:.3}",
        reuse_median.as_secs_f64() * 1000.0
    );
    println!("peak_rss_mb={:.1}", rss_after_rows as f64 / 1_048_576.0);
}
