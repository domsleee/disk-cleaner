use criterion::{criterion_group, criterion_main, Criterion};
use disk_cleaner::scanner::{self, ScanProgress};
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

fn new_progress() -> Arc<ScanProgress> {
    Arc::new(ScanProgress {
        file_count: AtomicU64::new(0),
        total_size: AtomicU64::new(0),
        cancelled: AtomicBool::new(false),
        permission_denied: AtomicU64::new(0),
    })
}

/// Benchmark scanning a small synthetic tree (100 dirs, 1000 files)
fn bench_scan_synthetic(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    // Create 100 dirs with 10 files each
    for i in 0..100 {
        let dir = tmp.path().join(format!("dir_{i:03}"));
        fs::create_dir(&dir).unwrap();
        for j in 0..10 {
            fs::write(dir.join(format!("file_{j}.bin")), vec![0u8; 1024]).unwrap();
        }
    }

    c.bench_function("scan_1000_files_100_dirs", |b| {
        b.iter(|| {
            let progress = new_progress();
            scanner::scan_directory(tmp.path(), progress)
        })
    });
}

/// Benchmark scanning a deep tree (10 levels, ~1000 nodes)
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

/// Benchmark scanning a large synthetic tree (10,000 files / 500 dirs)
fn bench_scan_large_synthetic(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    for i in 0..500 {
        let dir = tmp.path().join(format!("dir_{i:04}"));
        fs::create_dir(&dir).unwrap();
        for j in 0..20 {
            fs::write(dir.join(format!("file_{j}.dat")), vec![0u8; 4096]).unwrap();
        }
    }

    c.bench_function("scan_10000_files_500_dirs", |b| {
        b.iter(|| {
            let progress = new_progress();
            scanner::scan_directory(tmp.path(), progress)
        })
    });
}

/// Benchmark scanning a 20k-file tree to measure hidden-detection overhead.
/// This is the target benchmark for the lstat-elimination optimization:
/// before the fix, every non-dot file triggered a second lstat via is_os_hidden();
/// after, st_flags() is read from the already-fetched Metadata.
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

/// Benchmark scanning the project's own directory (real-world data)
fn bench_scan_self(c: &mut Criterion) {
    let project_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    c.bench_function("scan_project_dir", |b| {
        b.iter(|| {
            let progress = new_progress();
            scanner::scan_directory(project_dir, progress)
        })
    });
}

/// Benchmark scanning ~/git (real-world, large directory)
fn bench_scan_home_git(c: &mut Criterion) {
    let home_git = dirs::home_dir()
        .map(|h| h.join("git"))
        .filter(|p| p.is_dir());

    if let Some(git_dir) = home_git {
        c.bench_function("scan_home_git", |b| {
            b.iter(|| {
                let progress = new_progress();
                scanner::scan_directory(&git_dir, progress)
            })
        });
    } else {
        eprintln!("Skipping ~/git benchmark: directory not found");
    }
}

criterion_group!(
    benches,
    bench_scan_synthetic,
    bench_scan_deep,
    bench_scan_large_synthetic,
    bench_scan_20k_files,
    bench_scan_self,
    bench_scan_home_git,
);
criterion_main!(benches);
