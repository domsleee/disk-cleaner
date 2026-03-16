use criterion::{criterion_group, criterion_main, Criterion};
use disk_cleaner::scanner::{self, ScanProgress};
use disk_cleaner::tree::FileNode;
use disk_cleaner::ui;
use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
    })
}

fn count_nodes(node: &FileNode) -> usize {
    1 + node.children.iter().map(count_nodes).sum::<usize>()
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
            let nodes = count_nodes(&tree);
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

    c.bench_function("node_matches_hit_1100_nodes", |b| {
        b.iter(|| ui::node_matches(&tree, "file_5"))
    });

    c.bench_function("node_matches_miss_1100_nodes", |b| {
        b.iter(|| ui::node_matches(&tree, "nonexistent_zzz"))
    });

    c.bench_function("count_selected_1100_nodes", |b| {
        b.iter(|| ui::count_selected(&tree))
    });
}

criterion_group!(benches, bench_memory_synthetic, bench_tree_ops);
criterion_main!(benches);
