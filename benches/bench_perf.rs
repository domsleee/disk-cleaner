use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
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

/// Benchmark the hidden-flag detection path on macOS.
///
/// Creates N non-dot files (names like `file_0000.dat`) in a flat directory.
/// On macOS, each file triggers an `lstat` call in `is_os_hidden()` to check
/// the UF_HIDDEN flag. On other platforms, only the `starts_with('.')` check
/// runs, serving as a baseline.
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
///
/// The `walk_dir` function allocates a Vec, calls `read_dir`, sets up
/// `par_bridge`, and builds a `DirNode` for each directory even if empty.
/// This benchmark amplifies that cost by creating a flat tree of N empty dirs.
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
///
/// Similar to the empty-dirs case but each directory also contains 3 small
/// files, exercising both the directory overhead and file-processing path.
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

criterion_group!(
    benches,
    bench_hidden_flag_lookup,
    bench_many_empty_dirs,
    bench_many_small_dirs,
);
criterion_main!(benches);
