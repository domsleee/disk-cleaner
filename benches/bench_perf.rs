use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use disk_cleaner::scanner::{self, ScanProgress};
use disk_cleaner::tree::{DirNode, FileLeaf, FileNode};
use disk_cleaner::ui;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
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

/// Build a synthetic tree with `num_dirs` directories, each containing
/// `files_per_dir` files. Some file names contain "main" to match a filter.
fn make_large_tree(num_dirs: usize, files_per_dir: usize) -> FileNode {
    let mut children = Vec::with_capacity(num_dirs);
    for d in 0..num_dirs {
        let mut files = Vec::with_capacity(files_per_dir);
        for f in 0..files_per_dir {
            let name = if f % 10 == 0 {
                format!("main_{f:05}.rs")
            } else {
                format!("file_{f:05}.dat")
            };
            files.push(FileNode::File(FileLeaf {
                name: name.into(),
                size: 1024,
                hidden: false,
            }));
        }
        children.push(FileNode::Dir(Box::new(DirNode {
            name: format!("dir_{d:05}").into(),
            size: files_per_dir as u64 * 1024,
            children: files,
            expanded: true,
            hidden: false,
        })));
    }
    FileNode::Dir(Box::new(DirNode {
        name: "root".into(),
        size: num_dirs as u64 * files_per_dir as u64 * 1024,
        children,
        expanded: true,
        hidden: false,
    }))
}

/// Benchmark building text-match cache from scratch vs reusing a pre-built
/// cache when the filter string hasn't changed.
///
/// "cold" = build_text_match_cache + collect_cached_rows (what happened before).
/// "warm" = collect_cached_rows only, reusing the cached HashSet (after this PR).
fn bench_filter_cache_reuse(c: &mut Criterion) {
    let mut group = c.benchmark_group("filter_cache");
    // 500 dirs × 100 files = 50K nodes
    let tree = make_large_tree(500, 100);
    let query = "main";
    let expanded: HashSet<PathBuf> = HashSet::new();

    // Pre-build the cache once for the "warm" benchmarks.
    let text_cache = ui::build_text_match_cache(&tree, query);

    group.bench_function("cold_build_50k", |b| {
        b.iter(|| {
            let cache = ui::build_text_match_cache(&tree, query);
            ui::collect_cached_rows(
                &tree,
                query,
                None,
                false,
                Some(&cache),
                None,
                Some(&expanded),
            )
        })
    });

    group.bench_function("warm_reuse_50k", |b| {
        b.iter(|| {
            ui::collect_cached_rows(
                &tree,
                query,
                None,
                false,
                Some(&text_cache),
                None,
                Some(&expanded),
            )
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_hidden_flag_lookup,
    bench_many_empty_dirs,
    bench_many_small_dirs,
    bench_filter_cache_reuse,
);
criterion_main!(benches);
