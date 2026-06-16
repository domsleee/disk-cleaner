mod app_icon;
mod categories;
mod deleter;
mod icons;
mod scanner;
mod tree;
mod treemap;
mod ui;

use eframe::egui;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use scanner::ScanProgress;

fn debug_enabled() -> bool {
    use std::sync::OnceLock;
    static DEBUG: OnceLock<bool> = OnceLock::new();
    *DEBUG.get_or_init(|| std::env::var("DISK_CLEANER_DEBUG").is_ok_and(|v| v == "1"))
}

fn format_elapsed(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs >= 3600 {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else if duration < Duration::from_secs(1) {
        format!("{:.1}s", duration.as_secs_f64())
    } else {
        format!("{secs}s")
    }
}

fn write_fallback_report(
    scan_path: Option<&std::path::Path>,
    duration: Option<Duration>,
    total: u64,
    access_denied: u64,
    bulk_scan: u64,
    details: &[scanner::ScanFallbackDetail],
) -> std::io::Result<PathBuf> {
    let report_dir = std::env::temp_dir().join("disk-cleaner");
    std::fs::create_dir_all(&report_dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let report_path = report_dir.join(format!("windows-compatibility-report-{stamp}.txt"));

    let mut text = String::new();
    text.push_str("Disk Cleaner Windows compatibility report\n");
    text.push_str("========================================\n\n");
    if let Some(path) = scan_path {
        text.push_str(&format!("Scan path: {}\n", path.display()));
    }
    if let Some(duration) = duration {
        text.push_str(&format!("Scan duration: {}\n", format_elapsed(duration)));
    }
    if let Some(summary) = scanner::format_fallback_summary(total, access_denied, bulk_scan) {
        text.push_str(&format!("Summary: {summary}\n"));
    }
    text.push_str(&format!("Captured entries: {}\n\n", details.len()));

    if details.is_empty() {
        text.push_str("No compatibility details were recorded.\n");
    } else {
        text.push_str("Technical details:\n\n");
        for (index, detail) in details.iter().enumerate() {
            text.push_str(&format!(
                "{}. [{}] {}\n   {}\n",
                index + 1,
                detail.kind.label(),
                detail.path.display(),
                detail.error
            ));
        }
    }

    std::fs::write(&report_path, text)?;
    Ok(report_path)
}

fn open_text_report(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        Command::new("notepad").arg(path).spawn()?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(path).spawn()?;
        Ok(())
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(path).spawn()?;
        Ok(())
    }
}

/// Reveal a path in the OS file manager, selecting/highlighting it where the
/// platform supports it.
fn reveal_in_file_manager(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        // explorer.exe expects `/select,<path>` as a single argument and
        // wants Windows-style separators.
        Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg("-R").arg(path).spawn()?;
        Ok(())
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // No portable "select" on Linux file managers, so open the
        // containing folder instead.
        let target = path.parent().unwrap_or(path);
        Command::new("xdg-open").arg(target).spawn()?;
        Ok(())
    }
}

/// Result from the background scan thread — includes pre-computed stats
/// so they don't block the UI thread.
struct ScanResult {
    tree: tree::FileNode,
    stats: categories::CategoryStats,
}
use tree::FileNode;
use treemap::TreemapAction;

use deleter::BackgroundDeleter;

#[derive(PartialEq, Clone, Copy)]
enum ViewMode {
    Tree,
    Treemap,
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("disk-cleaner").join("config.json"))
}

fn load_config() -> (Option<PathBuf>, bool) {
    let path = match config_path() {
        Some(p) => p,
        None => return (None, false),
    };
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return (None, false),
    };
    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(j) => j,
        Err(_) => return (None, false),
    };
    let last = json["last_path"].as_str().map(PathBuf::from);
    let show_hidden = json["show_hidden"].as_bool().unwrap_or(false);
    (last, show_hidden)
}

fn save_config(last_path: &std::path::Path, show_hidden: bool) {
    if let Some(config) = config_path() {
        if let Some(parent) = config.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let json = serde_json::json!({
            "last_path": last_path.to_string_lossy(),
            "show_hidden": show_hidden,
        });
        let _ = std::fs::write(config, json.to_string());
    }
}

/// Strip the `\\?\` extended-length path prefix that `canonicalize()` adds
/// on Windows. The prefix is unnecessary for paths under 260 chars and
/// displays poorly in the UI.
#[cfg(windows)]
fn dunce_simplified(p: &std::path::Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        p.to_path_buf()
    }
}

fn print_help() {
    eprintln!("Usage: disk-cleaner [OPTIONS] [PATH]");
    eprintln!();
    eprintln!("Arguments:");
    eprintln!("  [PATH]  Directory to scan on launch");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --screenshot <prefix>  Take screenshots and save as <prefix>_home.png, etc.");
    eprintln!("  -h, --help             Print this help message");
}

fn main() -> eframe::Result {
    let process_start = Instant::now();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut initial_path: Option<PathBuf> = None;
    let mut screenshot_prefix: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "--screenshot" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --screenshot requires a prefix argument");
                    std::process::exit(1);
                }
                screenshot_prefix = Some(args[i].clone());
            }
            other => {
                if other.starts_with('-') {
                    eprintln!("Unknown option: {other}");
                    print_help();
                    std::process::exit(1);
                }
                if initial_path.is_some() {
                    eprintln!("Error: multiple paths provided");
                    print_help();
                    std::process::exit(1);
                }
                let expanded = if other.starts_with("~/") || other == "~" {
                    dirs::home_dir()
                        .map(|h| h.join(other.strip_prefix("~/").unwrap_or("")))
                        .unwrap_or_else(|| PathBuf::from(other))
                } else {
                    PathBuf::from(other)
                };
                // Canonicalize so relative paths (e.g. "../") resolve to
                // absolute paths before we pass them to the scanner.
                // On Windows, strip the \\?\ extended-length prefix that
                // canonicalize() adds — it's unnecessary for normal paths
                // and looks ugly in the UI.
                let p = expanded.canonicalize().unwrap_or(expanded);
                #[cfg(windows)]
                let p = dunce_simplified(&p);
                if !p.is_dir() {
                    eprintln!("Error: not a directory: {other}");
                    std::process::exit(1);
                }
                initial_path = Some(p);
            }
        }
        i += 1;
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_icon(app_icon::generate())
            .with_visible(false), // hidden until first frame renders (avoids white flash)
        ..Default::default()
    };

    eframe::run_native(
        "Disk Cleaner",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
            // Tell the OS to use dark window decorations (title bar on Windows).
            cc.egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
            let mut app = App {
                process_start: Some(process_start),
                screenshot_prefix: screenshot_prefix.clone(),
                screenshot_state: if screenshot_prefix.is_some() {
                    ScreenshotState::WaitingForView
                } else {
                    ScreenshotState::Idle
                },
                ..Default::default()
            };
            if app.screenshot_prefix.is_some() {
                app.show_hidden = true;
            }
            if let Some(path) = initial_path {
                app.start_scan(path);
            }
            Ok(Box::new(app))
        }),
    )
}

#[derive(PartialEq, Clone, Copy)]
enum ScreenshotState {
    Idle,
    WaitingForView,
    /// Wait N frames for rendering to stabilize before capturing.
    WaitFrames(u8),
    Capturing,
    /// Wait for Event::Screenshot to arrive before proceeding.
    WaitingForEvent,
    /// Switch to next view, then capture again.
    NextView(ViewMode),
    /// Open the File Types sidebar, then capture tree_full.
    ShowCategories,
    Done,
}

/// A permanent delete awaiting confirmation. Targets are resolved when the user
/// asks, so a later "Yes" is unaffected by intervening tree changes.
struct PendingDelete {
    path: PathBuf,
    prompt: String,
    targets: Vec<PathBuf>,
}

/// A batch permanent delete awaiting confirmation. Targets are resolved when
/// the user asks, so a later "Yes" is unaffected by intervening selection,
/// visibility, or tree changes.
struct PendingBatchDelete {
    /// Selected row count when the user asked (shown in the prompt).
    item_count: usize,
    targets: Vec<PathBuf>,
}

struct App {
    tree: Option<FileNode>,
    scanning: bool,
    scan_path: Option<PathBuf>,
    scan_progress: Arc<ScanProgress>,
    receiver: Option<mpsc::Receiver<ScanResult>>,
    error: Option<String>,
    confirm_delete: Option<PendingDelete>,
    confirm_batch_delete: Option<PendingBatchDelete>,
    search_query: String,
    /// The search query currently applied to the cached rows (debounced).
    applied_search: String,
    /// When the search text last changed (for debouncing).
    search_changed_at: Option<Instant>,
    focused_path: Option<PathBuf>,
    last_scan_path: Option<PathBuf>,
    view_mode: ViewMode,
    treemap_zoom: Option<PathBuf>,
    treemap_zoom_anim: Option<f64>,
    volumes: Vec<scanner::VolumeInfo>,
    volumes_last_refresh: Option<std::time::Instant>,
    scan_disk_info: Option<(u64, u64)>, // (total, available) for scan path
    scan_is_volume: bool,               // true when scanning a volume root
    category_filter: Option<categories::FileCategory>,
    category_stats: Option<categories::CategoryStats>,
    show_hidden: bool,
    icon_cache: Option<icons::IconCache>,
    last_scan_file_count: u64,
    last_scan_total_size: u64,
    last_scan_duration: Option<Duration>,
    last_scan_fallback_count: u64,
    last_scan_access_denied_fallback_count: u64,
    last_scan_bulk_scan_fallback_count: u64,
    last_scan_fallback_details: Vec<scanner::ScanFallbackDetail>,
    show_categories: bool,
    tree_scroll_to_focus: bool,
    /// Cached visible row list for rendering; rebuilt when dirty.
    cached_rows: Vec<ui::CachedRow>,
    rows_dirty: bool,
    /// Cached treemap layout; rebuilt when treemap_dirty.
    treemap_cache: Option<treemap::TreemapCache>,
    treemap_dirty: bool,
    /// Selection state stored centrally for O(1) clear/select instead of O(n) tree walk.
    selected_paths: HashSet<PathBuf>,
    /// Anchor path for shift+click range selection.
    selection_anchor: Option<PathBuf>,
    /// Tracks which file groups in the tree view are expanded.
    expanded_file_groups: HashSet<PathBuf>,
    /// Process start time for measuring startup latency.
    process_start: Option<Instant>,
    /// Frame-time tracking during scans.
    scan_frame_times: Vec<Duration>,
    /// Start of the current scan for total duration tracking.
    scan_start_time: Option<Instant>,
    /// Screenshot mode: file prefix for output PNGs.
    screenshot_prefix: Option<String>,
    /// Screenshot state machine.
    screenshot_state: ScreenshotState,
    /// Number of screenshots saved (for tracking completion).
    screenshots_saved: u8,
    /// Background deletion state.
    deleter: BackgroundDeleter,
    /// Last OS theme observed, used to re-assert the dark title bar on Windows
    /// when the system theme changes (see `keep_titlebar_dark`).
    last_os_theme: Option<egui::Theme>,
    /// Whether the window was focused on the previous frame, used to re-assert
    /// the dark title bar when focus is regained.
    was_focused: bool,
}

impl Default for App {
    fn default() -> Self {
        let (last_scan_path, show_hidden) = load_config();
        Self {
            tree: None,
            scanning: false,
            scan_path: None,
            scan_progress: Arc::new(ScanProgress {
                file_count: 0.into(),
                total_size: 0.into(),
                fallback_count: 0.into(),
                access_denied_fallback_count: 0.into(),
                bulk_scan_fallback_count: 0.into(),
                fallback_details: std::sync::Mutex::new(Vec::new()),
                cancelled: false.into(),
            }),
            receiver: None,
            error: None,
            confirm_delete: None,
            confirm_batch_delete: None,
            search_query: String::new(),
            applied_search: String::new(),
            search_changed_at: None,
            focused_path: None,
            last_scan_path,
            view_mode: ViewMode::Tree,
            treemap_zoom: None,
            treemap_zoom_anim: None,
            volumes: scanner::list_volumes(),
            volumes_last_refresh: Some(std::time::Instant::now()),
            scan_disk_info: None,
            scan_is_volume: false,
            category_filter: None,
            category_stats: None,
            show_hidden,
            icon_cache: None,
            last_scan_file_count: 0,
            last_scan_total_size: 0,
            last_scan_duration: None,
            last_scan_fallback_count: 0,
            last_scan_access_denied_fallback_count: 0,
            last_scan_bulk_scan_fallback_count: 0,
            last_scan_fallback_details: Vec::new(),
            show_categories: false,
            tree_scroll_to_focus: false,
            cached_rows: Vec::new(),
            rows_dirty: true,
            treemap_cache: None,
            treemap_dirty: true,
            selected_paths: HashSet::new(),
            selection_anchor: None,
            expanded_file_groups: HashSet::new(),
            process_start: None,
            scan_frame_times: Vec::new(),
            scan_start_time: None,
            screenshot_prefix: None,
            screenshot_state: ScreenshotState::Idle,
            screenshots_saved: 0,
            deleter: BackgroundDeleter::default(),
            last_os_theme: None,
            was_focused: true,
        }
    }
}

impl App {
    /// Keep the OS window title bar dark.
    ///
    /// eframe/egui only applies the window decoration theme when a
    /// `ViewportCommand::SetTheme` is sent; it never re-applies it in response
    /// to OS events. On Windows the forced dark title bar is dropped back to
    /// the system default when the OS theme changes or the window regains
    /// focus, so we re-assert it on those transitions. This is event-driven
    /// (fires only on a theme or focus change), not every frame.
    fn keep_titlebar_dark(&mut self, ctx: &egui::Context) {
        let (os_theme, focused) = ctx.input(|i| {
            (
                i.raw.system_theme,
                i.viewport().focused.unwrap_or(self.was_focused),
            )
        });

        let theme_changed = os_theme != self.last_os_theme;
        let focus_regained = focused && !self.was_focused;
        self.last_os_theme = os_theme;
        self.was_focused = focused;

        if theme_changed || focus_regained {
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Dark));
        }
    }

    fn cancel_scan(&mut self) {
        self.scan_progress.cancelled.store(true, Ordering::Relaxed);
        self.scanning = false;
        self.receiver = None;
        self.scan_start_time = None;
    }

    fn open_fallback_report(&mut self) {
        match write_fallback_report(
            self.scan_path.as_deref(),
            self.last_scan_duration,
            self.last_scan_fallback_count,
            self.last_scan_access_denied_fallback_count,
            self.last_scan_bulk_scan_fallback_count,
            &self.last_scan_fallback_details,
        )
        .and_then(|path| open_text_report(&path))
        {
            Ok(()) => {}
            Err(err) => {
                self.error = Some(format!("Could not open compatibility report: {err}"));
            }
        }
    }

    fn start_scan(&mut self, path: PathBuf) {
        // Cancel any in-progress scan so its threads release the rayon pool
        self.scan_progress.cancelled.store(true, Ordering::Relaxed);

        save_config(&path, self.show_hidden);
        self.last_scan_path = Some(path.clone());
        self.scanning = true;
        self.error = None;
        self.tree = None;
        self.selected_paths.clear();
        self.selection_anchor = None;
        self.scan_path = Some(path.clone());
        self.scan_disk_info = scanner::disk_space(&path);
        self.scan_is_volume = self.volumes.iter().any(|v| v.path == path);

        let progress = Arc::new(ScanProgress {
            file_count: 0.into(),
            total_size: 0.into(),
            fallback_count: 0.into(),
            access_denied_fallback_count: 0.into(),
            bulk_scan_fallback_count: 0.into(),
            fallback_details: std::sync::Mutex::new(Vec::new()),
            cancelled: false.into(),
        });
        self.scan_progress = progress.clone();

        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);

        self.scan_start_time = Some(Instant::now());
        self.last_scan_duration = None;
        self.last_scan_fallback_count = 0;
        self.last_scan_access_denied_fallback_count = 0;
        self.last_scan_bulk_scan_fallback_count = 0;
        self.last_scan_fallback_details.clear();
        self.scan_frame_times.clear();

        thread::spawn(move || {
            let tree = scanner::scan_directory(&path, progress);
            let stats = categories::compute_stats(&tree);
            let _ = tx.send(ScanResult { tree, stats });
        });
    }

    fn rebuild_rows_if_dirty(&mut self) {
        if !self.rows_dirty {
            return;
        }

        let text_cache = if !self.applied_search.is_empty() {
            self.tree
                .as_ref()
                .map(|t| ui::build_text_match_cache(t, &self.applied_search))
        } else {
            None
        };

        let cat_cache = if let Some(cat) = self.category_filter {
            self.tree
                .as_ref()
                .map(|t| ui::build_category_match_cache(t, cat))
        } else {
            None
        };

        // Drop old rows before building new ones to avoid holding two full
        // Vec<CachedRow> in memory simultaneously (OOM risk on large trees).
        self.cached_rows = Vec::new();

        if let Some(ref tree) = self.tree {
            self.cached_rows = ui::collect_cached_rows(
                tree,
                &self.applied_search,
                self.category_filter,
                self.show_hidden,
                text_cache.as_ref(),
                cat_cache.as_ref(),
                Some(&self.expanded_file_groups),
            );
        }
        self.rows_dirty = false;
    }

    /// Mark both tree-view and treemap caches as needing rebuild.
    fn mark_dirty(&mut self) {
        self.rows_dirty = true;
        self.treemap_dirty = true;
    }

    fn batch_trash_selected(&mut self) {
        let paths: Vec<PathBuf> = self.selected_paths.drain().collect();
        let targets = self.batch_targets(paths);
        self.deleter.start(targets, true);
    }

    /// Expand one tree-row path into the real paths it represents.
    fn deletion_targets(&self, path: &Path) -> Vec<PathBuf> {
        resolve_deletion_targets(
            &self.cached_rows,
            self.tree.as_ref(),
            path,
            self.show_hidden,
        )
    }

    /// Expand a batch of selected row paths into a de-duplicated target list.
    fn batch_targets(&self, paths: Vec<PathBuf>) -> Vec<PathBuf> {
        resolve_batch_targets(
            &self.cached_rows,
            self.tree.as_ref(),
            paths,
            self.show_hidden,
        )
    }

    /// Build a confirmed batch-delete plan from the current selection,
    /// resolving targets now so a later "Yes" click is unaffected by
    /// intervening selection, visibility, or tree changes.
    fn pending_batch_delete(&self) -> PendingBatchDelete {
        let paths: Vec<PathBuf> = self.selected_paths.iter().cloned().collect();
        PendingBatchDelete {
            item_count: paths.len(),
            targets: self.batch_targets(paths),
        }
    }

    /// Build a confirmed-delete plan, resolving targets now so a later "Yes"
    /// click is unaffected by intervening scroll, collapse, or rescan.
    fn pending_delete_for(&self, path: &Path) -> PendingDelete {
        let targets = self.deletion_targets(path);
        let is_group = row_is_file_group(&self.cached_rows, path);
        let prompt = if is_group {
            let dir = path.parent().unwrap_or(path);
            format!(
                "Permanently delete {} files in\n{}",
                targets.len(),
                dir.display()
            )
        } else {
            format!("Permanently delete?\n{}", path.display())
        };
        PendingDelete {
            path: path.to_path_buf(),
            prompt,
            targets,
        }
    }

    /// Poll for background deletion completion and apply results to the tree.
    fn poll_delete_completion(&mut self) {
        if let deleter::PollResult::Done(results) = self.deleter.poll() {
            let mut deleted_paths = Vec::new();
            for (path, err) in results {
                if let Some(msg) = err {
                    self.error = Some(format!("Delete failed: {msg}"));
                } else {
                    if let Some(ref mut tree) = self.tree {
                        ui::remove_node(tree, &path);
                        self.mark_dirty();
                    }
                    deleted_paths.push(path);
                }
            }
            if !deleted_paths.is_empty() {
                self.refresh_disk_info();
            }
        }
    }

    /// Re-query disk space so the status bar reflects freed space after deletions.
    fn refresh_disk_info(&mut self) {
        if let Some(ref path) = self.scan_path {
            self.scan_disk_info = scanner::disk_space(path);
        }
    }
}

/// True if `path` is a synthetic file-group row among the rendered rows.
///
/// The single source of truth for group identity: never the path string or
/// the filesystem, so a real entry named `__file_group__` is never mistaken
/// for a group.
fn row_is_file_group(rows: &[ui::CachedRow], path: &Path) -> bool {
    rows.iter().any(|r| r.is_file_group && r.path == path)
}

/// Expand a batch of selected row paths into a de-duplicated target list.
/// Group-row paths are collected once so each lookup is O(1), keeping batch
/// resolution O(rows + selected) rather than O(rows*selected).
fn resolve_batch_targets(
    rows: &[ui::CachedRow],
    tree: Option<&FileNode>,
    paths: Vec<PathBuf>,
    show_hidden: bool,
) -> Vec<PathBuf> {
    let group_paths: HashSet<&Path> = rows
        .iter()
        .filter(|r| r.is_file_group)
        .map(|r| r.path.as_path())
        .collect();
    let mut targets: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for p in paths {
        if group_paths.contains(p.as_path()) {
            if let (Some(dir), Some(tree)) = (p.parent(), tree) {
                for f in ui::file_group_files(tree, dir, show_hidden) {
                    if seen.insert(f.clone()) {
                        targets.push(f);
                    }
                }
            }
        } else if seen.insert(p.clone()) {
            targets.push(p);
        }
    }
    targets
}

/// Expand a tree-row path into the real paths a delete should touch.
///
/// Identity comes from the rendered rows, never the filesystem: `path` is a
/// group only if a rendered row carries it with `is_file_group`. A group
/// expands to the loose files in its parent dir; anything else maps to itself.
fn resolve_deletion_targets(
    rows: &[ui::CachedRow],
    tree: Option<&FileNode>,
    path: &Path,
    show_hidden: bool,
) -> Vec<PathBuf> {
    if row_is_file_group(rows, path) {
        match (path.parent(), tree) {
            (Some(dir), Some(tree)) => ui::file_group_files(tree, dir, show_hidden),
            _ => Vec::new(),
        }
    } else {
        vec![path.to_path_buf()]
    }
}

fn save_screenshot_png(
    color_image: &egui::ColorImage,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut rgba = Vec::with_capacity(color_image.pixels.len() * 4);
    for pixel in &color_image.pixels {
        rgba.push(pixel.r());
        rgba.push(pixel.g());
        rgba.push(pixel.b());
        rgba.push(pixel.a());
    }
    image::save_buffer(
        path,
        &rgba,
        color_image.width() as u32,
        color_image.height() as u32,
        image::ColorType::Rgba8,
    )?;
    eprintln!("[screenshot] saved: {path}");
    Ok(())
}

impl eframe::App for App {
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        // Match the panel fill so sub-pixel gaps between panels don't
        // expose the default (darker) clear color as a shadow line.
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn ui(&mut self, _ui: &mut egui::Ui, _frame: &mut eframe::Frame) {}

    #[allow(deprecated)]
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let frame_start = Instant::now();

        // Show window on first frame (was created hidden to avoid white flash)
        if let Some(start) = self.process_start.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            if debug_enabled() {
                eprintln!("[perf] startup → first frame: {:?}", start.elapsed());
            }
        }

        self.keep_titlebar_dark(ctx);

        // Apply debounced search query after 150ms of no typing
        if let Some(changed_at) = self.search_changed_at {
            if changed_at.elapsed() >= Duration::from_millis(150) {
                self.applied_search = self.search_query.clone();
                self.search_changed_at = None;
                self.rows_dirty = true;
                // Treemap doesn't filter by search text, so no treemap_dirty
            } else {
                let remaining = Duration::from_millis(150).saturating_sub(changed_at.elapsed());
                ctx.request_repaint_after(remaining);
            }
        }

        // Check if scan completed
        if let Some(ref rx) = self.receiver
            && let Ok(result) = rx.try_recv()
        {
            self.category_stats = Some(result.stats);
            self.tree = Some(result.tree);
            if let Some(ref mut t) = self.tree {
                tree::auto_expand(t, 0, 2);
            }
            self.last_scan_file_count = self.scan_progress.file_count.load(Ordering::Relaxed);
            self.last_scan_total_size = self.scan_progress.total_size.load(Ordering::Relaxed);
            self.last_scan_fallback_count =
                self.scan_progress.fallback_count.load(Ordering::Relaxed);
            self.last_scan_access_denied_fallback_count = self
                .scan_progress
                .access_denied_fallback_count
                .load(Ordering::Relaxed);
            self.last_scan_bulk_scan_fallback_count = self
                .scan_progress
                .bulk_scan_fallback_count
                .load(Ordering::Relaxed);
            self.last_scan_fallback_details = self.scan_progress.fallback_details_snapshot();
            self.scanning = false;
            self.receiver = None;
            self.category_filter = None;
            self.mark_dirty();

            // Report frame-time stats for the scan
            if let Some(scan_start) = self.scan_start_time.take() {
                let scan_dur = scan_start.elapsed();
                self.last_scan_duration = Some(scan_dur);
                if debug_enabled() {
                    let ft = &mut self.scan_frame_times;
                    ft.sort();
                    let n = ft.len();
                    if n > 0 {
                        let avg: Duration = ft.iter().sum::<Duration>() / n as u32;
                        let p99 = ft[((n as f64 * 0.99) as usize).min(n - 1)];
                        let over = ft
                            .iter()
                            .filter(|d| **d > Duration::from_millis(16))
                            .count();
                        eprintln!(
                            "[perf] scan done in {scan_dur:?} ({} files)",
                            self.last_scan_file_count
                        );
                        if self.last_scan_fallback_count > 0 {
                            eprintln!(
                                "[perf] windows bulk fallbacks: {}",
                                scanner::format_fallback_summary(
                                    self.last_scan_fallback_count,
                                    self.last_scan_access_denied_fallback_count,
                                    self.last_scan_bulk_scan_fallback_count
                                )
                                .unwrap_or_else(|| self.last_scan_fallback_count.to_string())
                            );
                        }
                        eprintln!(
                            "[perf] frame times (n={n}): min={:?} med={:?} avg={avg:?} p99={p99:?} max={:?}",
                            ft[0],
                            ft[n / 2],
                            ft[n - 1]
                        );
                        eprintln!(
                            "[perf] frames >16ms: {over}/{n} ({:.1}%)",
                            over as f64 / n as f64 * 100.0
                        );
                    }
                }
                self.scan_frame_times.clear();
            }
        }

        // Check if background deletion completed
        self.poll_delete_completion();
        if self.deleter.is_active() {
            ctx.request_repaint();
        }

        // ── Screenshot state machine ──
        if self.screenshot_prefix.is_some() {
            // Handle incoming screenshot events
            let got_screenshot = ctx.input(|i| {
                i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::Screenshot { .. }))
            });

            if got_screenshot {
                ctx.input(|i| {
                    for event in &i.events {
                        if let egui::Event::Screenshot { image, .. } = event {
                            let prefix = self.screenshot_prefix.as_ref().unwrap();
                            let suffix = match self.view_mode {
                                ViewMode::Tree if self.show_categories => "tree_full",
                                ViewMode::Tree => "tree",
                                ViewMode::Treemap => "treemap",
                            };
                            let label = if self.tree.is_none() && !self.scanning {
                                "home"
                            } else {
                                suffix
                            };
                            let path = format!("{prefix}_{label}.png");
                            if let Err(e) = save_screenshot_png(image, &path) {
                                eprintln!("[screenshot] error: {e}");
                            }
                            self.screenshots_saved += 1;
                        }
                    }
                });
            }

            match self.screenshot_state {
                ScreenshotState::WaitingForView => {
                    if !self.scanning {
                        self.screenshot_state = ScreenshotState::WaitFrames(5);
                    }
                }
                ScreenshotState::WaitFrames(0) => {
                    self.screenshot_state = ScreenshotState::Capturing;
                    ctx.request_repaint();
                }
                ScreenshotState::WaitFrames(n) => {
                    self.screenshot_state = ScreenshotState::WaitFrames(n - 1);
                    ctx.request_repaint();
                }
                ScreenshotState::Capturing => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                        egui::UserData::default(),
                    ));
                    self.screenshot_state = ScreenshotState::WaitingForEvent;
                    ctx.request_repaint();
                }
                ScreenshotState::WaitingForEvent => {
                    if got_screenshot {
                        // Screenshot saved — determine what to do next
                        if self.tree.is_none() {
                            self.screenshot_state = ScreenshotState::Done;
                        } else {
                            match self.view_mode {
                                ViewMode::Tree if !self.show_categories => {
                                    self.screenshot_state = ScreenshotState::ShowCategories;
                                }
                                ViewMode::Tree => {
                                    // tree_full captured; close sidebar and move on
                                    self.show_categories = false;
                                    self.screenshot_state =
                                        ScreenshotState::NextView(ViewMode::Treemap);
                                }
                                ViewMode::Treemap => {
                                    self.screenshot_state = ScreenshotState::Done;
                                }
                            }
                        }
                    }
                    ctx.request_repaint();
                }
                ScreenshotState::ShowCategories => {
                    self.show_categories = true;
                    self.screenshot_state = ScreenshotState::WaitFrames(5);
                    ctx.request_repaint();
                }
                ScreenshotState::NextView(next) => {
                    self.view_mode = next;
                    self.screenshot_state = ScreenshotState::WaitFrames(5);
                    ctx.request_repaint();
                }
                ScreenshotState::Done => {
                    eprintln!(
                        "[screenshot] done — {} screenshots saved",
                        self.screenshots_saved
                    );
                    self.screenshot_state = ScreenshotState::Idle;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                ScreenshotState::Idle => {}
            }
        }

        // Keyboard shortcuts (only when no text input is focused)
        let has_text_focus = ctx.memory(|m| m.focused().is_some());
        if !has_text_focus {
            // Ensure visible path cache is fresh before keyboard nav
            self.rebuild_rows_if_dirty();

            // Arrow key navigation
            let (up, down, left, right) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::ArrowLeft),
                    i.key_pressed(egui::Key::ArrowRight),
                )
            });

            if up || down {
                let rows = &self.cached_rows;
                if !rows.is_empty() {
                    if let Some(ref focused) = self.focused_path {
                        if let Some(idx) = rows.iter().position(|r| &r.path == focused) {
                            let new_idx = if up {
                                idx.saturating_sub(1)
                            } else {
                                (idx + 1).min(rows.len() - 1)
                            };
                            self.focused_path = Some(rows[new_idx].path.clone());
                        }
                    } else {
                        self.focused_path = Some(rows[0].path.clone());
                    }
                    // Clear selection so only the focused row is highlighted
                    self.selected_paths.clear();
                    self.tree_scroll_to_focus = true;
                }
            }

            if (left || right)
                && let Some(ref focused) = self.focused_path.clone()
            {
                // Row identity, not the path string: a real entry named
                // __file_group__ must get ordinary navigation.
                let is_file_group = row_is_file_group(&self.cached_rows, focused);

                if is_file_group {
                    // File group path is parent_dir/__file_group__; key is parent_dir
                    if let Some(parent_dir) = focused.parent() {
                        let key = parent_dir.to_path_buf();
                        let group_expanded = self.expanded_file_groups.contains(&key);
                        if left {
                            if group_expanded {
                                self.expanded_file_groups.remove(&key);
                                self.mark_dirty();
                            } else {
                                // Already collapsed — navigate to parent directory
                                self.focused_path = Some(parent_dir.to_path_buf());
                                self.selected_paths.clear();
                                self.tree_scroll_to_focus = true;
                            }
                        } else if right {
                            if !group_expanded {
                                self.expanded_file_groups.insert(key);
                                self.mark_dirty();
                            } else {
                                // Already expanded — move focus to first child row
                                let rows = &self.cached_rows;
                                if let Some(idx) = rows.iter().position(|r| &r.path == focused)
                                    && idx + 1 < rows.len()
                                {
                                    self.focused_path = Some(rows[idx + 1].path.clone());
                                    self.selected_paths.clear();
                                    self.tree_scroll_to_focus = true;
                                }
                            }
                        }
                    }
                } else if let Some(ref mut tree) = self.tree
                    && let Some((is_dir, expanded, has_children)) =
                        ui::find_node_info(tree, focused)
                {
                    if left {
                        if is_dir && expanded {
                            ui::set_expanded(tree, focused, false);
                            self.mark_dirty();
                        } else if let Some(parent) = ui::find_parent_path(tree, focused) {
                            self.focused_path = Some(parent);
                            self.selected_paths.clear();
                            self.tree_scroll_to_focus = true;
                        }
                    } else if right {
                        if is_dir && !expanded && has_children {
                            ui::set_expanded(tree, focused, true);
                            self.mark_dirty();
                        } else if is_dir && expanded {
                            let rows = &self.cached_rows;
                            if let Some(idx) = rows.iter().position(|r| &r.path == focused)
                                && idx + 1 < rows.len()
                            {
                                self.focused_path = Some(rows[idx + 1].path.clone());
                                self.selected_paths.clear();
                                self.tree_scroll_to_focus = true;
                            }
                        }
                    }
                }
            }

            if let Some(ref focused) = self.focused_path.clone() {
                let (space, shift_del, del) = ctx.input(|i| {
                    (
                        i.key_pressed(egui::Key::Space),
                        i.modifiers.shift && i.key_pressed(egui::Key::Delete),
                        !i.modifiers.shift && i.key_pressed(egui::Key::Delete),
                    )
                });
                if space {
                    if let Some(ref mut tree) = self.tree {
                        ui::toggle_expand(tree, focused);
                        self.mark_dirty();
                    }
                } else if shift_del {
                    self.confirm_delete = Some(self.pending_delete_for(focused));
                } else if del {
                    let targets = self.deletion_targets(focused);
                    self.selected_paths.remove(focused);
                    self.deleter.start(targets, true);
                    self.focused_path = None;
                }
            }
        }

        // Batch delete confirmation dialog
        let mut do_batch_delete = false;
        let mut close_batch_dialog = false;

        if let Some(ref pending) = self.confirm_batch_delete {
            let enter_pressed = ctx.input(|i| i.key_pressed(egui::Key::Enter));
            egui::Window::new("Confirm Batch Delete")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Permanently delete {} selected item(s)? This cannot be undone.",
                        pending.item_count
                    ));
                    ui.horizontal(|ui| {
                        let delete_btn = egui::Button::new(
                            egui::RichText::new("Yes, delete all").color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(220, 50, 50));
                        if ui.add(delete_btn).clicked() || enter_pressed {
                            do_batch_delete = true;
                            close_batch_dialog = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close_batch_dialog = true;
                        }
                    });
                });
        }

        if do_batch_delete {
            // Use the plan captured when the user asked, not a fresh lookup.
            if let Some(pending) = self.confirm_batch_delete.take() {
                self.selected_paths.clear();
                self.deleter.start(pending.targets, false);
            }
        } else if close_batch_dialog {
            self.confirm_batch_delete = None;
        }

        // Single-item delete confirmation dialog
        let mut do_delete = false;
        let mut close_dialog = false;

        if let Some(ref pending) = self.confirm_delete {
            let enter_pressed = ctx.input(|i| i.key_pressed(egui::Key::Enter));
            egui::Window::new("Confirm Delete")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(&pending.prompt);
                    ui.horizontal(|ui| {
                        let delete_btn = egui::Button::new(
                            egui::RichText::new("Yes, delete").color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(220, 50, 50));
                        if ui.add(delete_btn).clicked() || enter_pressed {
                            do_delete = true;
                            close_dialog = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close_dialog = true;
                        }
                    });
                });
        }

        if do_delete {
            // Use the plan captured when the user asked, not a fresh lookup.
            if let Some(pending) = self.confirm_delete.take() {
                self.selected_paths.remove(&pending.path);
                self.deleter.start(pending.targets, false);
            }
        } else if close_dialog {
            self.confirm_delete = None;
        }

        // Top panel with toolbar (hidden on home page where it only has "Open Directory")
        let show_toolbar = self.tree.is_some() || self.scanning;
        if show_toolbar {
            egui::TopBottomPanel::top("toolbar")
                .show_separator_line(false)
                .default_size(28.0) // 24px interact_size + 4px inner_margin
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        // Standardize widget height so buttons and selectable labels align
                        ui.spacing_mut().interact_size.y = 24.0;

                        if ui.button("Open Directory...").clicked()
                            && let Some(path) = rfd::FileDialog::new().pick_folder()
                        {
                            self.start_scan(path);
                        }

                        if self.tree.is_some()
                            && ui.button("Re-scan").clicked()
                            && let Some(path) = self.scan_path.clone()
                        {
                            self.start_scan(path);
                        }

                        // View mode toggle
                        if self.tree.is_some() {
                            ui.separator();
                            for (label, mode) in
                                [("Tree", ViewMode::Tree), ("Treemap", ViewMode::Treemap)]
                            {
                                let is_active = self.view_mode == mode;
                                let text = if is_active {
                                    egui::RichText::new(label).strong().size(14.0)
                                } else {
                                    egui::RichText::new(label)
                                        .size(14.0)
                                        .color(ui.visuals().weak_text_color())
                                };

                                let btn = egui::Button::new(text)
                                    .frame(false)
                                    .min_size(egui::vec2(0.0, 24.0));
                                let response = ui.add(btn);

                                // Draw underline for active tab
                                if is_active {
                                    let rect = response.rect;
                                    let painter = ui.painter();
                                    let accent = egui::Color32::from_rgb(100, 180, 255);
                                    painter.rect_filled(
                                        egui::Rect::from_min_size(
                                            egui::pos2(rect.left(), rect.bottom() - 2.0),
                                            egui::vec2(rect.width(), 2.0),
                                        ),
                                        0.0,
                                        accent,
                                    );
                                }

                                if response.clicked() {
                                    self.view_mode = mode;
                                }
                            }
                        }

                        // Search/filter bar — hidden: filter feature crashes (DIS-253)
                        // if self.tree.is_some() {
                        //     ui.separator();
                        //     ui.label("Filter:");
                        //     let response = ui.add(
                        //         egui::TextEdit::singleline(&mut self.search_query)
                        //             .hint_text("file name...")
                        //             .desired_width(200.0),
                        //     );
                        //     if response.changed() {
                        //         self.search_query = self.search_query.to_lowercase();
                        //         self.search_changed_at = Some(Instant::now());
                        //     }
                        //     if !self.search_query.is_empty() && ui.small_button("×").clicked() {
                        //         self.search_query.clear();
                        //         self.applied_search.clear();
                        //         self.search_changed_at = None;
                        //         self.rows_dirty = true;
                        //     }
                        // }

                        // Hidden files toggle
                        if self.tree.is_some() {
                            ui.separator();
                            if ui
                                .selectable_label(self.show_hidden, "Show hidden")
                                .clicked()
                            {
                                self.show_hidden = !self.show_hidden;
                                self.mark_dirty();
                                // Persist preference
                                if let Some(ref path) = self.last_scan_path {
                                    save_config(path, self.show_hidden);
                                }
                                // Recompute stats
                                if let Some(ref tree) = self.tree {
                                    self.category_stats = Some(categories::compute_stats(tree));
                                }
                            }
                        }

                        // File types panel toggle
                        if self.tree.is_some() {
                            ui.separator();
                            if ui
                                .selectable_label(self.show_categories, "File Types")
                                .clicked()
                            {
                                self.show_categories = !self.show_categories;
                                if !self.show_categories {
                                    self.category_filter = None;
                                    self.mark_dirty();
                                }
                            }
                        }

                        if self.scanning {
                            // Throttle repaints during scanning — progress counter doesn't
                            // need 1000fps. 100ms (~10fps) keeps the UI responsive without
                            // starving scan threads or causing frame-pacing jank.
                            ctx.request_repaint_after(Duration::from_millis(100));
                        }

                        if let Some(ref err) = self.error {
                            ui.colored_label(egui::Color32::RED, err);
                        }
                    });
                });
        } // show_toolbar

        // Category side panel (toggled via toolbar button)
        if self.tree.is_some() && !self.scanning && self.show_categories {
            egui::SidePanel::left("categories")
                .resizable(true)
                .default_width(200.0)
                .min_width(160.0)
                .show(ctx, |ui| {
                    ui.heading("File Types");
                    ui.add_space(4.0);

                    if let Some(ref stats) = self.category_stats {
                        let total_size: u64 = stats.entries.iter().map(|e| e.1).sum();

                        // "All files" option to clear filter
                        let all_selected = self.category_filter.is_none();
                        if ui
                            .selectable_label(
                                all_selected,
                                egui::RichText::new("All files").strong(),
                            )
                            .clicked()
                        {
                            self.category_filter = None;
                            self.rows_dirty = true;
                            self.treemap_dirty = true;
                        }

                        ui.add_space(4.0);
                        ui.separator();
                        ui.add_space(4.0);

                        for &(cat, size, count) in &stats.entries {
                            let is_active =
                                self.category_filter.as_ref().is_some_and(|f| *f == cat);
                            let fraction = if total_size > 0 {
                                size as f32 / total_size as f32
                            } else {
                                0.0
                            };

                            let response = ui
                                .horizontal(|ui| {
                                    // Color swatch
                                    let (swatch_rect, _) = ui.allocate_exact_size(
                                        egui::vec2(12.0, 12.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().rect_filled(swatch_rect, 2.0, cat.color());

                                    let label = if is_active {
                                        egui::RichText::new(cat.label()).strong()
                                    } else {
                                        egui::RichText::new(cat.label())
                                    };
                                    let _ = ui.selectable_label(is_active, label);
                                })
                                .response;

                            // Size bar under the label
                            let bar_height = 4.0;
                            let (bar_rect, _) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), bar_height),
                                egui::Sense::hover(),
                            );
                            let painter = ui.painter();
                            painter.rect_filled(bar_rect, 1.0, ui.visuals().extreme_bg_color);
                            let fill_w = (bar_rect.width() * fraction.clamp(0.0, 1.0)).max(1.0);
                            let fill_rect = egui::Rect::from_min_size(
                                bar_rect.min,
                                egui::vec2(fill_w, bar_height),
                            );
                            painter.rect_filled(fill_rect, 1.0, cat.color());

                            ui.horizontal(|ui| {
                                ui.small(format!(
                                    "{} | {} files | {:.1}%",
                                    bytesize::ByteSize::b(size),
                                    count,
                                    fraction * 100.0
                                ));
                            });

                            if response.clicked() {
                                if is_active {
                                    self.category_filter = None;
                                } else {
                                    self.category_filter = Some(cat);
                                }
                                self.rows_dirty = true;
                                self.treemap_dirty = true;
                            }

                            ui.add_space(2.0);
                        }
                    }
                });
        }

        let mut open_fallback_report = false;

        // Bottom status bar with scan info + selection + keyboard hints
        egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                // Left: static scan summary.
                if self.tree.is_some() && !self.scanning {
                    if self.scan_path.is_some() {
                        let summary = format!(
                            "{} files, {}",
                            self.last_scan_file_count,
                            bytesize::ByteSize::b(self.last_scan_total_size)
                        );
                        ui.label(egui::RichText::new(summary).small());
                    }
                } else if let Some(ref path) = self.scan_path
                    && !self.scanning
                    && self.last_scan_file_count > 0
                {
                    ui.label(
                        egui::RichText::new(format!(
                            "Scanned: {} ({} files, {})",
                            path.display(),
                            self.last_scan_file_count,
                            bytesize::ByteSize::b(self.last_scan_total_size)
                        ))
                        .small(),
                    );
                }

                // Right: keyboard hints + disk stats + version
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                            .small()
                            .weak(),
                    );

                    // Disk space info
                    if self.tree.is_some() && !self.scanning {
                        if let Some(duration) = self.last_scan_duration {
                            ui.label(
                                egui::RichText::new(format!("Scan: {}", format_elapsed(duration)))
                                    .small()
                                    .weak(),
                            );
                            ui.separator();
                        }

                        if self.last_scan_fallback_count > 0 {
                            let button = egui::Button::new(
                                egui::RichText::new(format!("⚠ {}", self.last_scan_fallback_count))
                                    .small()
                                    .color(egui::Color32::from_rgb(230, 200, 80)),
                            )
                            .frame(false);
                            let hover = scanner::format_fallback_summary(
                                self.last_scan_fallback_count,
                                self.last_scan_access_denied_fallback_count,
                                self.last_scan_bulk_scan_fallback_count,
                            )
                            .unwrap_or_else(|| {
                                format!(
                                    "{} fallback{}",
                                    self.last_scan_fallback_count,
                                    if self.last_scan_fallback_count == 1 {
                                        ""
                                    } else {
                                        "s"
                                    }
                                )
                            });
                            let response = ui
                                .add(button)
                                .on_hover_text(format!("{hover}\nClick to open details"));
                            if response.clicked() {
                                open_fallback_report = true;
                            }
                            ui.separator();
                        }

                        if let Some((total, available)) = self.scan_disk_info {
                            let used = total.saturating_sub(available);
                            ui.label(
                                egui::RichText::new(format!(
                                    "Disk: {} used / {} ({} free)",
                                    bytesize::ByteSize::b(used),
                                    bytesize::ByteSize::b(total),
                                    bytesize::ByteSize::b(available),
                                ))
                                .small(),
                            );
                            ui.separator();
                        }

                        // Keyboard hints
                        ui.label(
                            egui::RichText::new("Arrow keys navigate  Space expand  Del trash")
                                .small()
                                .weak(),
                        );
                        ui.separator();
                    }
                });
            });
        });

        if open_fallback_report {
            self.open_fallback_report();
        }

        // Main content
        egui::CentralPanel::default().show(ctx, |ui| {
            // Full-page scanning UI
            if self.scanning {
                ui.vertical_centered(|ui| {
                    let available = ui.available_height();
                    ui.add_space(available * 0.3);

                    ui.spinner();
                    ui.add_space(12.0);

                    let path_str = self
                        .scan_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    ui.heading("Scanning");
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(&path_str).weak().size(13.0));
                    ui.add_space(16.0);

                    let files = self.scan_progress.file_count.load(Ordering::Relaxed);
                    let size = self.scan_progress.total_size.load(Ordering::Relaxed);
                    let size_str = bytesize::ByteSize::b(size).to_string();
                    ui.label(format!("{files} files — {size_str}"));

                    // Progress bar: estimated scan progress based on used disk space
                    if self.scan_is_volume
                        && let Some((total, available)) = self.scan_disk_info
                    {
                        let used = total.saturating_sub(available);
                        if used > 0 {
                            ui.add_space(12.0);
                            let fraction = (size as f32 / used as f32).clamp(0.0, 1.0);
                            let bar = egui::ProgressBar::new(fraction).desired_width(300.0);
                            ui.add(bar);
                        }
                    }

                    // Elapsed time
                    if let Some(start) = self.scan_start_time {
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(format!(
                                "Elapsed: {}",
                                format_elapsed(start.elapsed())
                            ))
                            .weak()
                            .size(13.0),
                        );
                    }

                    ui.add_space(24.0);
                    let cancel_btn = egui::Button::new(egui::RichText::new("Cancel").size(15.0))
                        .min_size(egui::vec2(120.0, 36.0));
                    if ui.add(cancel_btn).clicked() {
                        self.cancel_scan();
                    }
                });
                return;
            }

            if self.tree.is_none() {
                // Refresh volume list every 5 seconds
                let should_refresh = self
                    .volumes_last_refresh
                    .is_none_or(|t| t.elapsed().as_secs() >= 5);
                if should_refresh {
                    self.volumes = scanner::list_volumes();
                    self.volumes_last_refresh = Some(std::time::Instant::now());
                }

                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    ui.heading("Disk Cleaner");
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("Select a volume to scan and reclaim disk space")
                            .weak()
                            .size(13.0),
                    );
                    ui.add_space(20.0);

                    // Volume list
                    if !self.volumes.is_empty() {
                        ui.label(egui::RichText::new("Volumes").strong().size(14.0));
                        ui.add_space(8.0);

                        let mut scan_path: Option<PathBuf> = None;
                        for vol in &self.volumes {
                            let used = vol.total_bytes.saturating_sub(vol.available_bytes);
                            let fraction = if vol.total_bytes > 0 {
                                used as f32 / vol.total_bytes as f32
                            } else {
                                0.0
                            };

                            let card_response = egui::Frame::group(ui.style())
                                .inner_margin(12.0)
                                .show(ui, |ui| {
                                    ui.set_width(400.0);
                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new("\u{1F4BD}").size(16.0));
                                        ui.strong(&vol.name);
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.label(format!(
                                                    "{} used of {}",
                                                    bytesize::ByteSize::b(used),
                                                    bytesize::ByteSize::b(vol.total_bytes),
                                                ));
                                            },
                                        );
                                    });

                                    // Capacity bar
                                    let bar_height = 14.0;
                                    let (bar_rect, _) = ui.allocate_exact_size(
                                        egui::vec2(ui.available_width(), bar_height),
                                        egui::Sense::hover(),
                                    );
                                    let painter = ui.painter();
                                    painter.rect_filled(
                                        bar_rect,
                                        3.0,
                                        ui.visuals().extreme_bg_color,
                                    );
                                    let fill_w =
                                        (bar_rect.width() * fraction.clamp(0.0, 1.0)).max(1.0);
                                    let fill_rect = egui::Rect::from_min_size(
                                        bar_rect.min,
                                        egui::vec2(fill_w, bar_height),
                                    );
                                    let fill_color = if fraction > 0.9 {
                                        egui::Color32::from_rgb(220, 60, 60)
                                    } else if fraction > 0.7 {
                                        egui::Color32::from_rgb(220, 150, 50)
                                    } else {
                                        egui::Color32::from_rgb(52, 152, 219)
                                    };
                                    painter.rect_filled(fill_rect, 3.0, fill_color);

                                    ui.label(format!(
                                        "{:.0}% used \u{2014} {} free",
                                        fraction * 100.0,
                                        bytesize::ByteSize::b(vol.available_bytes)
                                    ));
                                });

                            // Make entire card clickable
                            let card_rect = card_response.response.rect;
                            let card_id = egui::Id::new(("vol_card", &vol.path));
                            let card_interact = ui
                                .interact(card_rect, card_id, egui::Sense::click())
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if card_interact.clicked() {
                                scan_path = Some(vol.path.clone());
                            }

                            ui.add_space(4.0);
                        }

                        if let Some(path) = scan_path {
                            self.start_scan(path);
                        }

                        ui.add_space(12.0);
                    }

                    // Resume last scan
                    if let Some(ref last) = self.last_scan_path.clone() {
                        let label = format!("Resume: {}", last.display());
                        if ui.button(label).clicked() {
                            self.start_scan(last.clone());
                        }
                        ui.add_space(8.0);
                    }

                    // Open Directory — primary action on home page
                    if ui.button("Open Directory...").clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_folder()
                    {
                        self.start_scan(path);
                    }

                    if let Some(ref err) = self.error {
                        ui.add_space(12.0);
                        ui.colored_label(egui::Color32::RED, err);
                    }
                });
                return;
            }

            match self.view_mode {
                ViewMode::Tree => {
                    // Lazy-load icons on first tree render (not at startup)
                    if self.icon_cache.is_none() {
                        self.icon_cache = icons::IconCache::load(ctx);
                    }
                    let render_start = std::time::Instant::now();
                    let rebuild_needed = self.rows_dirty;
                    self.rebuild_rows_if_dirty();
                    let actions = ui::render_tree(
                        ui,
                        &self.cached_rows,
                        &self.focused_path,
                        self.icon_cache.as_ref(),
                        self.tree_scroll_to_focus,
                        &self.selected_paths,
                    );
                    self.tree_scroll_to_focus = false;
                    let render_elapsed = render_start.elapsed();
                    if debug_enabled() && render_elapsed > std::time::Duration::from_millis(16) {
                        eprintln!(
                            "[perf] tree frame: {:?} ({} rows, rebuild={})",
                            render_elapsed,
                            self.cached_rows.len(),
                            rebuild_needed,
                        );
                    }
                    // Handle actions from tree rendering
                    for action in &actions {
                        match action {
                            ui::TreeAction::Click {
                                path,
                                shift,
                                toggle,
                            } => {
                                if *shift {
                                    // Range select: select all visible rows between anchor and clicked row
                                    if let Some(ref anchor) = self.selection_anchor {
                                        let rows = &self.cached_rows;
                                        let anchor_idx =
                                            rows.iter().position(|r| &r.path == anchor);
                                        let click_idx = rows.iter().position(|r| &r.path == path);
                                        if let (Some(a), Some(b)) = (anchor_idx, click_idx) {
                                            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                                            self.selected_paths.clear();
                                            for r in &rows[lo..=hi] {
                                                self.selected_paths.insert(r.path.clone());
                                            }
                                        }
                                    } else {
                                        // No anchor yet — treat as plain click
                                        self.selected_paths.clear();
                                        self.selected_paths.insert(path.clone());
                                        self.selection_anchor = Some(path.clone());
                                    }
                                } else if *toggle {
                                    // Cmd/Ctrl+click: toggle individual item
                                    if !self.selected_paths.remove(path) {
                                        self.selected_paths.insert(path.clone());
                                    }
                                    self.selection_anchor = Some(path.clone());
                                } else {
                                    // Plain click: replace selection and set anchor
                                    self.selected_paths.clear();
                                    self.selected_paths.insert(path.clone());
                                    self.selection_anchor = Some(path.clone());
                                }
                            }
                            ui::TreeAction::Focus(path) => {
                                self.focused_path = Some(path.clone());
                            }
                            ui::TreeAction::Trash(path) => {
                                let targets = self.deletion_targets(path);
                                self.selected_paths.remove(path);
                                self.deleter.start(targets, true);
                            }
                            ui::TreeAction::TrashSelected => {
                                self.batch_trash_selected();
                            }
                            ui::TreeAction::ConfirmDelete(path) => {
                                self.confirm_delete = Some(self.pending_delete_for(path));
                            }
                            ui::TreeAction::ConfirmDeleteSelected => {
                                self.confirm_batch_delete = Some(self.pending_batch_delete());
                            }
                            ui::TreeAction::RevealInFinder(path) => {
                                if let Err(e) = reveal_in_file_manager(path) {
                                    self.error =
                                        Some(format!("Could not reveal in file manager: {e}"));
                                }
                            }
                            ui::TreeAction::CopyPath(path) => {
                                ctx.copy_text(path.display().to_string());
                            }
                            _ => {}
                        }
                    }
                    // Apply expand/collapse changes to tree
                    if let Some(ref mut tree) = self.tree {
                        for action in &actions {
                            match action {
                                ui::TreeAction::ToggleExpand(path) => {
                                    ui::toggle_expand(tree, path);
                                    self.rows_dirty = true;
                                    self.selected_paths.clear();
                                    self.selection_anchor = None;
                                }
                                ui::TreeAction::ToggleFileGroup(path) => {
                                    // path is parent_dir/__file_group__; extract parent
                                    if let Some(parent) = path.parent() {
                                        let p = parent.to_path_buf();
                                        if !self.expanded_file_groups.remove(&p) {
                                            self.expanded_file_groups.insert(p);
                                        }
                                    }
                                    self.rows_dirty = true;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                ViewMode::Treemap => {
                    if let Some(ref tree) = self.tree {
                        let tm_actions = treemap::render_treemap(
                            ui,
                            &mut self.treemap_cache,
                            &mut self.treemap_dirty,
                            tree,
                            &self.treemap_zoom,
                            &self.focused_path,
                            self.treemap_zoom_anim,
                            self.category_filter,
                            self.show_hidden,
                        );

                        for action in tm_actions {
                            match action {
                                TreemapAction::ZoomTo(path) => {
                                    let is_root =
                                        std::path::Path::new(tree.name()) == path.as_path();
                                    let new_zoom = if is_root { None } else { Some(path) };
                                    if new_zoom != self.treemap_zoom {
                                        self.treemap_zoom_anim = Some(ctx.input(|i| i.time));
                                        self.treemap_zoom = new_zoom;
                                        self.treemap_dirty = true;
                                    }
                                }
                                TreemapAction::Focus(path) => {
                                    self.focused_path = Some(path);
                                }
                            }
                        }
                    }
                }
            }
        });

        // Floating batch actions bar (shown when items are selected)
        let selected_count = self.selected_paths.len();
        if selected_count > 0
            && self.tree.is_some()
            && !self.scanning
            && self.view_mode == ViewMode::Tree
        {
            egui::Area::new(egui::Id::new("batch_actions_float"))
                .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -32.0])
                .interactable(true)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style())
                        .inner_margin(egui::Margin::symmetric(16, 8))
                        .corner_radius(8.0)
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 2],
                            blur: 8,
                            spread: 0,
                            color: egui::Color32::from_black_alpha(60),
                        })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{selected_count} item{} selected",
                                        if selected_count == 1 { "" } else { "s" }
                                    ))
                                    .strong(),
                                );
                                ui.add_space(12.0);
                                if ui.button("Move to Trash").clicked() {
                                    self.batch_trash_selected();
                                }
                                if ui
                                    .button(
                                        egui::RichText::new("Delete Permanently")
                                            .color(egui::Color32::from_rgb(220, 60, 60)),
                                    )
                                    .clicked()
                                {
                                    self.confirm_batch_delete = Some(self.pending_batch_delete());
                                }
                                ui.add_space(4.0);
                                if ui
                                    .small_button("×")
                                    .on_hover_text("Clear selection")
                                    .clicked()
                                {
                                    self.selected_paths.clear();
                                }
                            });
                        });
                });
        }

        // Deletion progress overlay
        if self.deleter.is_active() {
            let done = self.deleter.done_count();
            let total = self.deleter.total();
            let fraction = if total > 0 {
                done as f32 / total as f32
            } else {
                0.0
            };
            egui::Area::new(egui::Id::new("delete_progress_float"))
                .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -32.0])
                .interactable(false)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style())
                        .inner_margin(egui::Margin::symmetric(16, 8))
                        .corner_radius(8.0)
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 2],
                            blur: 8,
                            spread: 0,
                            color: egui::Color32::from_black_alpha(60),
                        })
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(
                                    egui::RichText::new(format!("Deleting {done}/{total}..."))
                                        .strong(),
                                );
                                ui.add(egui::ProgressBar::new(fraction).desired_width(200.0));
                            });
                        });
                });
        }

        // Record frame time while scanning (only when debug output is enabled)
        if self.scanning && debug_enabled() {
            self.scan_frame_times.push(frame_start.elapsed());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{dir, leaf};

    /// Rendered rows — the source of truth deletion uses for group identity.
    fn rows_for(tree: &FileNode) -> Vec<ui::CachedRow> {
        ui::collect_cached_rows(tree, "", None, true, None, None, None)
    }

    #[test]
    fn row_is_file_group_true_for_synthetic_group_row() {
        let mut tree = dir("root", vec![leaf("a.txt", 10), leaf("b.txt", 20)]);
        tree.set_expanded(true);
        let rows = rows_for(&tree);

        assert!(row_is_file_group(&rows, Path::new("root/__file_group__")));
    }

    #[test]
    fn row_is_file_group_false_for_real_file_named_marker() {
        // Grouping is suppressed, so the row at root/__file_group__ is the
        // real file — keyboard nav and deletes must not treat it as a group.
        let mut tree = dir(
            "root",
            vec![
                leaf("a.txt", 10),
                leaf("b.txt", 20),
                leaf("__file_group__", 1),
            ],
        );
        tree.set_expanded(true);
        let rows = rows_for(&tree);

        assert!(!row_is_file_group(&rows, Path::new("root/__file_group__")));
    }

    #[test]
    fn row_is_file_group_false_for_path_not_rendered() {
        assert!(!row_is_file_group(&[], Path::new("root/__file_group__")));
    }

    #[test]
    fn resolve_synthetic_group_expands_to_loose_files() {
        // Two loose files → a synthetic group row at root/__file_group__.
        let mut tree = dir("root", vec![leaf("a.txt", 10), leaf("b.txt", 20)]);
        tree.set_expanded(true);
        let rows = rows_for(&tree);
        assert!(
            rows.iter()
                .any(|r| r.is_file_group && r.path.as_path() == Path::new("root/__file_group__"))
        );

        let got =
            resolve_deletion_targets(&rows, Some(&tree), Path::new("root/__file_group__"), true);

        assert_eq!(
            got,
            vec![PathBuf::from("root/a.txt"), PathBuf::from("root/b.txt")]
        );
    }

    #[test]
    fn resolve_real_file_named_group_deletes_only_itself() {
        // Invariant suppresses grouping when a loose file is named
        // __file_group__, so deleting that row must remove only the real file.
        let mut tree = dir(
            "root",
            vec![
                leaf("a.txt", 10),
                leaf("b.txt", 20),
                leaf("__file_group__", 1),
            ],
        );
        tree.set_expanded(true);
        let rows = rows_for(&tree);
        assert!(!rows.iter().any(|r| r.is_file_group));

        let got =
            resolve_deletion_targets(&rows, Some(&tree), Path::new("root/__file_group__"), true);

        assert_eq!(got, vec![PathBuf::from("root/__file_group__")]);
    }

    #[test]
    fn resolve_ordinary_file_maps_to_itself() {
        let mut tree = dir("root", vec![leaf("a.txt", 10), leaf("b.txt", 20)]);
        tree.set_expanded(true);
        let rows = rows_for(&tree);

        assert_eq!(
            resolve_deletion_targets(&rows, Some(&tree), Path::new("root/a.txt"), true),
            vec![PathBuf::from("root/a.txt")]
        );
    }

    #[test]
    fn batch_expands_group_and_dedups_overlapping_child() {
        // Selecting the group row AND one of its loose files must delete
        // each file once.
        let mut tree = dir("root", vec![leaf("a.txt", 10), leaf("b.txt", 20)]);
        tree.set_expanded(true);
        let rows = rows_for(&tree);

        let got = resolve_batch_targets(
            &rows,
            Some(&tree),
            vec![
                PathBuf::from("root/__file_group__"),
                PathBuf::from("root/a.txt"),
            ],
            true,
        );

        assert_eq!(
            got,
            vec![PathBuf::from("root/a.txt"), PathBuf::from("root/b.txt")]
        );
    }

    #[test]
    fn batch_group_expansion_respects_show_hidden() {
        // With show_hidden off, a selected group must not expand to hidden
        // files the user never saw.
        let mut tree = dir(
            "root",
            vec![leaf("a.txt", 10), leaf("b.txt", 20), leaf(".secret", 5)],
        );
        tree.set_expanded(true);
        let rows = ui::collect_cached_rows(&tree, "", None, false, None, None, None);

        let got = resolve_batch_targets(
            &rows,
            Some(&tree),
            vec![PathBuf::from("root/__file_group__")],
            false,
        );

        assert_eq!(
            got,
            vec![PathBuf::from("root/a.txt"), PathBuf::from("root/b.txt")]
        );
    }

    #[test]
    fn batch_stale_group_path_treated_literally() {
        // A group path no longer among the rendered rows maps to itself;
        // the deleter no-ops on it unless a real entry exists there.
        let got =
            resolve_batch_targets(&[], None, vec![PathBuf::from("root/__file_group__")], true);

        assert_eq!(got, vec![PathBuf::from("root/__file_group__")]);
    }

    #[test]
    fn resolve_stale_path_not_in_rows_maps_to_itself() {
        // A path no longer in the rendered rows is treated literally — no
        // filesystem probe, no sibling expansion.
        let tree = dir("root", vec![leaf("a.txt", 10), leaf("b.txt", 20)]);
        let rows: Vec<ui::CachedRow> = Vec::new();

        assert_eq!(
            resolve_deletion_targets(&rows, Some(&tree), Path::new("root/__file_group__"), true),
            vec![PathBuf::from("root/__file_group__")]
        );
    }

    #[test]
    fn resolve_group_without_tree_is_empty() {
        let mut tree = dir("root", vec![leaf("a.txt", 10), leaf("b.txt", 20)]);
        tree.set_expanded(true);
        let rows = rows_for(&tree);
        assert!(
            resolve_deletion_targets(&rows, None, Path::new("root/__file_group__"), true)
                .is_empty()
        );
    }
}
