//! Scanning benchmarks — disk I/O, tree construction, memory allocation.
//!
//! Covers: synthetic scans (deep + 20k-file), real directory scans,
//! directory-heavy layouts, and memory-per-node tracking.
//!
//! ```sh
//! cargo bench --bench scan_bench
//! MEMORY_REPORT=1 cargo bench --bench scan_bench   # also print memory breakdowns
//! ```

#[path = "common/alloc.rs"]
mod alloc;

use alloc::{ALLOCATED, PEAK, reset_tracking};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use disk_cleaner::categories;
use disk_cleaner::scanner::{self, ScanProgress};
use disk_cleaner::tree::FileNode;
use disk_cleaner::treemap;
use disk_cleaner::ui;
use eframe::egui;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_progress() -> Arc<ScanProgress> {
    Arc::new(ScanProgress {
        file_count: AtomicU64::new(0),
        total_size: AtomicU64::new(0),
        fallback_count: AtomicU64::new(0),
        access_denied_fallback_count: AtomicU64::new(0),
        bulk_scan_fallback_count: AtomicU64::new(0),
        fallback_details: std::sync::Mutex::new(Vec::new()),
        cancelled: AtomicBool::new(false),
        seen_inodes: Default::default(),
        mft_used: AtomicBool::new(false),
        mft_elevation_hint: AtomicBool::new(false),
    })
}

fn count_nodes(node: &FileNode) -> usize {
    1 + node.children().iter().map(count_nodes).sum::<usize>()
}

fn count_treemap_tiles(cache: &treemap::TreemapCache) -> usize {
    cache.tiles.len()
        + cache.other.iter().count()
        + cache
            .tiles
            .iter()
            .map(|tile| tile.nested.len())
            .sum::<usize>()
}

fn measure_retained<T>(f: impl FnOnce() -> T) -> (T, usize, usize) {
    let before = ALLOCATED.load(Ordering::SeqCst);
    PEAK.store(before, Ordering::SeqCst);
    let value = f();
    let after = ALLOCATED.load(Ordering::SeqCst);
    let peak = PEAK.load(Ordering::SeqCst);
    (
        value,
        after.saturating_sub(before),
        peak.saturating_sub(before),
    )
}

fn print_memory_line(
    label: &str,
    delta: usize,
    peak: usize,
    node_count: usize,
    extra: impl std::fmt::Display,
) {
    eprintln!(
        "    {label:22} | {:>10} delta | {:>10} peak | {:.0} b/node | {extra}",
        bytesize::ByteSize::b(delta as u64),
        bytesize::ByteSize::b(peak as u64),
        delta as f64 / node_count as f64,
    );
}

/// Bytes-per-unit figure for a structure's own unit (row, path, tile),
/// alongside the cross-structure b/node column.
fn per_unit(delta: usize, units: usize) -> f64 {
    delta as f64 / units.max(1) as f64
}

/// Peak resident memory of the process from the OS — the reality anchor for
/// the requested-bytes numbers (which exclude allocator rounding/metadata,
/// thread stacks, and code). Process-lifetime peak, so it also covers the
/// criterion iterations that ran before this report.
fn peak_rss_bytes() -> Option<u64> {
    #[cfg(windows)]
    unsafe {
        #[repr(C)]
        struct ProcessMemoryCounters {
            cb: u32,
            page_fault_count: u32,
            peak_working_set_size: usize,
            working_set_size: usize,
            quota_peak_paged_pool_usage: usize,
            quota_paged_pool_usage: usize,
            quota_peak_non_paged_pool_usage: usize,
            quota_non_paged_pool_usage: usize,
            pagefile_usage: usize,
            peak_pagefile_usage: usize,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn K32GetProcessMemoryInfo(
                process: isize,
                counters: *mut ProcessMemoryCounters,
                cb: u32,
            ) -> i32;
            fn GetCurrentProcess() -> isize;
        }
        let mut counters: ProcessMemoryCounters = std::mem::zeroed();
        counters.cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;
        (K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) != 0)
            .then_some(counters.peak_working_set_size as u64)
    }
    #[cfg(unix)]
    unsafe {
        let mut ru: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut ru) != 0 {
            return None;
        }
        // ru_maxrss is bytes on macOS, kilobytes on Linux.
        let raw = ru.ru_maxrss as u64;
        Some(if cfg!(target_os = "macos") {
            raw
        } else {
            raw * 1024
        })
    }
    #[cfg(not(any(windows, unix)))]
    None
}

fn print_real_scan_breakdown(label: &str, path: &std::path::Path) {
    // Warm the rayon pool and per-thread scanner buffers on a throwaway scan
    // so their one-time allocations don't land in the first measured row.
    {
        let warmup = tempfile::tempdir().unwrap();
        for i in 0..64 {
            let dir = warmup.path().join(format!("w{i}"));
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("f.bin"), [0u8; 64]).unwrap();
        }
        std::hint::black_box(scanner::scan_directory(warmup.path(), new_progress()));
    }
    reset_tracking();
    let baseline = ALLOCATED.load(Ordering::SeqCst);

    let progress = new_progress();
    let (mut tree, tree_delta, tree_peak) =
        measure_retained(|| scanner::scan_directory(path, progress.clone()));
    let node_count = count_nodes(&tree);
    let file_count = progress.file_count.load(Ordering::Relaxed);
    let scan_size = progress.total_size.load(Ordering::Relaxed);
    let fallbacks = progress.fallback_count.load(Ordering::Relaxed);

    disk_cleaner::tree::auto_expand(&mut tree, 0, 2);

    eprintln!(
        "  {label:8} | {file_count:>9} files | {node_count:>9} nodes | scanned {}{}",
        bytesize::ByteSize::b(scan_size),
        if fallbacks > 0 {
            format!(" | {fallbacks} fallback dirs")
        } else {
            String::new()
        },
    );
    eprintln!(
        "    (requested bytes, not RSS; delta = retained after build; peak = extra live during \
         build, process-wide)"
    );
    print_memory_line(
        "scanner tree",
        tree_delta,
        tree_peak,
        node_count,
        format_args!(
            "{file_count} files{}",
            if cfg!(windows) {
                " (peak includes transient hardlink-dedup set)"
            } else {
                ""
            }
        ),
    );

    let expanded_file_groups: HashSet<PathBuf> = HashSet::new();
    let (rows, rows_delta, rows_peak) = measure_retained(|| {
        ui::collect_cached_rows(
            &tree,
            "",
            None,
            true,
            None,
            None,
            Some(&expanded_file_groups),
        )
    });
    print_memory_line(
        "row cache (unfiltered)",
        rows_delta,
        rows_peak,
        node_count,
        format_args!(
            "{} rows, {:.0} b/row",
            rows.len(),
            per_unit(rows_delta, rows.len())
        ),
    );
    drop(rows);

    // The app retains category stats after every scan, so measure it as part
    // of the retained set (and reuse it to pick the largest category below).
    let (stats, stats_delta, stats_peak) = measure_retained(|| categories::compute_stats(&tree));
    print_memory_line(
        "category stats",
        stats_delta,
        stats_peak,
        node_count,
        format_args!("{} categories", stats.entries.len()),
    );

    if let Some((cat, _, _)) = stats.entries.first().copied() {
        let (cat_cache, cache_delta, cache_peak) =
            measure_retained(|| ui::build_category_match_cache(&tree, cat));
        print_memory_line(
            &format!("category cache ({})", cat.label()),
            cache_delta,
            cache_peak,
            node_count,
            format_args!(
                "{} cached paths, {:.0} b/path",
                cat_cache.len(),
                per_unit(cache_delta, cat_cache.len())
            ),
        );

        let (filtered_rows, filtered_rows_delta, filtered_rows_peak) = measure_retained(|| {
            ui::collect_cached_rows(
                &tree,
                "",
                Some(cat),
                true,
                None,
                Some(&cat_cache),
                Some(&expanded_file_groups),
            )
        });
        print_memory_line(
            &format!("rows ({})", cat.label()),
            filtered_rows_delta,
            filtered_rows_peak,
            node_count,
            format_args!(
                "{} rows, {:.0} b/row",
                filtered_rows.len(),
                per_unit(filtered_rows_delta, filtered_rows.len())
            ),
        );
        drop(filtered_rows);
        drop(cat_cache);
    }

    let full_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0));
    let (treemap_cache, treemap_delta, treemap_peak) =
        measure_retained(|| treemap::build_treemap_cache(&tree, &None, None, true, full_rect));
    let tile_count = count_treemap_tiles(&treemap_cache);
    print_memory_line(
        "treemap cache",
        treemap_delta,
        treemap_peak,
        node_count,
        format_args!(
            "{} tiles / {} crumbs, {:.0} b/tile",
            tile_count,
            treemap_cache.breadcrumbs.len(),
            per_unit(treemap_delta, tile_count)
        ),
    );
    drop(treemap_cache);

    eprintln!("    text cache                | skipped by default (search UI currently hidden)");

    // Residual check (rust-analyzer style): what's still retained now vs the
    // itemized survivors (tree + category stats; rows/caches were dropped).
    // Nonzero residual = allocations the breakdown doesn't attribute, e.g.
    // the progress struct or stray thread-local growth.
    let retained_total = ALLOCATED.load(Ordering::SeqCst).saturating_sub(baseline);
    let itemized = tree_delta + stats_delta;
    eprintln!(
        "    retained total {} | itemized {} | unaccounted {}",
        bytesize::ByteSize::b(retained_total as u64),
        bytesize::ByteSize::b(itemized as u64),
        bytesize::ByteSize::b(retained_total.saturating_sub(itemized) as u64),
    );
    if let Some(rss) = peak_rss_bytes() {
        eprintln!(
            "    process peak RSS {} (OS-level; includes allocator overhead, stacks, code, and \
             earlier bench iterations)",
            bytesize::ByteSize::b(rss),
        );
    }
    std::hint::black_box((tree, stats));
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

    if memory_report_enabled() {
        eprintln!("\n=== Real Scan Memory Breakdown ===");
        for (label, path) in [
            (
                "~/git",
                dirs::home_dir()
                    .map(|h| h.join("git"))
                    .filter(|p| p.is_dir()),
            ),
            ("~", dirs::home_dir().filter(|p| p.is_dir())),
        ] {
            if let Some(ref p) = path {
                print_real_scan_breakdown(label, p);
            }
        }
        eprintln!("===============================\n");
    }
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

        group.bench_with_input(BenchmarkId::new("empty_dirs", count), &count, |b, _| {
            b.iter(|| {
                let progress = new_progress();
                scanner::scan_directory(tmp.path(), progress)
            })
        });
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

    print_synthetic_memory_report("1000 files / 100 dirs", tmp.path());
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

    print_synthetic_memory_report("10,000 files / 500 dirs", tmp.path());
}

fn memory_report_enabled() -> bool {
    std::env::var_os("MEMORY_REPORT").is_some()
}

fn print_synthetic_memory_report(label: &str, path: &std::path::Path) {
    if !memory_report_enabled() {
        return;
    }
    reset_tracking();
    let progress = new_progress();
    let before = ALLOCATED.load(Ordering::SeqCst);
    let tree = scanner::scan_directory(path, progress.clone());
    let delta = ALLOCATED.load(Ordering::SeqCst).saturating_sub(before);
    let peak = PEAK.load(Ordering::SeqCst).saturating_sub(before);
    let nodes = count_nodes(&tree);
    let files = progress.file_count.load(Ordering::Relaxed);
    eprintln!("\n=== Memory Report: {label} ===");
    eprintln!("Nodes: {nodes}");
    eprintln!("Files scanned: {files}");
    eprintln!(
        "Memory delta: {delta} bytes ({:.1} KB)",
        delta as f64 / 1024.0
    );
    eprintln!("Bytes per node: {:.0}", delta as f64 / nodes as f64);
    eprintln!(
        "Peak allocation: {peak} bytes ({:.1} KB)",
        peak as f64 / 1024.0
    );
    eprintln!("=============================================\n");
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
