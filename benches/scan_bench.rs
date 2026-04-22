//! Scanning benchmarks — disk I/O, tree construction, memory allocation.
//!
//! Covers: synthetic scans (deep + 20k-file), real directory scans,
//! directory-heavy layouts, and memory-per-node tracking.
//!
//! ```sh
//! cargo bench --bench scan_bench
//! ```

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use disk_cleaner::categories;
use disk_cleaner::scanner::{self, ScanProgress};
use disk_cleaner::treemap;
use disk_cleaner::tree::FileNode;
use disk_cleaner::ui;
use eframe::egui;
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

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
        fallback_count: AtomicU64::new(0),
        access_denied_fallback_count: AtomicU64::new(0),
        bulk_scan_fallback_count: AtomicU64::new(0),
        fallback_details: std::sync::Mutex::new(Vec::new()),
        cancelled: AtomicBool::new(false),
    })
}

fn count_nodes(node: &FileNode) -> usize {
    1 + node.children().iter().map(count_nodes).sum::<usize>()
}

fn count_treemap_tiles(cache: &treemap::TreemapCache) -> usize {
    cache.tiles.len()
        + cache.other.iter().count()
        + cache.tiles.iter().map(|tile| tile.nested.len()).sum::<usize>()
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

fn print_real_scan_breakdown(label: &str, path: &std::path::Path) {
    reset_tracking();

    let progress = new_progress();
    let (mut tree, tree_delta, tree_peak) =
        measure_retained(|| scanner::scan_directory(path, progress.clone()));
    let node_count = count_nodes(&tree);
    let file_count = progress.file_count.load(Ordering::Relaxed);
    let scan_size = progress.total_size.load(Ordering::Relaxed);

    disk_cleaner::tree::auto_expand(&mut tree, 0, 2);

    eprintln!(
        "  {label:8} | {file_count:>9} files | {node_count:>9} nodes | scanned {}",
        bytesize::ByteSize::b(scan_size),
    );
    print_memory_line(
        "scanner tree",
        tree_delta,
        tree_peak,
        node_count,
        format_args!("{file_count} files"),
    );

    let expanded_file_groups: HashSet<PathBuf> = HashSet::new();
    let (rows, rows_delta, rows_peak) = measure_retained(|| {
        ui::collect_cached_rows(&tree, "", None, true, None, None, Some(&expanded_file_groups))
    });
    print_memory_line(
        "row cache (unfiltered)",
        rows_delta,
        rows_peak,
        node_count,
        format_args!("{} rows", rows.len()),
    );
    drop(rows);

    if let Some((cat, _, _)) = categories::compute_stats(&tree).entries.first().copied() {
        let (cat_cache, cache_delta, cache_peak) =
            measure_retained(|| ui::build_category_match_cache(&tree, cat));
        print_memory_line(
            &format!("category cache ({})", cat.label()),
            cache_delta,
            cache_peak,
            node_count,
            format_args!("{} cached paths", cat_cache.len()),
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
            format_args!("{} rows", filtered_rows.len()),
        );
        drop(filtered_rows);
        drop(cat_cache);
    }

    let full_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0));
    let (treemap_cache, treemap_delta, treemap_peak) =
        measure_retained(|| treemap::build_treemap_cache(&tree, &None, None, true, full_rect));
    print_memory_line(
        "treemap cache",
        treemap_delta,
        treemap_peak,
        node_count,
        format_args!(
            "{} tiles / {} crumbs",
            count_treemap_tiles(&treemap_cache),
            treemap_cache.breadcrumbs.len()
        ),
    );
    drop(treemap_cache);

    eprintln!("    text cache                | skipped by default (search UI currently hidden)");
    std::hint::black_box(tree);
}

#[derive(Debug, Default, PartialEq, Eq)]
struct CriterionCli {
    filter: Option<String>,
    exact: bool,
    suppress_side_reports: bool,
}

fn criterion_option_takes_value(arg: &str) -> bool {
    matches!(
        arg,
        "-c" | "--color"
            | "-s"
            | "--save-baseline"
            | "-b"
            | "--baseline"
            | "--baseline-lenient"
            | "--format"
            | "--profile-time"
            | "--load-baseline"
            | "--sample-size"
            | "--warm-up-time"
            | "--measurement-time"
            | "--nresamples"
            | "--noise-threshold"
            | "--confidence-level"
            | "--significance-level"
            | "--plotting-backend"
            | "--output-format"
    )
}

fn parse_criterion_cli<I, S>(args: I) -> CriterionCli
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut cli = CriterionCli::default();
    let mut skip_next = false;

    for arg in args.into_iter().skip(1) {
        let arg = arg.as_ref();

        if skip_next {
            skip_next = false;
            continue;
        }

        match arg {
            "--exact" => {
                cli.exact = true;
                continue;
            }
            "--list" | "--ignored" | "-h" | "--help" => {
                cli.suppress_side_reports = true;
                continue;
            }
            _ => {}
        }

        if let Some((flag, _)) = arg.split_once('=')
            && criterion_option_takes_value(flag)
        {
            continue;
        }

        if arg.starts_with('-') {
            if criterion_option_takes_value(arg) {
                skip_next = true;
            }
            continue;
        }

        if cli.filter.is_none() {
            cli.filter = Some(arg.to_owned());
        }
    }

    cli
}

fn criterion_cli() -> &'static CriterionCli {
    static CLI: OnceLock<CriterionCli> = OnceLock::new();
    CLI.get_or_init(|| parse_criterion_cli(std::env::args()))
}

fn cli_matches_bench_id(cli: &CriterionCli, bench_id: &str) -> bool {
    match cli.filter.as_deref() {
        None => true,
        Some(filter) if cli.exact => bench_id == filter,
        Some(filter) => bench_id.contains(filter),
    }
}

fn should_emit_side_reports() -> bool {
    !criterion_cli().suppress_side_reports
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

/// Check if a benchmark ID matches the active Criterion filter.
/// Criterion treats the first non-option argument as the filter, with
/// `--exact` switching from substring to exact matching.
fn bench_filter_matches(bench_id: &str) -> bool {
    cli_matches_bench_id(criterion_cli(), bench_id)
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

    // One-time memory report for each real directory (only when selected).
    if should_emit_side_reports() {
        eprintln!("\n=== Real Scan Memory Breakdown ===");
        for (label, id, path) in [
            (
                "~/git",
                "scan_real/home_git",
                dirs::home_dir()
                    .map(|h| h.join("git"))
                    .filter(|p| p.is_dir()),
            ),
            (
                "~",
                "scan_real/home",
                dirs::home_dir().filter(|p| p.is_dir()),
            ),
        ] {
            if !bench_filter_matches(id) {
                continue;
            }
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

    // Print a one-time memory report (only when this bench is selected)
    if should_emit_side_reports() && bench_filter_matches("memory_1000_files_100_dirs") {
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

    // Print memory report for large tree (only when this bench is selected)
    if should_emit_side_reports() && bench_filter_matches("memory_10000_files_500_dirs") {
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

#[cfg(test)]
mod tests {
    #[test]
    fn parses_plain_filter() {
        let cli = super::parse_criterion_cli(["scan_bench", "scan_real/home_git"]);
        assert_eq!(cli.filter.as_deref(), Some("scan_real/home_git"));
        assert!(!cli.exact);
        assert!(!cli.suppress_side_reports);
    }

    #[test]
    fn parses_filter_after_flags_and_values() {
        let cli = super::parse_criterion_cli([
            "scan_bench",
            "--color=always",
            "--sample-size",
            "25",
            "--exact",
            "scan_real/home_git",
        ]);
        assert_eq!(cli.filter.as_deref(), Some("scan_real/home_git"));
        assert!(cli.exact);
    }

    #[test]
    fn suppresses_side_reports_for_list_and_ignored_modes() {
        let list_cli = super::parse_criterion_cli(["scan_bench", "--list"]);
        assert!(list_cli.suppress_side_reports);

        let ignored_cli = super::parse_criterion_cli(["scan_bench", "--ignored"]);
        assert!(ignored_cli.suppress_side_reports);
    }

    #[test]
    fn matches_exact_filters_exactly() {
        let cli = super::parse_criterion_cli(["scan_bench", "--exact", "scan_real/home"]);
        assert!(super::cli_matches_bench_id(&cli, "scan_real/home"));
        assert!(!super::cli_matches_bench_id(&cli, "scan_real/home_git"));
    }
}
