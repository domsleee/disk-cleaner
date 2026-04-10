//! Performance benchmark for disk-cleaner scanning and post-scan processing.
//!
//! Runs multiple iterations with warmup, reports proper statistics (median,
//! mean, stddev, min, max, CV%) so results are reproducible and comparable.
//!
//! Usage:
//!   bench_perf [OPTIONS] [PATH]
//!
//! Options:
//!   --runs N      Number of measured iterations (default: 5)
//!   --warmup N    Number of warmup iterations (default: 1)
//!   --no-startup  Skip startup benchmark
//!   --json        Output results as JSON (for scripted comparison)
//!
//! PATH defaults to ~/git if it exists, otherwise the project directory.

use disk_cleaner::categories;
use disk_cleaner::scanner::{self, ScanProgress};
use disk_cleaner::tree;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

struct Config {
    path: PathBuf,
    runs: usize,
    warmup: usize,
    skip_startup: bool,
    json: bool,
}

/// Results from a single scan iteration.
struct ScanRun {
    scan_duration: Duration,
    post_scan_duration: Duration,
    file_count: u64,
    total_size: u64,
    frame_count: usize,
    frame_max: Duration,
    frames_over_16ms: usize,
}

/// Statistical summary of a set of duration measurements.
struct Stats {
    min: Duration,
    max: Duration,
    median: Duration,
    mean: Duration,
    stddev: Duration,
    cv_percent: f64,
    values: Vec<Duration>,
}

impl Stats {
    fn from_durations(mut durations: Vec<Duration>) -> Self {
        assert!(!durations.is_empty());
        durations.sort();
        let n = durations.len();
        let min = durations[0];
        let max = durations[n - 1];
        let median = durations[n / 2];

        let sum: Duration = durations.iter().sum();
        let mean = sum / n as u32;
        let mean_ns = mean.as_nanos() as f64;

        let variance = durations
            .iter()
            .map(|d| {
                let diff = d.as_nanos() as f64 - mean_ns;
                diff * diff
            })
            .sum::<f64>()
            / n as f64;
        let stddev_ns = variance.sqrt();
        let stddev = Duration::from_nanos(stddev_ns as u64);
        let cv_percent = if mean_ns > 0.0 {
            (stddev_ns / mean_ns) * 100.0
        } else {
            0.0
        };

        Stats {
            min,
            max,
            median,
            mean,
            stddev,
            cv_percent,
            values: durations,
        }
    }
}

fn parse_args() -> Config {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut path: Option<PathBuf> = None;
    let mut runs = 5;
    let mut warmup = 1;
    let mut skip_startup = false;
    let mut json = false;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--runs" => {
                i += 1;
                runs = args[i].parse().expect("--runs requires a number");
            }
            "--warmup" => {
                i += 1;
                warmup = args[i].parse().expect("--warmup requires a number");
            }
            "--no-startup" => skip_startup = true,
            "--json" => json = true,
            "-h" | "--help" => {
                eprintln!("Usage: bench_perf [OPTIONS] [PATH]");
                eprintln!();
                eprintln!("Options:");
                eprintln!("  --runs N      Measured iterations (default: 5)");
                eprintln!("  --warmup N    Warmup iterations (default: 1)");
                eprintln!("  --no-startup  Skip startup benchmark");
                eprintln!("  --json        Output as JSON");
                std::process::exit(0);
            }
            arg if !arg.starts_with('-') => path = Some(PathBuf::from(arg)),
            other => {
                eprintln!("Unknown option: {other}");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let path = path
        .or_else(|| {
            dirs::home_dir()
                .map(|h| h.join("git"))
                .filter(|p| p.is_dir())
        })
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));

    Config {
        path,
        runs,
        warmup,
        skip_startup,
        json,
    }
}

fn main() {
    // Match main binary's rayon oversubscription for I/O-bound scanning
    rayon::ThreadPoolBuilder::new()
        .num_threads(std::thread::available_parallelism().map_or(8, |n| n.get()) * 2)
        .build_global()
        .ok();

    let config = parse_args();

    if !config.path.is_dir() {
        eprintln!("Error: not a directory: {}", config.path.display());
        std::process::exit(1);
    }

    if !config.json {
        eprintln!("=== Disk Cleaner Performance Benchmark ===");
        eprintln!("Scan target: {}", config.path.display());
        eprintln!("Warmup: {}  Measured runs: {}", config.warmup, config.runs);
        eprintln!();
    }

    if !config.skip_startup {
        bench_startup(&config);
    }

    bench_scan(&config);
}

/// Measure the non-GUI portion of startup (App::default equivalent).
fn bench_startup(config: &Config) {
    if !config.json {
        eprintln!("--- Startup Time (non-GUI init) ---");
    }

    const RUNS: u32 = 10;
    let mut times = Vec::with_capacity(RUNS as usize);

    for _ in 0..RUNS {
        let start = Instant::now();
        let _volumes = scanner::list_volumes();
        let _progress = Arc::new(ScanProgress {
            file_count: AtomicU64::new(0),
            total_size: AtomicU64::new(0),
            cancelled: AtomicBool::new(false),
        });
        times.push(start.elapsed());
    }

    let stats = Stats::from_durations(times);

    if !config.json {
        eprintln!("  Runs:   {RUNS}");
        eprintln!("  Min:    {:?}", stats.min);
        eprintln!("  Median: {:?}", stats.median);
        eprintln!("  Mean:   {:?}", stats.mean);
        eprintln!("  Max:    {:?}", stats.max);
        eprintln!("  Target: < 200ms total (including window creation)");
        eprintln!();
    }
}

/// Run a single scan + post-scan iteration, returning metrics.
fn run_scan_iteration(scan_path: &Path) -> ScanRun {
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
        let _file_count = progress.file_count.load(Ordering::Relaxed);
        let _total_size = progress.total_size.load(Ordering::Relaxed);

        match rx.try_recv() {
            Ok(tree) => {
                result_tree = Some(tree);
                frame_times.push(frame_start.elapsed());
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }

        let frame_dur = frame_start.elapsed();
        frame_times.push(frame_dur);

        let target = Duration::from_micros(16_667);
        if let Some(remaining) = target.checked_sub(frame_dur) {
            std::thread::sleep(remaining);
        }
    }

    let scan_duration = scan_start.elapsed();
    let file_count = progress.file_count.load(Ordering::Relaxed);
    let total_size = progress.total_size.load(Ordering::Relaxed);

    // Post-scan work
    let post_start = Instant::now();
    if let Some(ref tree) = result_tree {
        let _stats = categories::compute_stats(tree);
    }
    if let Some(ref mut t) = result_tree {
        tree::auto_expand(t, 0, 2);
    }
    let post_scan_duration = post_start.elapsed();

    frame_times.sort();
    let frame_count = frame_times.len();
    let frame_max = frame_times.last().copied().unwrap_or(Duration::ZERO);
    let frames_over_16ms = frame_times
        .iter()
        .filter(|d| **d > Duration::from_millis(16))
        .count();

    std::hint::black_box(result_tree);

    ScanRun {
        scan_duration,
        post_scan_duration,
        file_count,
        total_size,
        frame_count,
        frame_max,
        frames_over_16ms,
    }
}

/// Run warmup + measured iterations and report statistics.
fn bench_scan(config: &Config) {
    // --- Warmup ---
    if !config.json && config.warmup > 0 {
        eprintln!(
            "--- Warmup ({} run{}) ---",
            config.warmup,
            if config.warmup == 1 { "" } else { "s" }
        );
    }
    for i in 0..config.warmup {
        let run = run_scan_iteration(&config.path);
        if !config.json {
            eprintln!(
                "  warmup {}: scan {:.3}s, post-scan {:.1}ms",
                i + 1,
                run.scan_duration.as_secs_f64(),
                run.post_scan_duration.as_secs_f64() * 1000.0,
            );
        }
    }
    if !config.json && config.warmup > 0 {
        eprintln!();
    }

    // --- Measured runs ---
    if !config.json {
        eprintln!("--- Measured Runs ({}) ---", config.runs);
    }

    let mut runs: Vec<ScanRun> = Vec::with_capacity(config.runs);
    for i in 0..config.runs {
        let run = run_scan_iteration(&config.path);
        if !config.json {
            eprintln!(
                "  run {}: scan {:.3}s, post-scan {:.1}ms, frames {} (max {:?}, >16ms: {})",
                i + 1,
                run.scan_duration.as_secs_f64(),
                run.post_scan_duration.as_secs_f64() * 1000.0,
                run.frame_count,
                run.frame_max,
                run.frames_over_16ms,
            );
        }
        runs.push(run);
    }

    // --- Statistics ---
    let scan_stats = Stats::from_durations(runs.iter().map(|r| r.scan_duration).collect());
    let post_stats = Stats::from_durations(runs.iter().map(|r| r.post_scan_duration).collect());
    let file_count = runs.last().map(|r| r.file_count).unwrap_or(0);
    let total_size = runs.last().map(|r| r.total_size).unwrap_or(0);

    if config.json {
        print_json(&scan_stats, &post_stats, file_count, total_size, config);
    } else {
        eprintln!();
        eprintln!("--- Results ---");
        eprintln!("  Files scanned: {file_count}");
        eprintln!("  Total size:    {}", bytesize::ByteSize::b(total_size));
        eprintln!();
        print_stats("  Scan duration", &scan_stats, "s");
        eprintln!();
        print_stats("  Post-scan", &post_stats, "ms");
        eprintln!();

        let total_over_16ms: usize = runs.iter().map(|r| r.frames_over_16ms).sum();
        let total_frames: usize = runs.iter().map(|r| r.frame_count).sum();
        eprintln!(
            "  Frame jank: {total_over_16ms}/{total_frames} frames >16ms across all runs"
        );
    }
}

fn print_stats(label: &str, stats: &Stats, unit: &str) {
    let scale = match unit {
        "ms" => 1000.0,
        _ => 1.0,
    };
    eprintln!("{label}:");
    eprintln!(
        "    Median: {:.3}{unit}  Mean: {:.3}{unit}  Stddev: {:.3}{unit}  CV: {:.1}%",
        stats.median.as_secs_f64() * scale,
        stats.mean.as_secs_f64() * scale,
        stats.stddev.as_secs_f64() * scale,
        stats.cv_percent,
    );
    eprintln!(
        "    Min:    {:.3}{unit}  Max:  {:.3}{unit}",
        stats.min.as_secs_f64() * scale,
        stats.max.as_secs_f64() * scale,
    );
    eprint!("    Runs:   [");
    for (i, v) in stats.values.iter().enumerate() {
        if i > 0 {
            eprint!(", ");
        }
        eprint!("{:.3}", v.as_secs_f64() * scale);
    }
    eprintln!("]{unit}");
}

fn print_json(
    scan: &Stats,
    post: &Stats,
    file_count: u64,
    total_size: u64,
    config: &Config,
) {
    let scan_values: Vec<f64> = scan.values.iter().map(|d| d.as_secs_f64()).collect();
    let post_values: Vec<f64> = post
        .values
        .iter()
        .map(|d| d.as_secs_f64() * 1000.0)
        .collect();

    println!(
        r#"{{"path":"{}","runs":{},"warmup":{},"file_count":{},"total_size":{},"scan_s":{{"median":{:.3},"mean":{:.3},"stddev":{:.3},"cv_pct":{:.1},"min":{:.3},"max":{:.3},"values":{:?}}},"post_scan_ms":{{"median":{:.1},"mean":{:.1},"stddev":{:.1},"cv_pct":{:.1},"min":{:.1},"max":{:.1},"values":{:?}}}}}"#,
        config.path.display(),
        config.runs,
        config.warmup,
        file_count,
        total_size,
        scan.median.as_secs_f64(),
        scan.mean.as_secs_f64(),
        scan.stddev.as_secs_f64(),
        scan.cv_percent,
        scan.min.as_secs_f64(),
        scan.max.as_secs_f64(),
        scan_values,
        post.median.as_secs_f64() * 1000.0,
        post.mean.as_secs_f64() * 1000.0,
        post.stddev.as_secs_f64() * 1000.0,
        post.cv_percent,
        post.min.as_secs_f64() * 1000.0,
        post.max.as_secs_f64() * 1000.0,
        post_values,
    );
}
