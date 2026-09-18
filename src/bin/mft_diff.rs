//! Dumps a scan as a canonical listing so two scanners can be compared path by
//! path. Whole-volume totals can agree while individual directories do not.
//!
//! The scanner is chosen by `DISK_CLEANER_MFT`, which is read once per process,
//! so run this twice and diff the output:
//!
//!   set DISK_CLEANER_MFT=off   && mft_diff C:\ walker.txt
//!   set DISK_CLEANER_MFT=force && mft_diff C:\ mft.txt
//!   diff walker.txt mft.txt

use disk_cleaner::scanner::{self, ScanProgress};
use disk_cleaner::tree::FileNode;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn write_node(out: &mut impl Write, node: &FileNode, prefix: &mut String, files: bool) {
    match node {
        FileNode::File(leaf) => {
            if files {
                let _ = writeln!(out, "F\t{}\t{}/{}", leaf.size(), prefix, leaf.name);
            }
        }
        FileNode::Dir(dir) => {
            let _ = writeln!(
                out,
                "D\t{}\t{}\t{}/{}",
                dir.size,
                dir.children.len(),
                prefix,
                dir.name
            );
            let len = prefix.len();
            prefix.push('/');
            prefix.push_str(&dir.name);
            let mut sorted: Vec<&FileNode> = dir.children.iter().collect();
            sorted.sort_by(|a, b| a.name().cmp(b.name()));
            for child in sorted {
                write_node(out, child, prefix, files);
            }
            prefix.truncate(len);
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().map(PathBuf::from);
    let dest = args.next();
    let files = std::env::args().any(|a| a == "--files");
    let (Some(path), Some(dest)) = (path, dest) else {
        eprintln!("Usage: mft_diff <PATH> <OUT> [--files]");
        std::process::exit(1);
    };

    let progress = Arc::new(ScanProgress {
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
    });

    let tree = scanner::scan_directory(&path, progress.clone());
    let scanner_used = if progress.mft_used.load(Ordering::Relaxed) {
        "mft"
    } else {
        "walker"
    };

    let file = std::fs::File::create(&dest).expect("create output");
    let mut out = BufWriter::new(file);
    let _ = writeln!(out, "# scanner={scanner_used} root={}", path.display());
    let mut prefix = String::new();
    write_node(&mut out, &tree, &mut prefix, files);
    let _ = out.flush();

    eprintln!(
        "scanner={scanner_used} files={} bytes={} -> {dest}",
        progress.file_count.load(Ordering::Relaxed),
        progress.total_size.load(Ordering::Relaxed),
    );
}
