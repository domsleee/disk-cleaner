//! Scanning benchmarks — disk I/O, tree construction, memory allocation.
//!
//! Covers: synthetic scans, real directory scans, hidden-flag detection,
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

/// Benchmark scanning a small synthetic tree (100 dirs, 1000 files)
fn bench_scan_synthetic(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    // Create 100 dirs with 10 files each
    for i in 0..100 {
        let dir = tmp.path().join(format!("dir_{i:03}"));
        fs::create_dir(&dir).unwrap();
        for j in 0..10 {
            fs::write(dir.join(format!("file_{j}.bin")), vec![0u8; 1024]).unwrap();
        }
    }

    c.bench_function("scan_1000_files_100_dirs", |b| {
        b.iter(|| {
            let progress = new_progress();
            scanner::scan_directory(tmp.path(), progress)
        })
    });
}

/// Benchmark scanning a deep tree (10 levels, ~1000 nodes)
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

/// Benchmark scanning a large synthetic tree (10,000 files / 500 dirs)
fn bench_scan_large_synthetic(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..500 {
        let dir = tmp.path().join(format!("dir_{i:04}"));
        fs::create_dir(&dir).unwrap();
        for j in 0..20 {
            fs::write(dir.join(format!("file_{j}.dat")), vec![0u8; 4096]).unwrap();
        }
    }

    c.bench_function("scan_10000_files_500_dirs", |b| {
        b.iter(|| {
            let progress = new_progress();
            scanner::scan_directory(tmp.path(), progress)
        })
    });
}

/// Benchmark scanning a 20k-file tree to measure hidden-detection overhead.
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

/// Benchmark scanning the project's own directory (real-world data)
fn bench_scan_self(c: &mut Criterion) {
    let project_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    c.bench_function("scan_project_dir", |b| {
        b.iter(|| {
            let progress = new_progress();
            scanner::scan_directory(project_dir, progress)
        })
    });
}

/// Benchmark scanning ~/git (real-world, large directory)
fn bench_scan_home_git(c: &mut Criterion) {
    let home_git = dirs::home_dir()
        .map(|h| h.join("git"))
        .filter(|p| p.is_dir());

    if let Some(git_dir) = home_git {
        c.bench_function("scan_home_git", |b| {
            b.iter(|| {
                let progress = new_progress();
                scanner::scan_directory(&git_dir, progress)
            })
        });
    } else {
        eprintln!("Skipping ~/git benchmark: directory not found");
    }
}

// ---------------------------------------------------------------------------
// Hidden flag + directory-heavy benchmarks (scan shape variants)
// ---------------------------------------------------------------------------

/// Benchmark the hidden-flag detection path on macOS.
///
/// Creates N non-dot files in a flat directory. On macOS, each file triggers
/// an `lstat` call in `is_os_hidden()` to check the UF_HIDDEN flag.
fn bench_hidden_flag_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("hidden_flag_lookup");
    for count in [500, 2000, 5000] {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..count {
            fs::write(
                tmp.path().join(format!("file_{i:05}.dat")),
                vec![0u8; 64],
            )
            .unwrap();
        }

        group.bench_with_input(
            BenchmarkId::new("non_dot_files", count),
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

/// Benchmark many small directories (each with a few files).
fn bench_many_small_dirs(c: &mut Criterion) {
    let mut group = c.benchmark_group("dir_heavy");
    for count in [200, 1000, 3000] {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..count {
            let dir = tmp.path().join(format!("dir_{i:05}"));
            fs::create_dir(&dir).unwrap();
            for j in 0..3 {
                fs::write(dir.join(format!("f{j}.bin")), vec![0u8; 128]).unwrap();
            }
        }

        group.bench_with_input(
            BenchmarkId::new("small_dirs_3_files", count),
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
    bench_scan_synthetic,
    bench_scan_deep,
    bench_scan_large_synthetic,
    bench_scan_20k_files,
    // Real directory scans
    bench_scan_self,
    bench_scan_home_git,
    // Scan shape variants
    bench_hidden_flag_lookup,
    bench_many_empty_dirs,
    bench_many_small_dirs,
    // Memory allocation
    bench_memory_synthetic,
    bench_memory_large_synthetic,
);
criterion_main!(benches);
