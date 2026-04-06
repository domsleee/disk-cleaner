//! Benchmark UI-layer memory: measures RSS after scanning and building
//! filter caches + CachedRows, which is where the path interner has impact.
//!
//! Usage: bench_ui_memory [PATH]
//!   PATH defaults to ~ if it exists, otherwise the project directory.

use disk_cleaner::categories::FileCategory;
use disk_cleaner::intern::PathInterner;
use disk_cleaner::scanner::{self, ScanProgress};
use disk_cleaner::tree::{self, FileNode};
use disk_cleaner::ui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

fn rss_bytes() -> u64 {
    use std::mem::MaybeUninit;
    unsafe {
        let mut usage = MaybeUninit::<libc::rusage>::zeroed().assume_init();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        // macOS reports ru_maxrss in bytes
        usage.ru_maxrss as u64
    }
}

/// Current RSS (not peak) via mach task_info
fn current_rss_bytes() -> u64 {
    #[cfg(target_os = "macos")]
    {
        use std::mem;
        extern "C" {
            fn mach_task_self() -> u32;
            fn task_info(
                target: u32,
                flavor: u32,
                info: *mut libc::c_void,
                count: *mut u32,
            ) -> i32;
        }
        // MACH_TASK_BASIC_INFO = 20
        #[repr(C)]
        struct MachTaskBasicInfo {
            virtual_size: u64,
            resident_size: u64,
            resident_size_max: u64,
            user_time: [u32; 2],
            system_time: [u32; 2],
            policy: i32,
            suspend_count: i32,
        }
        let mut info: MachTaskBasicInfo = unsafe { mem::zeroed() };
        let mut count = (mem::size_of::<MachTaskBasicInfo>() / mem::size_of::<u32>()) as u32;
        unsafe {
            task_info(
                mach_task_self(),
                20, // MACH_TASK_BASIC_INFO
                &mut info as *mut _ as *mut libc::c_void,
                &mut count,
            );
        }
        info.resident_size
    }
    #[cfg(not(target_os = "macos"))]
    {
        rss_bytes()
    }
}

fn count_nodes(node: &FileNode) -> usize {
    1 + node.children().iter().map(count_nodes).sum::<usize>()
}

fn expand_all(node: &mut FileNode) {
    node.set_expanded(true);
    if let Some(d) = node.as_dir_mut() {
        for child in &mut d.children {
            expand_all(child);
        }
    }
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().filter(|p| p.is_dir()))
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));

    if !path.is_dir() {
        eprintln!("Error: not a directory: {}", path.display());
        std::process::exit(1);
    }

    eprintln!("=== UI Memory Benchmark ===");
    eprintln!("Scan target: {}", path.display());
    eprintln!();

    // --- Phase 1: Scan ---
    let rss_before_scan = current_rss_bytes();
    let progress = Arc::new(ScanProgress {
        file_count: AtomicU64::new(0),
        total_size: AtomicU64::new(0),
        cancelled: AtomicBool::new(false),
    });

    let start = Instant::now();
    let mut tree = scanner::scan_directory(&path, progress.clone());
    let scan_time = start.elapsed();
    let file_count = progress.file_count.load(Ordering::Relaxed);
    let total_size = progress.total_size.load(Ordering::Relaxed);
    let nodes = count_nodes(&tree);

    let rss_after_scan = current_rss_bytes();

    eprintln!("--- Phase 1: Scan ---");
    eprintln!("  Files:     {file_count}");
    eprintln!("  Nodes:     {nodes}");
    eprintln!("  Size:      {}", bytesize::ByteSize::b(total_size));
    eprintln!("  Time:      {scan_time:?}");
    eprintln!(
        "  RSS:       {:.1} MB (Δ {:.1} MB from baseline {:.1} MB)",
        mb(rss_after_scan),
        mb(rss_after_scan.saturating_sub(rss_before_scan)),
        mb(rss_before_scan),
    );
    eprintln!();

    // --- Phase 2: Expand all + build CachedRows (no filters) ---
    tree::auto_expand(&mut tree, 0, 2);
    expand_all(&mut tree);

    let mut interner = PathInterner::new();

    let start = Instant::now();
    let rows = ui::collect_cached_rows(
        &tree, "", None, true, None, None, None, &mut interner,
    );
    let rows_time = start.elapsed();
    let row_count = rows.len();
    let rss_after_rows = current_rss_bytes();

    eprintln!("--- Phase 2: CachedRows (all expanded, no filter) ---");
    eprintln!("  Rows:      {row_count}");
    eprintln!("  Time:      {rows_time:?}");
    eprintln!(
        "  RSS:       {:.1} MB (Δ {:.1} MB from scan)",
        mb(rss_after_rows),
        mb(rss_after_rows.saturating_sub(rss_after_scan)),
    );
    eprintln!();
    drop(rows);

    // --- Phase 3: Text filter cache ("log") ---
    let start = Instant::now();
    let text_cache = ui::build_text_match_cache(&tree, "log", &mut interner);
    let tc_time = start.elapsed();
    let tc_size = text_cache.len();
    let rss_after_tc = current_rss_bytes();

    eprintln!("--- Phase 3: Text filter cache (\"log\") ---");
    eprintln!("  Matching paths: {tc_size}");
    eprintln!("  Time:      {tc_time:?}");
    eprintln!(
        "  RSS:       {:.1} MB (Δ {:.1} MB from rows)",
        mb(rss_after_tc),
        mb(rss_after_tc.saturating_sub(rss_after_rows)),
    );
    eprintln!();

    // --- Phase 4: CachedRows with text filter ---
    let start = Instant::now();
    let rows_filtered = ui::collect_cached_rows(
        &tree, "log", None, true, Some(&text_cache), None, None, &mut interner,
    );
    let rf_time = start.elapsed();
    let rf_count = rows_filtered.len();
    let rss_after_filtered = current_rss_bytes();

    eprintln!("--- Phase 4: CachedRows with text filter ---");
    eprintln!("  Rows:      {rf_count}");
    eprintln!("  Time:      {rf_time:?}");
    eprintln!(
        "  RSS:       {:.1} MB (Δ {:.1} MB from text cache)",
        mb(rss_after_filtered),
        mb(rss_after_filtered.saturating_sub(rss_after_tc)),
    );
    eprintln!();

    // --- Phase 5: Category filter cache (Code) ---
    let start = Instant::now();
    let cat_cache = ui::build_category_match_cache(
        &tree, FileCategory::Code, &mut interner,
    );
    let cc_time = start.elapsed();
    let cc_size = cat_cache.len();
    let rss_after_cc = current_rss_bytes();

    eprintln!("--- Phase 5: Category filter cache (Code) ---");
    eprintln!("  Matching paths: {cc_size}");
    eprintln!("  Time:      {cc_time:?}");
    eprintln!(
        "  RSS:       {:.1} MB (Δ {:.1} MB from filtered rows)",
        mb(rss_after_cc),
        mb(rss_after_cc.saturating_sub(rss_after_filtered)),
    );
    eprintln!();

    // --- Phase 6: CachedRows with both filters ---
    let start = Instant::now();
    let rows_both = ui::collect_cached_rows(
        &tree, "log", Some(FileCategory::Code), true,
        Some(&text_cache), Some(&cat_cache), None, &mut interner,
    );
    let rb_time = start.elapsed();
    let rb_count = rows_both.len();
    let rss_after_both = current_rss_bytes();

    eprintln!("--- Phase 6: CachedRows with both filters ---");
    eprintln!("  Rows:      {rb_count}");
    eprintln!("  Time:      {rb_time:?}");
    eprintln!(
        "  RSS:       {:.1} MB (Δ {:.1} MB from cat cache)",
        mb(rss_after_both),
        mb(rss_after_both.saturating_sub(rss_after_cc)),
    );
    eprintln!();

    // --- Summary ---
    let peak = rss_bytes();
    eprintln!("=== Summary ===");
    eprintln!("  Peak RSS:            {:.1} MB", mb(peak));
    eprintln!("  Final RSS:           {:.1} MB", mb(rss_after_both));
    eprintln!(
        "  Scan → final Δ:     {:.1} MB",
        mb(rss_after_both.saturating_sub(rss_before_scan))
    );
    eprintln!(
        "  UI caches overhead:  {:.1} MB (from scan baseline to final)",
        mb(rss_after_both.saturating_sub(rss_after_scan))
    );
    eprintln!("===============");

    // Keep everything alive for accurate RSS
    std::hint::black_box((&tree, &text_cache, &cat_cache, &rows_filtered, &rows_both, &interner));
}
