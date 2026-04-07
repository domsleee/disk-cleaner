use criterion::{criterion_group, criterion_main, Criterion};
use disk_cleaner::arena_scanner;
use disk_cleaner::scanner::{self, ScanProgress};
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

fn new_progress() -> Arc<ScanProgress> {
    Arc::new(ScanProgress {
        file_count: AtomicU64::new(0),
        total_size: AtomicU64::new(0),
        cancelled: AtomicBool::new(false),
    })
}

/// Create a synthetic tree with `num_dirs` directories, each containing
/// `files_per_dir` files. Returns the temp dir handle (must stay alive).
fn create_tree(num_dirs: usize, files_per_dir: usize) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..num_dirs {
        let dir = tmp.path().join(format!("dir_{i:05}"));
        fs::create_dir(&dir).unwrap();
        for j in 0..files_per_dir {
            fs::write(dir.join(format!("file_{j:04}.dat")), vec![0u8; 256]).unwrap();
        }
    }
    tmp
}

/// Benchmark: standard parallel scan (baseline) — 100K files / 1K dirs
fn bench_baseline_100k(c: &mut Criterion) {
    let tmp = create_tree(1_000, 100);
    c.bench_function("scan_100k_baseline_parallel", |b| {
        b.iter(|| {
            let progress = new_progress();
            scanner::scan_directory(tmp.path(), progress)
        })
    });
}

/// Benchmark: arena single-threaded scan — 100K files / 1K dirs
fn bench_arena_100k(c: &mut Criterion) {
    let tmp = create_tree(1_000, 100);
    c.bench_function("scan_100k_arena_singlethread", |b| {
        b.iter(|| {
            let progress = new_progress();
            arena_scanner::arena_scan_directory(tmp.path(), progress)
        })
    });
}

/// Benchmark: standard parallel scan (baseline) — 20K files / 200 dirs
fn bench_baseline_20k(c: &mut Criterion) {
    let tmp = create_tree(200, 100);
    c.bench_function("scan_20k_baseline_parallel", |b| {
        b.iter(|| {
            let progress = new_progress();
            scanner::scan_directory(tmp.path(), progress)
        })
    });
}

/// Benchmark: arena single-threaded scan — 20K files / 200 dirs
fn bench_arena_20k(c: &mut Criterion) {
    let tmp = create_tree(200, 100);
    c.bench_function("scan_20k_arena_singlethread", |b| {
        b.iter(|| {
            let progress = new_progress();
            arena_scanner::arena_scan_directory(tmp.path(), progress)
        })
    });
}

criterion_group!(
    benches,
    bench_baseline_20k,
    bench_arena_20k,
    bench_baseline_100k,
    bench_arena_100k,
);
criterion_main!(benches);
