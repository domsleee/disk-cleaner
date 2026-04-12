//! Scanning benchmarks — disk I/O, tree construction, memory allocation.
//!
//! Covers: synthetic scans (deep + 20k-file), real directory scans,
//! directory-heavy layouts, and memory-per-node tracking.
//!
//! ```sh
//! cargo bench --bench scan_bench
//! ```

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use disk_cleaner::scanner::{self, ScanProgress};
use disk_cleaner::tree::FileNode;
use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Tracking allocator (for memory benchmarks)
// ---------------------------------------------------------------------------

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

fn reset_tracking() {
    ALLOCATED.store(0, Ordering::SeqCst);
    PEAK.store(0, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Synthetic scan benchmarks
// ---------------------------------------------------------------------------

/// Benchmark scanning a deep tree (10 levels, ~100 nodes).
/// Tests recursive descent overhead separately from file volume.
fn bench_scan_deep(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    let mut path = tmp.path().to_path_buf();
    for i in 0..10 {
        path = path.join(format!("level_{i}"));
        fs::create_dir(&path).unwrap();
        for j in 0..10 {
            fs::write(path.join(format!("file_{j}.dat")), vec![0u8; 512]).unwrap();
        }
    }

    c.bench_function("scan_deep_10_levels", |b| {
        b.iter(|| {
            let progress = new_progress();
            scanner::scan_directory(tmp.path(), progress)
        })
    });
}

/// Benchmark scanning a 20k-file tree to measure hidden-detection overhead.
/// Primary synthetic scan benchmark — large enough to surface real costs.
fn bench_scan_20k_files(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..200 {
        let dir = tmp.path().join(format!("dir_{i:04}"));
        fs::create_dir(&dir).unwrap();
        for j in 0..100 {
            fs::write(dir.join(format!("file_{j:03}.dat")), vec![0u8; 256]).unwrap();
        }
    }

    c.bench_function("scan_20k_files_200_dirs", |b| {
        b.iter(|| {
            let progress = new_progress();
            scanner::scan_directory(tmp.path(), progress)
        })
    });
}

// ---------------------------------------------------------------------------
// Real directory scan benchmarks
// ---------------------------------------------------------------------------

/// Benchmark scanning real directories (project dir, ~/git, ~).
/// Low sample count — these are I/O-bound and take seconds per iteration.
fn bench_scan_real_dirs(c: &mut Criterion) {
    let mut group = c.benchmark_group("scan_real");
    group.sample_size(10);

    let project_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    group.bench_function("project_dir", |b| {
        b.iter(|| {
            let progress = new_progress();
            scanner::scan_directory(project_dir, progress)
        })
    });

    let home_git = dirs::home_dir()
        .map(|h| h.join("git"))
        .filter(|p| p.is_dir());
    if let Some(git_dir) = home_git {
        group.bench_function("home_git", |b| {
            b.iter(|| {
                let progress = new_progress();
                scanner::scan_directory(&git_dir, progress)
            })
        });
    } else {
        eprintln!("Skipping ~/git benchmark: directory not found");
    }

    if let Some(home) = dirs::home_dir().filter(|p| p.is_dir()) {
        group.bench_function("home", |b| {
            b.iter(|| {
                let progress = new_progress();
                scanner::scan_directory(&home, progress)
            })
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Directory-heavy benchmarks (scan shape variant)
// ---------------------------------------------------------------------------

/// Benchmark many empty directories to isolate per-directory overhead.
fn bench_many_empty_dirs(c: &mut Criterion) {
    let mut group = c.benchmark_group("dir_heavy");
    for count in [500, 2000, 5000] {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..count {
            fs::create_dir(tmp.path().join(format!("dir_{i:05}"))).unwrap();
        }

        group.bench_with_input(
            BenchmarkId::new("empty_dirs", count),
            &count,
            |b, _| {
                b.iter(|| {
                    let progress = new_progress();
                    scanner::scan_directory(tmp.path(), progress)
                })
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Memory allocation benchmarks (scan + track bytes/node)
// ---------------------------------------------------------------------------

/// Measure memory for building a synthetic tree (1000 files, 100 dirs)
fn bench_memory_synthetic(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..100 {
        let dir = tmp.path().join(format!("dir_{i:03}"));
        fs::create_dir(&dir).unwrap();
        for j in 0..10 {
            fs::write(dir.join(format!("file_{j}.bin")), vec![0u8; 1024]).unwrap();
        }
    }

    c.bench_function("memory_1000_files_100_dirs", |b| {
        b.iter(|| {
            let before = ALLOCATED.load(Ordering::SeqCst);
            let progress = new_progress();
            let tree = scanner::scan_directory(tmp.path(), progress);
            let after = ALLOCATED.load(Ordering::SeqCst);
            let nodes = count_nodes(&tree);
            std::hint::black_box((after.saturating_sub(before), nodes));
            tree
        })
    });

    // Print a one-time memory report
    reset_tracking();
    let progress = new_progress();
    let before = ALLOCATED.load(Ordering::SeqCst);
    let tree = scanner::scan_directory(tmp.path(), progress.clone());
    let after = ALLOCATED.load(Ordering::SeqCst);
    let nodes = count_nodes(&tree);
    let files = progress.file_count.load(Ordering::Relaxed);
    eprintln!("\n=== Memory Report: 1000 files / 100 dirs ===");
    eprintln!("Nodes: {nodes}");
    eprintln!("Files scanned: {files}");
    eprintln!(
        "Memory delta: {} bytes ({:.1} KB)",
        after.saturating_sub(before),
        after.saturating_sub(before) as f64 / 1024.0
    );
    eprintln!(
        "Bytes per node: {:.0}",
        after.saturating_sub(before) as f64 / nodes as f64
    );
    eprintln!(
        "Peak allocation: {} bytes ({:.1} KB)",
        PEAK.load(Ordering::SeqCst),
        PEAK.load(Ordering::SeqCst) as f64 / 1024.0
    );
    eprintln!("=============================================\n");
    std::hint::black_box(tree);
}

/// Memory benchmark for a large synthetic tree (10,000 files / 500 dirs)
fn bench_memory_large_synthetic(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..500 {
        let dir = tmp.path().join(format!("dir_{i:04}"));
        fs::create_dir(&dir).unwrap();
        for j in 0..20 {
            fs::write(dir.join(format!("file_{j}.dat")), vec![0u8; 4096]).unwrap();
        }
    }

    c.bench_function("memory_10000_files_500_dirs", |b| {
        b.iter(|| {
            let progress = new_progress();
            scanner::scan_directory(tmp.path(), progress)
        })
    });

    // Print memory report for large tree
    reset_tracking();
    let progress = new_progress();
    let before = ALLOCATED.load(Ordering::SeqCst);
    let tree = scanner::scan_directory(tmp.path(), progress.clone());
    let after = ALLOCATED.load(Ordering::SeqCst);
    let nodes = count_nodes(&tree);
    let files = progress.file_count.load(Ordering::Relaxed);
    eprintln!("\n=== Memory Report: 10,000 files / 500 dirs ===");
    eprintln!("Nodes: {nodes}");
    eprintln!("Files scanned: {files}");
    eprintln!(
        "Memory delta: {} bytes ({:.1} KB)",
        after.saturating_sub(before),
        after.saturating_sub(before) as f64 / 1024.0
    );
    eprintln!(
        "Bytes per node: {:.0}",
        after.saturating_sub(before) as f64 / nodes as f64
    );
    eprintln!(
        "Peak allocation: {} bytes ({:.1} KB)",
        PEAK.load(Ordering::SeqCst),
        PEAK.load(Ordering::SeqCst) as f64 / 1024.0
    );
    eprintln!("================================================\n");
    std::hint::black_box(tree);
}

// ---------------------------------------------------------------------------
// Criterion groups
// ---------------------------------------------------------------------------

criterion_group!(
    benches,
    // Synthetic scans
    bench_scan_deep,
    bench_scan_20k_files,
    // Real directory scans
    bench_scan_real_dirs,
    // Scan shape variants
    bench_many_empty_dirs,
    // Memory allocation
    bench_memory_synthetic,
    bench_memory_large_synthetic,
);
criterion_main!(benches);
