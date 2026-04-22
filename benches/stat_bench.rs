//! Statistical benchmark for measuring scan performance on real directories.
//!
//! Unlike the regression benchmark (which uses a synthetic fixture for CI
//! reproducibility), this benchmark scans a real directory to capture
//! real-world memory and speed characteristics.
//!
//! # Running
//!
//! ```sh
//! # Default: scan $HOME with 10 runs
//! cargo bench --bench stat_bench
//!
//! # Custom directory and run count
//! BENCH_DIR=/path/to/scan BENCH_RUNS=5 cargo bench --bench stat_bench
//! ```
//!
//! # Output
//!
//! Prints per-run stats plus a summary with mean, stddev, and 95% confidence
//! intervals. Memory measurements (bytes/node, delta) are deterministic (zero
//! variance). Speed measurements include CI for significance assessment.
//!
//! Compare branches by running on each and comparing the summary lines.

use disk_cleaner::scanner::{self, ScanProgress};
use disk_cleaner::tree::FileNode;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
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

fn count_nodes(node: &FileNode) -> usize {
    1 + node.children().iter().map(count_nodes).sum::<usize>()
}

fn new_progress() -> Arc<ScanProgress> {
    Arc::new(ScanProgress {
        file_count: AtomicU64::new(0),
        total_size: AtomicU64::new(0),
        fallback_count: AtomicU64::new(0),
        cancelled: AtomicBool::new(false),
    })
}

// ── Statistics ───────────────────────────────────────────────────────

fn mean(data: &[f64]) -> f64 {
    data.iter().sum::<f64>() / data.len() as f64
}

fn stddev(data: &[f64]) -> f64 {
    let m = mean(data);
    let variance = data.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (data.len() - 1) as f64;
    variance.sqrt()
}

/// 95% CI half-width using t-distribution critical values.
fn ci95_half(data: &[f64]) -> f64 {
    let n = data.len() as f64;
    let s = stddev(data);
    let t = match data.len() - 1 {
        1 => 12.706,
        2 => 4.303,
        3 => 3.182,
        4 => 2.776,
        5 => 2.571,
        6 => 2.447,
        7 => 2.365,
        8 => 2.306,
        9 => 2.262,
        10..=14 => 2.145,
        15..=29 => 2.042,
        _ => 1.96,
    };
    t * s / n.sqrt()
}

// ── Main ─────────────────────────────────────────────────────────────

fn main() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users".to_string());
    let dir = std::env::var("BENCH_DIR").unwrap_or(home);
    let runs: usize = std::env::var("BENCH_RUNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let path = std::path::Path::new(&dir);
    assert!(path.exists(), "Directory does not exist: {dir}");

    eprintln!("=== Statistical Benchmark ===");
    eprintln!("Directory : {dir}");
    eprintln!("Runs      : {runs} (+ 1 warmup)");
    eprintln!();

    // Warmup run (not measured)
    eprint!("  Warmup...");
    {
        let p = new_progress();
        let tree = scanner::scan_directory(path, p);
        std::hint::black_box(tree);
    }
    eprintln!(" done");

    let mut times = Vec::with_capacity(runs);
    let mut bpn_vals = Vec::with_capacity(runs);
    let mut peak_vals = Vec::with_capacity(runs);
    let mut delta_vals = Vec::with_capacity(runs);
    let mut node_count = 0usize;
    let mut file_count_last = 0u64;

    for i in 0..runs {
        reset_peak();
        let before = ALLOCATED.load(Ordering::SeqCst);
        let progress = new_progress();
        let start = Instant::now();
        let tree = scanner::scan_directory(path, progress.clone());
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        let after = ALLOCATED.load(Ordering::SeqCst);
        let peak = PEAK.load(Ordering::SeqCst);

        let delta = after.saturating_sub(before);
        let nodes = count_nodes(&tree);
        let bpn = delta as f64 / nodes as f64;
        file_count_last = progress.file_count.load(Ordering::Relaxed);
        node_count = nodes;

        eprintln!(
            "  Run {:2}: {:7.1} ms | {:6.1} MB delta | {:6.1} MB peak | {:5.1} b/node",
            i + 1,
            elapsed_ms,
            delta as f64 / 1e6,
            peak as f64 / 1e6,
            bpn
        );

        times.push(elapsed_ms);
        bpn_vals.push(bpn);
        peak_vals.push(peak as f64 / 1e6);
        delta_vals.push(delta as f64 / 1e6);

        std::hint::black_box(tree);
    }

    let t_mean = mean(&times);
    let t_sd = stddev(&times);
    let t_ci = ci95_half(&times);

    let bpn_mean = mean(&bpn_vals);
    let bpn_sd = stddev(&bpn_vals);
    let bpn_ci = ci95_half(&bpn_vals);

    eprintln!();
    eprintln!("=== Results ({runs} runs, {dir}) ===");
    eprintln!("Files      : {file_count_last}");
    eprintln!("Nodes      : {node_count}");
    eprintln!(
        "Scan time  : {t_mean:.1} ± {t_sd:.1} ms  (95% CI: [{:.1}, {:.1}])",
        t_mean - t_ci,
        t_mean + t_ci
    );
    eprintln!(
        "Bytes/node : {bpn_mean:.1} ± {bpn_sd:.1}  (95% CI: [{:.1}, {:.1}])",
        bpn_mean - bpn_ci,
        bpn_mean + bpn_ci
    );
    eprintln!(
        "Mem delta  : {:.1} ± {:.1} MB",
        mean(&delta_vals),
        stddev(&delta_vals)
    );
    eprintln!(
        "Peak alloc : {:.1} ± {:.1} MB",
        mean(&peak_vals),
        stddev(&peak_vals)
    );
    eprintln!("========================================");
}
