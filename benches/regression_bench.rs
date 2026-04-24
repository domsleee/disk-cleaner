//! Regression benchmarks for scan speed and peak memory.
//!
//! These benchmarks use a **fixed, reproducible synthetic fixture** (50,000 files
//! across 500 directories) so that results are comparable across machines, commits,
//! and CI runs.
//!
//! # Running
//!
//! ```sh
//! # Run regression benchmarks only:
//! cargo bench --bench regression_bench
//!
//! # Save a baseline (e.g. on main before changes):
//! cargo bench --bench regression_bench -- --save-baseline main
//!
//! # Compare against the saved baseline:
//! cargo bench --bench regression_bench -- --baseline main
//! ```
//!
//! # Interpreting results
//!
//! Criterion reports statistical comparisons automatically when a baseline exists.
//! Look for `regressed` / `improved` / `no change` in the output.
//!
//! The one-shot memory report prints after the criterion benchmarks and shows:
//! - **Bytes/node**: heap bytes per tree node — the primary memory efficiency metric.
//!   This is independent of fixture size and comparable across runs.
//! - **Peak alloc**: high-water mark of heap usage during a single scan.
//! - **Scan time**: wall-clock time for a single scan (not statistically averaged).
//!
//! # Regression thresholds
//!
//! | Metric       | Baseline (M2) | Threshold | Rationale                       |
//! |--------------|---------------|-----------|---------------------------------|
//! | Bytes/node   | ~68           | ≤ 200     | ~3x headroom over measured      |
//! | Scan time    | ~58 ms        | ≤ 500 ms  | ~8x headroom for slower CI HW   |
//!
//! If the one-shot report shows **REGRESSION** for either metric, investigate
//! before merging. The thresholds are intentionally generous to avoid flaky CI
//! failures while still catching large regressions.
//!
//! # Fixture details
//!
//! - 500 directories (`dir_0000` .. `dir_0499`), flat under root
//! - 100 files per directory (`file_000.dat` .. `file_099.dat`)
//! - Each file: 1,024 bytes of zeroes
//! - Total: 50,000 files, 500 dirs, ~50 MB on disk
//! - Total tree nodes: 50,501 (50,000 files + 500 dirs + 1 root)

use criterion::{Criterion, criterion_group, criterion_main};
use disk_cleaner::scanner::{self, ScanProgress};
use disk_cleaner::tree::FileNode;
use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

// ── Tracking allocator ───────────────────────────────────────────────

struct TrackingAllocator;

static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let current = ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(current, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        ALLOCATED.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static ALLOC: TrackingAllocator = TrackingAllocator;

fn reset_peak() {
    PEAK.store(ALLOCATED.load(Ordering::SeqCst), Ordering::SeqCst);
}

// ── Helpers ──────────────────────────────────────────────────────────

fn new_progress() -> Arc<ScanProgress> {
    Arc::new(ScanProgress {
        file_count: AtomicU64::new(0),
        total_size: AtomicU64::new(0),
        cancelled: AtomicBool::new(false),
    })
}

fn count_nodes(node: &FileNode) -> usize {
    1 + node.children().iter().map(count_nodes).sum::<usize>()
}

// ── Fixture ──────────────────────────────────────────────────────────

const DIRS: usize = 500;
const FILES_PER_DIR: usize = 100;
const FILE_SIZE: usize = 1024;

/// Create the standard regression fixture: 50,000 files across 500 directories.
fn create_fixture(root: &std::path::Path) {
    for i in 0..DIRS {
        let dir = root.join(format!("dir_{i:04}"));
        fs::create_dir(&dir).unwrap();
        for j in 0..FILES_PER_DIR {
            fs::write(dir.join(format!("file_{j:03}.dat")), vec![0u8; FILE_SIZE]).unwrap();
        }
    }
}

// ── Regression thresholds ────────────────────────────────────────────

/// Maximum acceptable bytes per tree node before flagging a regression.
/// Baseline on Apple M2: ~68 bytes/node. Threshold set at ~3x headroom.
const MAX_BYTES_PER_NODE: f64 = 200.0;

/// Maximum acceptable scan time (ms) before flagging a regression.
/// Baseline on Apple M2: ~58 ms. Threshold set at ~8x for slower CI hardware.
const MAX_SCAN_TIME_MS: f64 = 500.0;

// ── Criterion benchmarks ─────────────────────────────────────────────

/// Scan speed regression benchmark (50K files, criterion-measured).
fn bench_regression_scan_speed(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    create_fixture(tmp.path());

    let mut group = c.benchmark_group("regression");
    group.sample_size(20);

    group.bench_function("scan_50k_files", |b| {
        b.iter(|| {
            let progress = new_progress();
            scanner::scan_directory(tmp.path(), progress)
        })
    });

    group.finish();
}

/// Peak memory regression benchmark (50K files, criterion-measured scan
/// with a one-shot memory report printed at the end).
fn bench_regression_memory(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    create_fixture(tmp.path());

    let mut group = c.benchmark_group("regression");
    group.sample_size(20);

    group.bench_function("memory_50k_files", |b| {
        b.iter(|| {
            let progress = new_progress();
            scanner::scan_directory(tmp.path(), progress)
        })
    });

    group.finish();

    // ── One-shot memory + speed report ──────────────────────────────
    // Run a single scan outside criterion to capture accurate peak memory.
    reset_peak();
    let before = ALLOCATED.load(Ordering::SeqCst);
    let progress = new_progress();
    let start = Instant::now();
    let tree = scanner::scan_directory(tmp.path(), progress.clone());
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
    let after = ALLOCATED.load(Ordering::SeqCst);
    let peak = PEAK.load(Ordering::SeqCst);

    let files = progress.file_count.load(Ordering::Relaxed);
    let total_size = progress.total_size.load(Ordering::Relaxed);
    let delta = after.saturating_sub(before);
    let nodes = count_nodes(&tree);
    let bytes_per_node = delta as f64 / nodes as f64;

    let mem_status = if bytes_per_node <= MAX_BYTES_PER_NODE {
        "OK"
    } else {
        "REGRESSION"
    };
    let speed_status = if elapsed_ms <= MAX_SCAN_TIME_MS {
        "OK"
    } else {
        "REGRESSION"
    };

    eprintln!();
    eprintln!("=== Regression Report: 50K files / 500 dirs ===");
    eprintln!("Files scanned : {files}");
    eprintln!("Tree nodes    : {nodes}");
    eprintln!("Total size    : {:.1} MB", total_size as f64 / 1_048_576.0);
    eprintln!(
        "Scan time     : {elapsed_ms:.1} ms  [{speed_status}] (threshold: {MAX_SCAN_TIME_MS} ms)"
    );
    eprintln!("Memory delta  : {:.1} MB", delta as f64 / 1_048_576.0);
    eprintln!("Peak alloc    : {:.1} MB", peak as f64 / 1_048_576.0);
    eprintln!(
        "Bytes/node    : {bytes_per_node:.0}  [{mem_status}] (threshold: {MAX_BYTES_PER_NODE})"
    );
    eprintln!("=================================================");
    eprintln!();

    std::hint::black_box(tree);
}

criterion_group!(
    benches,
    bench_regression_scan_speed,
    bench_regression_memory,
);
criterion_main!(benches);
