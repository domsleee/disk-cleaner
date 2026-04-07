//! Benchmark startup time and simulated frame-time pressure during scan.
//!
//! Startup: measures App::default() construction (the non-window portion).
//! Frame-time: runs a scan on a background thread while the main thread
//! repeatedly polls for results (simulating what egui's update() does),
//! measuring per-"frame" durations.
//!
//! Usage: bench_perf [PATH]
//!   PATH defaults to ~/git if it exists, otherwise the project directory.

use disk_cleaner::categories;
use disk_cleaner::scanner::{self, ScanProgress};
use disk_cleaner::suggestions;
use disk_cleaner::tree;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| {
            dirs::home_dir()
                .map(|h| h.join("git"))
                .filter(|p| p.is_dir())
        })
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));

    if !path.is_dir() {
        eprintln!("Error: not a directory: {}", path.display());
        std::process::exit(1);
    }

    eprintln!("=== Disk Cleaner Performance Benchmark ===");
    eprintln!("Scan target: {}", path.display());
    eprintln!();

    // --- Startup time benchmark ---
    bench_startup();

    // --- Frame time during scan ---
    bench_frame_time(&path);
}

/// Measure the non-GUI portion of startup (App::default equivalent).
fn bench_startup() {
    eprintln!("--- Startup Time (non-GUI init) ---");

    const RUNS: u32 = 10;
    let mut times = Vec::with_capacity(RUNS as usize);

    for _ in 0..RUNS {
        let start = Instant::now();

        // Simulate what App::default() does (minus icon_cache which needs a GPU context)
        let _volumes = scanner::list_volumes();
        let _progress = Arc::new(ScanProgress {
            file_count: AtomicU64::new(0),
            total_size: AtomicU64::new(0),
            cancelled: AtomicBool::new(false),
        });

        let elapsed = start.elapsed();
        times.push(elapsed);
    }

    times.sort();
    let min = times[0];
    let max = times[times.len() - 1];
    let median = times[times.len() / 2];
    let avg: Duration = times.iter().sum::<Duration>() / RUNS;

    eprintln!("  Runs:   {RUNS}");
    eprintln!("  Min:    {min:?}");
    eprintln!("  Median: {median:?}");
    eprintln!("  Avg:    {avg:?}");
    eprintln!("  Max:    {max:?}");
    eprintln!("  Target: < 200ms total (including window creation)");
    eprintln!();
}

/// Simulate egui's update() loop during an active scan.
///
/// The main thread polls the scan channel + reads atomic counters on each
/// "frame", measuring how long each poll takes. This shows whether the rayon
/// scan threadpool contends with the UI thread.
fn bench_frame_time(scan_path: &Path) {
    eprintln!("--- Frame Time During Scan ---");
    eprintln!("  Scanning: {}", scan_path.display());

    let progress = Arc::new(ScanProgress {
        file_count: AtomicU64::new(0),
        total_size: AtomicU64::new(0),
        cancelled: AtomicBool::new(false),
    });

    let (tx, rx) = mpsc::channel();
    let p = progress.clone();
    let path = scan_path.to_path_buf();

    let scan_start = Instant::now();
    std::thread::spawn(move || {
        let tree = scanner::scan_directory(&path, p);
        let _ = tx.send(tree);
    });

    let mut frame_times: Vec<Duration> = Vec::new();
    let mut result_tree = None;

    // Simulate ~60fps polling loop
    loop {
        let frame_start = Instant::now();

        // This is what update() does each frame during a scan:
        let _file_count = progress.file_count.load(Ordering::Relaxed);
        let _total_size = progress.total_size.load(Ordering::Relaxed);

        match rx.try_recv() {
            Ok(tree) => {
                result_tree = Some(tree);
                let frame_dur = frame_start.elapsed();
                frame_times.push(frame_dur);
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }

        let frame_dur = frame_start.elapsed();
        frame_times.push(frame_dur);

        // Sleep to simulate ~60fps target (minus work done this frame)
        let target = Duration::from_micros(16_667); // 16.67ms
        if let Some(remaining) = target.checked_sub(frame_dur) {
            std::thread::sleep(remaining);
        }
    }

    let scan_duration = scan_start.elapsed();
    let file_count = progress.file_count.load(Ordering::Relaxed);
    let total_size = progress.total_size.load(Ordering::Relaxed);

    // Post-scan work (what update() does when scan completes)
    let post_scan_start = Instant::now();
    if let Some(ref tree) = result_tree {
        let _stats = categories::compute_stats(tree);
        let _suggestions = suggestions::analyze(tree);
    }
    if let Some(ref mut t) = result_tree {
        let root = t.root();
        tree::auto_expand(t, root, 0, 2);
    }
    let post_scan_dur = post_scan_start.elapsed();

    // Compute frame time stats (excluding the sleep portion — just work time)
    frame_times.sort();
    let n = frame_times.len();

    if n == 0 {
        eprintln!("  No frames recorded!");
        return;
    }

    let min = frame_times[0];
    let max = frame_times[n - 1];
    let median = frame_times[n / 2];
    let p99_idx = (n as f64 * 0.99) as usize;
    let p99 = frame_times[p99_idx.min(n - 1)];
    let avg: Duration = frame_times.iter().sum::<Duration>() / n as u32;
    let over_16ms = frame_times
        .iter()
        .filter(|d| **d > Duration::from_millis(16))
        .count();

    eprintln!("  Files scanned: {file_count}");
    eprintln!("  Total size:    {}", bytesize::ByteSize::b(total_size));
    eprintln!("  Scan duration: {scan_duration:?}");
    eprintln!("  Frames:        {n}");
    eprintln!();
    eprintln!("  Frame time (work portion, excluding sleep):");
    eprintln!("    Min:    {min:?}");
    eprintln!("    Median: {median:?}");
    eprintln!("    Avg:    {avg:?}");
    eprintln!("    P99:    {p99:?}");
    eprintln!("    Max:    {max:?}");
    eprintln!(
        "    >16ms:  {over_16ms}/{n} frames ({:.1}%)",
        over_16ms as f64 / n as f64 * 100.0
    );
    eprintln!();
    eprintln!("  Post-scan processing (categories + suggestions + auto_expand): {post_scan_dur:?}");
    eprintln!("  Target: all frames < 16ms (60fps)");
    eprintln!();

    std::hint::black_box(result_tree);
}
