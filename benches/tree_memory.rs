use criterion::{criterion_group, criterion_main, Criterion};
use disk_cleaner::scanner::{self, ScanProgress};
use disk_cleaner::tree::{FileTree, NodeId};
use disk_cleaner::treemap;
use disk_cleaner::ui;
use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

/// Tracking allocator that measures peak and current memory usage.
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

fn new_progress() -> Arc<ScanProgress> {
    Arc::new(ScanProgress {
        file_count: AtomicU64::new(0),
        total_size: AtomicU64::new(0),
        cancelled: AtomicBool::new(false),
    })
}

fn count_nodes(tree: &FileTree, id: NodeId) -> usize {
    1 + tree.children(id).iter().map(|&c| count_nodes(tree, c)).sum::<usize>()
}

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
            let nodes = count_nodes(&tree, tree.root());
            // Use black_box to prevent optimization
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
    let nodes = count_nodes(&tree, tree.root());
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

/// Measure memory for tree operations (node_matches, collect_selected)
fn bench_tree_ops(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..100 {
        let dir = tmp.path().join(format!("dir_{i:03}"));
        fs::create_dir(&dir).unwrap();
        for j in 0..10 {
            fs::write(dir.join(format!("file_{j}.bin")), vec![0u8; 1024]).unwrap();
        }
    }

    let progress = new_progress();
    let tree = scanner::scan_directory(tmp.path(), progress);

    let root = tree.root();
    c.bench_function("node_matches_hit_1100_nodes", |b| {
        b.iter(|| ui::node_matches(&tree, root, "file_5"))
    });

    c.bench_function("node_matches_miss_1100_nodes", |b| {
        b.iter(|| ui::node_matches(&tree, root, "nonexistent_zzz"))
    });

    // count_selected is benchmarked in tree_ops.rs with configurable selection sets
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
    let nodes = count_nodes(&tree, tree.root());
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

/// Benchmark squarify treemap layout at various sizes (render performance)
fn bench_squarify(c: &mut Criterion) {
    // 100 items — typical directory
    let sizes_100: Vec<f64> = (1..=100).rev().map(|i| i as f64).collect();
    c.bench_function("squarify_100_items", |b| {
        b.iter(|| treemap::squarify(&sizes_100, 0.0, 0.0, 1200.0, 800.0))
    });

    // 1000 items — large directory
    let sizes_1000: Vec<f64> = (1..=1000).rev().map(|i| i as f64).collect();
    c.bench_function("squarify_1000_items", |b| {
        b.iter(|| treemap::squarify(&sizes_1000, 0.0, 0.0, 1200.0, 800.0))
    });

    // 10,000 items — stress test
    let sizes_10k: Vec<f64> = (1..=10_000).rev().map(|i| i as f64).collect();
    c.bench_function("squarify_10000_items", |b| {
        b.iter(|| treemap::squarify(&sizes_10k, 0.0, 0.0, 1200.0, 800.0))
    });
}

/// Benchmark tree navigation (find_node, breadcrumbs) used during rendering
fn bench_tree_navigation(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..100 {
        let dir = tmp.path().join(format!("dir_{i:03}"));
        fs::create_dir(&dir).unwrap();
        for j in 0..10 {
            fs::write(dir.join(format!("file_{j}.bin")), vec![0u8; 1024]).unwrap();
        }
    }

    let progress = new_progress();
    let tree = scanner::scan_directory(tmp.path(), progress);

    // Find a deep node
    let deep_path = tmp.path().join("dir_050").join("file_5.bin");
    c.bench_function("find_node_1100_nodes", |b| {
        b.iter(|| treemap::find_node(&tree, &deep_path))
    });

    let dir_path = tmp.path().join("dir_050");
    c.bench_function("breadcrumbs_1100_nodes", |b| {
        b.iter(|| treemap::breadcrumbs(&tree, &dir_path))
    });
}

criterion_group!(
    benches,
    bench_memory_synthetic,
    bench_memory_large_synthetic,
    bench_tree_ops,
    bench_squarify,
    bench_tree_navigation,
);
criterion_main!(benches);
