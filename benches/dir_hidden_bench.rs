//! Microbenchmark: directory hidden-detection strategies on macOS.
//!
//! Measures four approaches:
//!   1. Old: symlink_metadata (lstat) per directory to get st_flags
//!   2. New: DirEntry::metadata() from parent (fstatat with open dirfd)
//!   3. Dot-prefix only: skip stat, just check name starts with '.'
//!   4. getattrlistbulk: batch-fetch flags for all entries in one syscall
//!
//! Run with:
//!   cargo bench --bench dir_hidden_bench
//!
//! Set BENCH_DIR to test a specific directory (default: $HOME).

use std::path::Path;
use std::time::{Duration, Instant};

// ── Approach 1: lstat per directory ──────────────────────────────────

fn measure_lstat_per_dir(dir: &Path) -> (usize, Duration) {
    let entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return (0, Duration::ZERO),
    };

    let start = Instant::now();
    let mut count = 0usize;
    for entry in &entries {
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() {
            // This is what current walk_dir does: lstat the child dir
            let _hidden = match std::fs::symlink_metadata(entry.path()) {
                Ok(m) => {
                    #[cfg(target_os = "macos")]
                    {
                        use std::os::darwin::fs::MetadataExt;
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        name.starts_with('.') || m.st_flags() & 0x8000 != 0
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        name.starts_with('.')
                    }
                }
                Err(_) => {
                    let name = entry.file_name();
                    name.to_string_lossy().starts_with('.')
                }
            };
            count += 1;
        }
    }
    (count, start.elapsed())
}

// ── Approach 2: DirEntry::metadata() from parent (the optimization) ─

fn measure_direntry_metadata(dir: &Path) -> (usize, Duration) {
    let entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return (0, Duration::ZERO),
    };

    let start = Instant::now();
    let mut count = 0usize;
    for entry in &entries {
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() {
            // This is what the optimized walk_dir does: use DirEntry::metadata()
            // which calls fstatat(dirfd, name, AT_SYMLINK_NOFOLLOW) — reuses
            // the already-open directory fd instead of resolving a path.
            let _hidden = match entry.metadata() {
                Ok(m) => {
                    #[cfg(target_os = "macos")]
                    {
                        use std::os::darwin::fs::MetadataExt;
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        name.starts_with('.') || m.st_flags() & 0x8000 != 0
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        name.starts_with('.')
                    }
                }
                Err(_) => {
                    let name = entry.file_name();
                    name.to_string_lossy().starts_with('.')
                }
            };
            count += 1;
        }
    }
    (count, start.elapsed())
}

// ── Approach 3: dot-prefix only (no stat) ───────────────────────────

fn measure_dot_only(dir: &Path) -> (usize, Duration) {
    let entries: Vec<_> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return (0, Duration::ZERO),
    };

    let start = Instant::now();
    let mut count = 0usize;
    for entry in &entries {
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() {
            let name = entry.file_name();
            let _hidden = name.to_string_lossy().starts_with('.');
            count += 1;
        }
    }
    (count, start.elapsed())
}

// ── Approach 4: getattrlistbulk (macOS only) ────────────────────────

#[cfg(target_os = "macos")]
fn measure_getattrlistbulk(dir: &Path) -> (usize, Duration) {
    use std::os::unix::ffi::OsStrExt;

    let dir_cstr = match std::ffi::CString::new(dir.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return (0, Duration::ZERO),
    };

    let fd = unsafe { libc::open(dir_cstr.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    if fd < 0 {
        return (0, Duration::ZERO);
    }

    // Set up attrlist requesting ATTR_CMN_NAME | ATTR_CMN_OBJTYPE | ATTR_CMN_FLAGS | ATTR_CMN_RETURNED_ATTRS
    #[repr(C)]
    struct AttrList {
        bitmapcount: u16,
        reserved: u16,
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }

    const ATTR_CMN_RETURNED_ATTRS: u32 = 0x0000_0001 << 31; // bit 31
    const ATTR_CMN_NAME: u32 = 0x0000_0001;
    const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
    const ATTR_CMN_FLAGS: u32 = 0x0004_0000;

    let mut attrlist = AttrList {
        bitmapcount: 5,
        reserved: 0,
        commonattr: ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_OBJTYPE | ATTR_CMN_FLAGS,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };

    let buf_size: usize = 256 * 1024; // 256 KB buffer
    let mut buf = vec![0u8; buf_size];

    let start = Instant::now();
    let mut dir_count = 0usize;

    loop {
        let ret = unsafe {
            libc::getattrlistbulk(
                fd,
                &mut attrlist as *mut AttrList as *mut libc::c_void,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf_size,
                0u64,
            )
        } as i64;

        if ret <= 0 {
            break;
        }

        // Parse entries
        let mut offset = 0usize;
        for _ in 0..ret as usize {
            if offset + 4 > buf_size {
                break;
            }
            let entry_len =
                u32::from_ne_bytes(buf[offset..offset + 4].try_into().unwrap()) as usize;
            if entry_len == 0 || offset + entry_len > buf_size {
                break;
            }

            // After length (4 bytes): attribute_set_t (5 * 4 = 20 bytes) for RETURNED_ATTRS
            // Then: name (attrreference_t = 4 + 4 bytes offset+length), objtype (u32), flags (u32)
            let base = offset + 4 + 20; // skip length + returned_attrs

            if base + 8 + 4 + 4 <= offset + entry_len {
                // attrreference_t for name: offset(i32) + length(u32)
                let _name_off =
                    i32::from_ne_bytes(buf[base..base + 4].try_into().unwrap()) as usize;
                let _name_len =
                    u32::from_ne_bytes(buf[base + 4..base + 8].try_into().unwrap()) as usize;

                let objtype = u32::from_ne_bytes(buf[base + 8..base + 12].try_into().unwrap());
                let flags = u32::from_ne_bytes(buf[base + 12..base + 16].try_into().unwrap());

                // VREG = 1, VDIR = 2
                if objtype == 2 {
                    // Directory - check name and flags
                    let name_start = base + _name_off;
                    if name_start < offset + entry_len {
                        let name_end = std::cmp::min(name_start + _name_len, offset + entry_len);
                        let name_bytes = &buf[name_start..name_end];
                        // Trim null terminator
                        let name_bytes = if name_bytes.last() == Some(&0) {
                            &name_bytes[..name_bytes.len() - 1]
                        } else {
                            name_bytes
                        };
                        let _hidden = name_bytes.first() == Some(&b'.') || (flags & 0x8000) != 0;
                    }
                    dir_count += 1;
                }
            }

            offset += entry_len;
        }
    }

    let elapsed = start.elapsed();
    unsafe { libc::close(fd) };
    (dir_count, elapsed)
}

#[cfg(not(target_os = "macos"))]
fn measure_getattrlistbulk(_dir: &Path) -> (usize, Duration) {
    eprintln!("getattrlistbulk not available on this platform");
    (0, Duration::ZERO)
}

// ── Recursive walker to measure across a whole tree ─────────────────

fn walk_and_measure(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    lstat_total: &mut Duration,
    direntry_total: &mut Duration,
    dot_total: &mut Duration,
    bulk_total: &mut Duration,
    dir_count: &mut usize,
) {
    if depth > max_depth {
        return;
    }

    let (n1, d1) = measure_lstat_per_dir(dir);
    let (_, d2) = measure_direntry_metadata(dir);
    let (_, d3) = measure_dot_only(dir);
    let (_, d4) = measure_getattrlistbulk(dir);

    *lstat_total += d1;
    *direntry_total += d2;
    *dot_total += d3;
    *bulk_total += d4;
    *dir_count += n1;

    // Recurse into subdirs
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            if let Ok(ft) = entry.file_type() {
                if ft.is_dir() {
                    let path = entry.path();
                    walk_and_measure(
                        &path,
                        depth + 1,
                        max_depth,
                        lstat_total,
                        direntry_total,
                        dot_total,
                        bulk_total,
                        dir_count,
                    );
                }
            }
        }
    }
}

fn main() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users".to_string());
    let dir = std::env::var("BENCH_DIR").unwrap_or(home);
    let max_depth: usize = std::env::var("BENCH_DEPTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let path = Path::new(&dir);
    assert!(path.exists(), "Directory does not exist: {dir}");

    eprintln!("=== Dir Hidden Detection Benchmark ===");
    eprintln!("Directory  : {dir}");
    eprintln!("Max depth  : {max_depth}");
    eprintln!();

    // Warmup
    eprint!("  Warmup...");
    {
        let mut a = Duration::ZERO;
        let mut b = Duration::ZERO;
        let mut c = Duration::ZERO;
        let mut d = Duration::ZERO;
        let mut e = 0;
        walk_and_measure(path, 0, max_depth, &mut a, &mut b, &mut c, &mut d, &mut e);
    }
    eprintln!(" done");

    let runs = 5;
    let mut lstat_times = Vec::new();
    let mut direntry_times = Vec::new();
    let mut dot_times = Vec::new();
    let mut bulk_times = Vec::new();
    let mut total_dirs = 0;

    for i in 0..runs {
        let mut lstat_total = Duration::ZERO;
        let mut direntry_total = Duration::ZERO;
        let mut dot_total = Duration::ZERO;
        let mut bulk_total = Duration::ZERO;
        let mut dir_count = 0;

        walk_and_measure(
            path,
            0,
            max_depth,
            &mut lstat_total,
            &mut direntry_total,
            &mut dot_total,
            &mut bulk_total,
            &mut dir_count,
        );

        total_dirs = dir_count;
        eprintln!(
            "  Run {}: lstat={:.1}ms  direntry={:.1}ms  dot={:.1}ms  bulk={:.1}ms  dirs={}",
            i + 1,
            lstat_total.as_secs_f64() * 1000.0,
            direntry_total.as_secs_f64() * 1000.0,
            dot_total.as_secs_f64() * 1000.0,
            bulk_total.as_secs_f64() * 1000.0,
            dir_count,
        );

        lstat_times.push(lstat_total.as_secs_f64() * 1000.0);
        direntry_times.push(direntry_total.as_secs_f64() * 1000.0);
        dot_times.push(dot_total.as_secs_f64() * 1000.0);
        bulk_times.push(bulk_total.as_secs_f64() * 1000.0);
    }

    let avg = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;

    eprintln!();
    eprintln!("=== Results ({total_dirs} directories, {runs} runs) ===");
    eprintln!(
        "  symlink_metadata  : {:.2} ms avg  ({:.1} µs/dir)  [OLD]",
        avg(&lstat_times),
        avg(&lstat_times) * 1000.0 / total_dirs as f64
    );
    eprintln!(
        "  DirEntry::metadata: {:.2} ms avg  ({:.1} µs/dir)  [NEW — the optimization]",
        avg(&direntry_times),
        avg(&direntry_times) * 1000.0 / total_dirs as f64
    );
    eprintln!(
        "  dot-prefix only   : {:.2} ms avg  ({:.1} µs/dir)  [lower bound]",
        avg(&dot_times),
        avg(&dot_times) * 1000.0 / total_dirs as f64
    );
    eprintln!(
        "  getattrlistbulk   : {:.2} ms avg  ({:.1} µs/dir)  [batch syscall]",
        avg(&bulk_times),
        avg(&bulk_times) * 1000.0 / total_dirs as f64
    );
    eprintln!();
    let lstat_avg = avg(&lstat_times);
    let direntry_avg = avg(&direntry_times);
    let dot_avg = avg(&dot_times);
    let bulk_avg = avg(&bulk_times);
    eprintln!(
        "  DirEntry vs lstat : saves {:.1}ms ({:.0}%)",
        lstat_avg - direntry_avg,
        if lstat_avg > 0.0 { (lstat_avg - direntry_avg) / lstat_avg * 100.0 } else { 0.0 },
    );
    eprintln!(
        "  dot-only vs lstat : saves {:.1}ms ({:.0}%)",
        lstat_avg - dot_avg,
        if lstat_avg > 0.0 { (lstat_avg - dot_avg) / lstat_avg * 100.0 } else { 0.0 },
    );
    eprintln!(
        "  bulk vs lstat     : saves {:.1}ms ({:.0}%)",
        lstat_avg - bulk_avg,
        if lstat_avg > 0.0 { (lstat_avg - bulk_avg) / lstat_avg * 100.0 } else { 0.0 },
    );
    eprintln!("==========================================");
}
