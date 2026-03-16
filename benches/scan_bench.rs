use criterion::{criterion_group, criterion_main, Criterion};
use disk_cleaner::scanner::{self, ScanProgress};
use std::fs;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

fn new_progress() -> Arc<ScanProgress> {
    Arc::new(ScanProgress {
        file_count: AtomicU64::new(0),
        total_size: AtomicU64::new(0),
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

criterion_group!(
    benches,
    bench_scan_synthetic,
    bench_scan_deep,
    bench_scan_self
);
criterion_main!(benches);
