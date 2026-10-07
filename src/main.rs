#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod background_query;
mod deleter;

use disk_cleaner::{categories, category_worker, icons, scanner, tree, treemap, ui};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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

/// Shorten a path for a button label, keeping the root and leaf visible.
fn middle_truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let head_s: String = chars[..head].iter().collect();
    let tail_s: String = chars[chars.len() - tail..].iter().collect();
    format!("{head_s}\u{2026}{tail_s}")
}

/// Split a trailing drive letter off a volume name so it survives truncation.
fn split_drive_letter(name: &str) -> (&str, &str) {
    if name.ends_with(":)")
        && let Some(open) = name.rfind(" (")
        && name.len() - open == 5
        && name.as_bytes()[open + 2].is_ascii_alphabetic()
    {
        return (&name[..open], &name[open + 1..]);
    }
    (name, "")
}

/// Paint the title bar dark before the window is ever shown.
///
/// `ViewportCommand::SetTheme` only reaches the decorations at the end of the
/// first frame, by which point the window is visible, so DWM animates the
/// caption from light to dark in front of the user. Setting the attribute on
/// the raw handle during setup wins that race.
#[cfg(target_os = "windows")]
fn set_dark_titlebar(cc: &eframe::CreationContext<'_>) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};

    let Ok(handle) = cc.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return;
    };
    let hwnd = win32.hwnd.get() as *mut core::ffi::c_void;
    let enabled: i32 = 1;
    // Attribute 20 arrived in Windows 10 build 18985. Builds between 17763 and
    // 18984 took the same value under the undocumented 19, so try that before
    // giving up; older builds have no dark caption at all.
    for attribute in [DWMWA_USE_IMMERSIVE_DARK_MODE as u32, 19] {
        let hr = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                attribute,
                (&raw const enabled).cast(),
                size_of::<i32>() as u32,
            )
        };
        if hr >= 0 {
            return;
        }
    }
}

/// Group an integer with thousands separators, e.g. `13544` -> `13,544`.
fn group_thousands(n: u64) -> String {
    let s = n.to_string();
    let len = s.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn write_fallback_report(
    scan_path: Option<&Path>,
    duration: Option<Duration>,
    progress: &ScanProgress,
) -> std::io::Result<PathBuf> {
    let details = progress.fallback_details_snapshot();
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
    if let Some(summary) = fallback_summary(progress) {
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

/// `None` when the scan needed no fallbacks.
fn fallback_summary(progress: &ScanProgress) -> Option<String> {
    scanner::format_fallback_summary(
        progress.fallback_count.load(Ordering::Relaxed),
        progress
            .access_denied_fallback_count
            .load(Ordering::Relaxed),
        progress.bulk_scan_fallback_count.load(Ordering::Relaxed),
    )
}

fn open_text_report(path: &Path) -> std::io::Result<()> {
    let program = if cfg!(windows) {
        "notepad"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    Command::new(program).arg(path).spawn().map(drop)
}

/// Reveal a path in the OS file manager, selecting/highlighting it where the
/// platform supports it.
fn reveal_in_file_manager(path: &Path) -> std::io::Result<()> {
    let (program, args): (&str, Vec<std::ffi::OsString>) = if cfg!(windows) {
        // explorer.exe expects `/select,<path>` as a single argument and
        // wants Windows-style separators.
        (
            "explorer",
            vec![format!("/select,{}", path.display()).into()],
        )
    } else if cfg!(target_os = "macos") {
        ("open", vec!["-R".into(), path.into()])
    } else {
        // No portable "select" on Linux file managers, so open the
        // containing folder instead.
        ("xdg-open", vec![path.parent().unwrap_or(path).into()])
    };
    Command::new(program).args(args).spawn().map(drop)
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

/// Relaunch this executable elevated (UAC) with `scan_path` as its argument,
/// so a whole-drive scan can take the raw NTFS MFT path.
#[cfg(target_os = "windows")]
fn relaunch_elevated(scan_path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;

    let exe = std::env::current_exe().map_err(|err| err.to_string())?;
    let mut arg = scan_path.to_string_lossy().into_owned();
    if arg.contains(' ') {
        // A backslash right before the closing quote would escape it.
        if arg.ends_with('\\') {
            arg.push('\\');
        }
        arg = format!("\"{arg}\"");
    }
    let wide =
        |s: &std::ffi::OsStr| -> Vec<u16> { s.encode_wide().chain(std::iter::once(0)).collect() };
    let verb = wide(std::ffi::OsStr::new("runas"));
    let file = wide(exe.as_os_str());
    let params = wide(std::ffi::OsStr::new(&arg));
    const SW_SHOWNORMAL: i32 = 1;
    let rc = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as usize;
    // Documented as "greater than 32 on success"; 5 is SE_ERR_ACCESSDENIED,
    // i.e. the UAC prompt being declined.
    match rc {
        code if code > 32 => Ok(()),
        5 => Err("permission was declined".to_string()),
        code => Err(format!("ShellExecuteW failed with code {code}")),
    }
}

#[cfg(not(target_os = "windows"))]
fn relaunch_elevated(_scan_path: &std::path::Path) -> Result<(), String> {
    Err("not supported on this platform".to_string())
}

fn main() -> eframe::Result {
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

    // `--screenshot` mode uses a narrower window so the right-anchored size
    // bars sit next to the names (no wide empty gap) — tighter docs images.
    // Inert for normal runs (only set when capturing screenshots).
    let win_size = if screenshot_prefix.is_some() {
        [760.0, 620.0]
    } else {
        [980.0, 700.0]
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(win_size)
            .with_min_inner_size([720.0, 480.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/app_icon.png"))
                    .expect("embedded app icon is a valid PNG"),
            )
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
            #[cfg(target_os = "windows")]
            set_dark_titlebar(cc);
            let mut app = App {
                show_window: true,
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

enum TreeEdit {
    Toggle(PathBuf),
    Expand(PathBuf, bool),
    Remove(PathBuf),
}

struct App {
    tree: Option<Arc<FileNode>>,
    category_worker: category_worker::CategoryWorker,
    category_failed: bool,
    pending_tree_edits: Vec<TreeEdit>,
    scanning: bool,
    scan_path: Option<PathBuf>,
    scan_progress: Arc<ScanProgress>,
    receiver: Option<mpsc::Receiver<FileNode>>,
    error: Option<String>,
    confirm_delete: Option<PendingDelete>,
    confirm_batch_delete: Option<PendingBatchDelete>,
    search_query: String,
    /// The search query currently applied to the cached rows (debounced).
    applied_search: String,
    /// When the search text last changed (for debouncing).
    search_changed_at: Option<Instant>,
    focused_path: Option<PathBuf>,
    view_mode: ViewMode,
    treemap_zoom: Option<PathBuf>,
    treemap_zoom_anim: Option<f64>,
    volumes: Vec<scanner::VolumeInfo>,
    volumes_query: background_query::BackgroundQuery<Vec<scanner::VolumeInfo>>,
    volumes_last_refresh: Option<std::time::Instant>,
    /// Last frame's drive-list height, so a short list can keep the actions
    /// under it rather than at the bottom of the window.
    volumes_list_height: f32,
    disk_space_query: background_query::BackgroundQuery<Option<(u64, u64)>>,
    scan_disk_info: Option<(u64, u64)>, // (total, available) for scan path
    scan_is_volume: bool,               // true when scanning a volume root
    category_filter: Option<categories::FileCategory>,
    category_stats: Option<categories::CategoryStats>,
    show_hidden: bool,
    icon_cache: Option<icons::IconCache>,
    last_scan_duration: Option<Duration>,
    show_categories: bool,
    tree_scroll_to_focus: bool,
    /// Cached visible row list for rendering; rebuilt when dirty.
    cached_rows: Vec<ui::CachedRow>,
    rows_dirty: bool,
    /// Memoized match caches, keyed by the filter they were built for.
    /// `NodeMatchSet` keys node addresses, so these are only valid while the
    /// tree is structurally unchanged — invalidated on scan start/completion
    /// and node removal (see `invalidate_match_memos`).
    cat_cache_memo: Option<(categories::FileCategory, ui::NodeMatchSet)>,
    text_cache_memo: Option<(String, ui::NodeMatchSet)>,
    /// Cached treemap layout; rebuilt when treemap_dirty.
    treemap_cache: Option<treemap::TreemapCache>,
    treemap_dirty: bool,
    /// Selection state stored centrally for O(1) clear/select instead of O(n) tree walk.
    selected_paths: HashSet<PathBuf>,
    /// Anchor path for shift+click range selection.
    selection_anchor: Option<PathBuf>,
    /// Tracks which file groups in the tree view are expanded.
    expanded_file_groups: HashSet<PathBuf>,
    /// The window starts hidden; show it once the first frame has rendered.
    show_window: bool,
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
        let (scan_path, show_hidden) = load_config();
        Self {
            tree: None,
            category_worker: Default::default(),
            category_failed: false,
            pending_tree_edits: Vec::new(),
            scanning: false,
            scan_path,
            scan_progress: Arc::new(ScanProgress {
                file_count: 0.into(),
                total_size: 0.into(),
                fallback_count: 0.into(),
                access_denied_fallback_count: 0.into(),
                bulk_scan_fallback_count: 0.into(),
                fallback_details: std::sync::Mutex::new(Vec::new()),
                cancelled: false.into(),
                seen_inodes: Default::default(),
                mft_used: false.into(),
                mft_elevation_hint: false.into(),
            }),
            receiver: None,
            error: None,
            confirm_delete: None,
            confirm_batch_delete: None,
            search_query: String::new(),
            applied_search: String::new(),
            search_changed_at: None,
            focused_path: None,
            view_mode: ViewMode::Tree,
            treemap_zoom: None,
            treemap_zoom_anim: None,
            volumes: Vec::new(),
            volumes_query: {
                let mut query = background_query::BackgroundQuery::default();
                query.request(scanner::list_volumes);
                query
            },
            volumes_last_refresh: None,
            volumes_list_height: 0.0,
            disk_space_query: Default::default(),
            scan_disk_info: None,
            scan_is_volume: false,
            category_filter: None,
            category_stats: None,
            show_hidden,
            icon_cache: None,
            last_scan_duration: None,
            show_categories: false,
            tree_scroll_to_focus: false,
            cached_rows: Vec::new(),
            rows_dirty: true,
            cat_cache_memo: None,
            text_cache_memo: None,
            treemap_cache: None,
            treemap_dirty: true,
            selected_paths: HashSet::new(),
            selection_anchor: None,
            expanded_file_groups: HashSet::new(),
            show_window: false,
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
        self.disk_space_query.cancel();
        self.scan_progress.cancelled.store(true, Ordering::Relaxed);
        self.scanning = false;
        self.receiver = None;
        self.scan_start_time = None;
    }

    fn open_fallback_report(&mut self) {
        if let Err(err) = write_fallback_report(
            self.scan_path.as_deref(),
            self.last_scan_duration,
            &self.scan_progress,
        )
        .and_then(|path| open_text_report(&path))
        {
            self.error = Some(format!("Could not open compatibility report: {err}"));
        }
    }

    fn start_scan(&mut self, path: PathBuf) {
        // Cancel any in-progress scan so its threads release the rayon pool
        self.scan_progress.cancelled.store(true, Ordering::Relaxed);

        save_config(&path, self.show_hidden);
        self.scanning = true;
        self.error = None;
        self.category_worker.cancel();
        self.category_stats = None;
        self.category_failed = false;
        self.pending_tree_edits.clear();
        self.tree = None;
        self.invalidate_match_memos();
        self.selected_paths.clear();
        self.selection_anchor = None;
        self.scan_path = Some(path.clone());
        self.scan_disk_info = None;
        self.refresh_disk_info();
        self.scan_is_volume = self.volumes.iter().any(|v| v.path == path);

        let progress = Arc::new(ScanProgress {
            file_count: 0.into(),
            total_size: 0.into(),
            fallback_count: 0.into(),
            access_denied_fallback_count: 0.into(),
            bulk_scan_fallback_count: 0.into(),
            fallback_details: std::sync::Mutex::new(Vec::new()),
            cancelled: false.into(),
            seen_inodes: Default::default(),
            mft_used: false.into(),
            mft_elevation_hint: false.into(),
        });
        self.scan_progress = progress.clone();

        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);

        self.scan_start_time = Some(Instant::now());
        self.last_scan_duration = None;

        thread::spawn(move || {
            let mut tree = scanner::scan_directory(&path, progress);
            tree::auto_expand(&mut tree, 0, 2);
            let _ = tx.send(tree);
        });
    }

    fn edit_tree(&mut self, edit: TreeEdit) {
        if self.tree.is_none() {
            return;
        }
        self.pending_tree_edits.push(edit);
        // Never clone the scan or wait for a reader on the UI thread.
        self.category_worker.cancel();
        self.apply_tree_edits();
    }

    fn apply_tree_edits(&mut self) {
        if self.pending_tree_edits.is_empty() {
            return;
        }
        let Some(tree) = self.tree.as_mut().and_then(Arc::get_mut) else {
            return;
        };
        let mut removed = false;
        for edit in self.pending_tree_edits.drain(..) {
            match edit {
                TreeEdit::Toggle(path) => {
                    if let Some(node) = tree.find_mut(&path) {
                        node.set_expanded(!node.expanded());
                    }
                }
                TreeEdit::Expand(path, expanded) => {
                    if let Some(node) = tree.find_mut(&path) {
                        node.set_expanded(expanded);
                    }
                }
                TreeEdit::Remove(path) => {
                    removed |= ui::remove_node(tree, &path).is_some();
                }
            }
        }
        if removed {
            self.category_stats = None;
            self.category_failed = false;
            self.invalidate_match_memos();
            self.mark_dirty();
        } else {
            self.rows_dirty = true;
        }
    }

    fn poll_categories(&mut self) {
        match self.category_worker.poll() {
            category_worker::Poll::Complete(Some(stats)) => self.category_stats = Some(stats),
            category_worker::Poll::Failed => self.category_failed = true,
            _ => {}
        }
        self.apply_tree_edits();
    }

    fn start_categories(&mut self) {
        if self.category_stats.is_none()
            && !self.category_failed
            && !self.category_worker.is_active()
            && self.pending_tree_edits.is_empty()
            && let Some(tree) = &self.tree
        {
            self.category_worker.start(tree.clone());
        }
    }

    fn rebuild_rows_if_dirty(&mut self) {
        if !self.rows_dirty {
            return;
        }

        // Reuse memoized match caches when the filter is unchanged — building
        // one is a full-tree walk, and refreshes fire on every expand/collapse.
        // Safe because the memos are invalidated whenever the tree changes.
        if !self.applied_search.is_empty() {
            if let Some(tree) = self.tree.as_ref()
                && self
                    .text_cache_memo
                    .as_ref()
                    .is_none_or(|(q, _)| q != &self.applied_search)
            {
                self.text_cache_memo = Some((
                    self.applied_search.clone(),
                    ui::build_text_match_cache(tree, &self.applied_search),
                ));
            }
        } else {
            self.text_cache_memo = None;
        }

        if let Some(cat) = self.category_filter {
            if let Some(tree) = self.tree.as_ref()
                && self.cat_cache_memo.as_ref().is_none_or(|(c, _)| *c != cat)
            {
                self.cat_cache_memo = Some((cat, ui::build_category_match_cache(tree, cat)));
            }
        } else {
            self.cat_cache_memo = None;
        }

        let text_cache = self.text_cache_memo.as_ref().map(|(_, s)| s);
        let cat_cache = self.cat_cache_memo.as_ref().map(|(_, s)| s);

        // Drop old rows before building new ones to avoid holding two full
        // Vec<CachedRow> in memory simultaneously (OOM risk on large trees).
        self.cached_rows = Vec::new();

        if let Some(ref tree) = self.tree {
            self.cached_rows = ui::collect_cached_rows(
                tree,
                &self.applied_search,
                self.category_filter,
                self.show_hidden,
                text_cache,
                cat_cache,
                Some(&self.expanded_file_groups),
            );
        }
        self.rows_dirty = false;
    }

    /// Drop memoized match caches. Must be called whenever the tree is
    /// replaced or structurally mutated: the sets key node addresses, so a
    /// stale set would give wrong (or address-reused) filter results.
    fn invalidate_match_memos(&mut self) {
        self.cat_cache_memo = None;
        self.text_cache_memo = None;
    }

    /// Move keyboard focus to `path`, clearing the selection so only the
    /// focused row is highlighted.
    fn focus_row(&mut self, path: PathBuf) {
        self.focused_path = Some(path);
        self.selected_paths.clear();
        self.tree_scroll_to_focus = true;
    }

    fn focus_next_row(&mut self, path: &Path) {
        if let Some(idx) = self.cached_rows.iter().position(|r| r.path == path)
            && let Some(next) = self.cached_rows.get(idx + 1)
        {
            self.focus_row(next.path.clone());
        }
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

    /// Expand a batch of selected row paths into a de-duplicated target list.
    fn batch_targets(&self, paths: Vec<PathBuf>) -> Vec<PathBuf> {
        resolve_batch_targets(
            &self.cached_rows,
            self.tree.as_deref(),
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
        let targets = self.batch_targets(vec![path.to_path_buf()]);
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
        if let Some(results) = self.deleter.poll() {
            let mut deleted_any = false;
            for (path, err) in results {
                if let Some(msg) = err {
                    self.error = Some(format!("Delete failed: {msg}"));
                } else {
                    self.edit_tree(TreeEdit::Remove(path));
                    deleted_any = true;
                }
            }
            if deleted_any {
                self.refresh_disk_info();
            }
        }
    }

    /// Re-query disk space so the status bar reflects freed space after deletions.
    fn refresh_disk_info(&mut self) {
        if let Some(path) = self.scan_path.clone() {
            self.disk_space_query
                .request(move || scanner::disk_space(&path));
        }
    }

    fn poll_disk_queries(&mut self, ctx: &egui::Context) {
        if let Some(volumes) = self.volumes_query.poll(ctx) {
            self.volumes = volumes;
            self.volumes_last_refresh = Some(Instant::now());
            self.scan_is_volume = self
                .scan_path
                .as_ref()
                .is_some_and(|path| self.volumes.iter().any(|volume| volume.path == *path));
            ctx.request_repaint();
        }
        if let Some(info) = self.disk_space_query.poll(ctx) {
            self.scan_disk_info = info;
            ctx.request_repaint();
        }
    }
}

/// A centered yes/cancel dialog: `Some(true)` on yes (or Enter), `Some(false)`
/// on cancel, `None` while still open.
fn confirm_window(ctx: &egui::Context, title: &str, text: &str, yes_label: &str) -> Option<bool> {
    let enter_pressed = ctx.input(|i| i.key_pressed(egui::Key::Enter));
    let mut answer = None;
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(text);
            ui.horizontal(|ui| {
                let yes =
                    egui::Button::new(egui::RichText::new(yes_label).color(egui::Color32::WHITE))
                        .fill(egui::Color32::from_rgb(220, 50, 50));
                if ui.add(yes).clicked() || enter_pressed {
                    answer = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    answer = answer.or(Some(false));
                }
            });
        });
    answer
}

/// A floating bar anchored above the bottom edge of the window.
fn floating_bar(
    ctx: &egui::Context,
    id: &str,
    interactable: bool,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    egui::Area::new(egui::Id::new(id))
        .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -32.0])
        .interactable(interactable)
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
                    ui.horizontal(add_contents);
                });
        });
}

/// True if `path` is a synthetic file-group row among the rendered rows.
///
/// The single source of truth for group identity: never the path string or
/// the filesystem, so a real entry named `__file_group__` is never mistaken
/// for a group.
fn row_is_file_group(rows: &[ui::CachedRow], path: &Path) -> bool {
    rows.iter().any(|r| r.is_file_group && r.path == path)
}

/// Expand selected row paths into the de-duplicated real paths a delete should
/// touch.
///
/// Identity comes from the rendered rows, never the filesystem: a path is a
/// group only if a rendered row carries it with `is_file_group`. A group
/// expands to the loose files in its parent dir; anything else maps to itself.
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

fn save_screenshot_png(
    color_image: &egui::ColorImage,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use eframe::icon_data::IconDataExt as _;
    let png = egui::IconData {
        rgba: color_image.as_raw().to_vec(),
        width: color_image.width() as u32,
        height: color_image.height() as u32,
    }
    .to_png_bytes()?;
    std::fs::write(path, png)?;
    eprintln!("[screenshot] saved: {path}");
    Ok(())
}

/// Screenshot helper: collect every directory path in the tree so their file
/// groups can be force-expanded for capture.
fn collect_group_dirs(node: &FileNode, buf: &mut PathBuf, out: &mut HashSet<PathBuf>) {
    if node.is_dir() {
        out.insert(buf.clone());
        for c in node.children() {
            buf.push(c.name());
            collect_group_dirs(c, buf, out);
            buf.pop();
        }
    }
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
        // Show window on first frame (was created hidden to avoid white flash)
        if std::mem::take(&mut self.show_window) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        }

        self.keep_titlebar_dark(ctx);
        self.poll_categories();

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
            && let Ok(tree) = rx.try_recv()
        {
            self.category_stats = None;
            self.category_failed = false;
            self.tree = Some(Arc::new(tree));
            self.invalidate_match_memos();
            self.scanning = false;
            self.receiver = None;
            self.category_filter = None;
            self.mark_dirty();
            self.last_scan_duration = self.scan_start_time.take().map(|start| start.elapsed());
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
                    // Home screenshots should include the asynchronously loaded
                    // drive cards, rather than racing the first discovery query.
                    if !self.scanning && (self.tree.is_some() || !self.volumes_query.is_active()) {
                        // Expand all file groups so individual files (e.g. hard
                        // links) are visible instead of a collapsed "[N files]".
                        if let Some(tree) = &self.tree {
                            let mut buf = PathBuf::from(tree.name());
                            collect_group_dirs(tree, &mut buf, &mut self.expanded_file_groups);
                        }
                        self.screenshot_state = ScreenshotState::WaitFrames(5);
                    }
                }
                ScreenshotState::WaitFrames(_)
                    if self.show_categories
                        && self.category_stats.is_none()
                        && !self.category_failed =>
                {
                    ctx.request_repaint_after(Duration::from_millis(16));
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

            if (up || down) && !self.cached_rows.is_empty() {
                let rows = &self.cached_rows;
                let target = match &self.focused_path {
                    None => Some(0),
                    Some(focused) => rows.iter().position(|r| &r.path == focused).map(|idx| {
                        if up {
                            idx.saturating_sub(1)
                        } else {
                            (idx + 1).min(rows.len() - 1)
                        }
                    }),
                };
                match target {
                    Some(idx) => self.focus_row(rows[idx].path.clone()),
                    None => {
                        self.selected_paths.clear();
                        self.tree_scroll_to_focus = true;
                    }
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
                                self.focus_row(key);
                            }
                        } else if !group_expanded {
                            self.expanded_file_groups.insert(key);
                            self.mark_dirty();
                        } else {
                            // Already expanded — move focus to first child row
                            self.focus_next_row(focused);
                        }
                    }
                } else if let Some(ref tree) = self.tree
                    && let Some(node) = tree.find(focused)
                {
                    let (is_dir, expanded) = (node.is_dir(), node.expanded());
                    let has_children = !node.children().is_empty();
                    let parent = (focused != Path::new(tree.name()))
                        .then(|| focused.parent())
                        .flatten()
                        .map(Path::to_path_buf);
                    if left {
                        if is_dir && expanded {
                            self.edit_tree(TreeEdit::Expand(focused.clone(), false));
                        } else if let Some(parent) = parent {
                            self.focus_row(parent);
                        }
                    } else if is_dir && !expanded && has_children {
                        self.edit_tree(TreeEdit::Expand(focused.clone(), true));
                    } else if is_dir && expanded {
                        self.focus_next_row(focused);
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
                    self.edit_tree(TreeEdit::Toggle(focused.clone()));
                } else if shift_del {
                    self.confirm_delete = Some(self.pending_delete_for(focused));
                } else if del {
                    let targets = self.batch_targets(vec![focused.clone()]);
                    self.selected_paths.remove(focused);
                    self.deleter.start(targets, true);
                    self.focused_path = None;
                }
            }
        }

        if let Some(pending) = &self.confirm_batch_delete {
            let text = format!(
                "Permanently delete {} selected item(s)? This cannot be undone.",
                pending.item_count
            );
            if let Some(yes) = confirm_window(ctx, "Confirm Batch Delete", &text, "Yes, delete all")
                && let Some(pending) = self.confirm_batch_delete.take()
                && yes
            {
                // Use the plan captured when the user asked, not a fresh lookup.
                self.selected_paths.clear();
                self.deleter.start(pending.targets, false);
            }
        }

        if let Some(pending) = &self.confirm_delete
            && let Some(yes) = confirm_window(ctx, "Confirm Delete", &pending.prompt, "Yes, delete")
            && let Some(pending) = self.confirm_delete.take()
            && yes
        {
            // Use the plan captured when the user asked, not a fresh lookup.
            self.selected_paths.remove(&pending.path);
            self.deleter.start(pending.targets, false);
        }

        // Toolbar only in the results view — hidden on home and while scanning,
        // where it would leave a lone orphaned button.
        let show_toolbar = self.tree.is_some() && !self.scanning;
        if show_toolbar {
            egui::TopBottomPanel::top("toolbar")
                .show_separator_line(false)
                .default_size(28.0) // 24px interact_size + 4px inner_margin
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        // Standardize widget height so buttons and selectable labels align
                        ui.spacing_mut().interact_size.y = 24.0;

                        if ui.button("Scan a Folder...").clicked()
                            && let Some(path) = rfd::FileDialog::new().pick_folder()
                        {
                            self.start_scan(path);
                        }

                        if self.tree.is_some()
                            && ui.button("Rescan").clicked()
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
                        ui.separator();
                        if ui
                            .selectable_label(self.show_hidden, "Show hidden")
                            .clicked()
                        {
                            self.show_hidden = !self.show_hidden;
                            self.mark_dirty();
                            // Persist preference
                            if let Some(ref path) = self.scan_path {
                                save_config(path, self.show_hidden);
                            }
                        }

                        // File types panel toggle
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
                    if self.category_stats.is_none() {
                        if self.category_failed {
                            ui.label("Could not calculate file types.");
                        } else {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Calculating…");
                            });
                        }
                    }

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
        let mut restart_elevated = false;

        // Bottom status bar with scan info + selection + keyboard hints
        egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                // Left: static scan summary.
                let file_count = self.scan_progress.file_count.load(Ordering::Relaxed);
                let total_size =
                    bytesize::ByteSize::b(self.scan_progress.total_size.load(Ordering::Relaxed));
                if self.tree.is_some() && !self.scanning {
                    if self.scan_path.is_some() {
                        let summary = format!("{file_count} files, {total_size}");
                        ui.label(egui::RichText::new(summary).small());
                    }
                } else if let Some(ref path) = self.scan_path
                    && !self.scanning
                    && file_count > 0
                {
                    ui.label(
                        egui::RichText::new(format!(
                            "Scanned: {} ({file_count} files, {total_size})",
                            path.display(),
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

                        if self.scan_progress.mft_used.load(Ordering::Relaxed) {
                            ui.label(egui::RichText::new("⚡ MFT fast scan").small().weak())
                                .on_hover_text(
                                    "Scanned via the raw NTFS MFT: the volume's file table is \
                                     read sequentially instead of walking every directory.",
                                );
                            ui.separator();
                        } else if self
                            .scan_progress
                            .mft_elevation_hint
                            .load(Ordering::Relaxed)
                        {
                            let button = egui::Button::new(
                                egui::RichText::new("💡 Restart as admin").small().weak(),
                            )
                            .frame(false);
                            let response = ui.add(button).on_hover_text(
                                "Relaunch as administrator to enable the NTFS fast scan for \
                                 whole-drive scans — typically 2-3x faster when the disk \
                                 cache is cold. Windows will ask for permission.",
                            );
                            if response.clicked() {
                                restart_elevated = true;
                            }
                            ui.separator();
                        }

                        if let Some(hover) = fallback_summary(&self.scan_progress) {
                            let count = self.scan_progress.fallback_count.load(Ordering::Relaxed);
                            let button = egui::Button::new(
                                egui::RichText::new(format!("⚠ {count}"))
                                    .small()
                                    .color(egui::Color32::from_rgb(230, 200, 80)),
                            )
                            .frame(false);
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
        if restart_elevated && let Some(path) = self.scan_path.clone() {
            match relaunch_elevated(&path) {
                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                Err(err) => {
                    self.error = Some(format!("Could not restart as administrator: {err}"));
                }
            }
        }

        // Main content
        egui::CentralPanel::default().show(ctx, |ui| {
            // Full-page scanning UI
            if self.scanning {
                let files = self.scan_progress.file_count.load(Ordering::Relaxed);
                let size = self.scan_progress.total_size.load(Ordering::Relaxed);
                let size_str = bytesize::ByteSize::b(size).to_string();
                let elapsed_str = self
                    .scan_start_time
                    .map(|s| format_elapsed(s.elapsed()))
                    .unwrap_or_else(|| "0s".to_string());
                let path_str = self
                    .scan_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                // Prefer the volume label for a volume root, else the folder name.
                let target = self
                    .volumes
                    .iter()
                    .find(|v| Some(&v.path) == self.scan_path.as_ref())
                    .map(|v| v.name.clone())
                    .or_else(|| {
                        self.scan_path
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .map(|n| n.to_string_lossy().into_owned())
                    })
                    .unwrap_or_else(|| path_str.clone());

                let avail = ui.available_height();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.set_min_height(avail);
                    ui.vertical_centered(|ui| {
                        ui.add_space(((avail - 240.0) * 0.5).max(24.0));

                        // Spinner above a strong, centered title.
                        ui.spinner();
                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new(format!("Scanning {target}"))
                                .size(22.0)
                                .strong(),
                        );
                        // Show the path only for folders — a volume's name already
                        // identifies it, so "/" would just be noise.
                        if !self.scan_is_volume {
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new(&path_str).weak().size(12.0));
                        }
                        ui.add_space(20.0);

                        // Live stat readout — unboxed columns; the counters are the
                        // point, so give them weight (accent the growing size).
                        egui::Frame::default().show(ui, |ui| {
                            ui.set_width(360.0);
                            ui.columns(3, |cols| {
                                let stat = |ui: &mut egui::Ui, k: &str, v: &str, accent: bool| {
                                    ui.vertical_centered(|ui| {
                                        ui.label(egui::RichText::new(k).size(10.0).weak());
                                        ui.add_space(2.0);
                                        let mut t = egui::RichText::new(v).size(21.0).strong();
                                        if accent {
                                            t = t.color(egui::Color32::from_rgb(90, 176, 255));
                                        }
                                        ui.label(t);
                                    });
                                };
                                stat(&mut cols[0], "Files", &group_thousands(files), false);
                                stat(&mut cols[1], "Size", &size_str, true);
                                stat(&mut cols[2], "Elapsed", &elapsed_str, false);
                            });
                        });

                        // Progress bar (volume scans estimate against used space).
                        // Painted flat like the volume capacity bars — the default
                        // ProgressBar's rounded cap reads as a slider thumb.
                        if self.scan_is_volume
                            && let Some((total, available)) = self.scan_disk_info
                        {
                            let used = total.saturating_sub(available);
                            if used > 0 {
                                let fraction = (size as f32 / used as f32).clamp(0.0, 1.0);
                                let pct = fraction * 100.0;
                                // "<1%" instead of a flat "0%" while size-based
                                // progress rounds down but files are streaming in.
                                let pct_str = if fraction > 0.0 && pct < 1.0 {
                                    "<1%".to_string()
                                } else {
                                    format!("{pct:.0}%")
                                };
                                ui.add_space(14.0);
                                ui.label(egui::RichText::new(pct_str).weak().size(12.0));
                                ui.add_space(4.0);
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(360.0, 8.0),
                                    egui::Sense::hover(),
                                );
                                let painter = ui.painter();
                                painter.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
                                let fill_w = rect.width() * fraction;
                                if fill_w > 0.5 {
                                    let fill = egui::Rect::from_min_size(
                                        rect.min,
                                        egui::vec2(fill_w.max(3.0), 8.0),
                                    );
                                    painter.rect_filled(
                                        fill,
                                        3.0,
                                        egui::Color32::from_rgb(37, 99, 235),
                                    );
                                }
                            }
                        }

                        ui.add_space(24.0);
                        let cancel_btn =
                            egui::Button::new(egui::RichText::new("Cancel").size(14.0))
                                .min_size(egui::vec2(120.0, 34.0));
                        if ui.add(cancel_btn).clicked() {
                            self.cancel_scan();
                        }
                    });
                });
                return;
            }

            if self.tree.is_none() {
                // Refresh volume list every 5 seconds
                let should_refresh = !self.volumes_query.is_active()
                    && self
                        .volumes_last_refresh
                        .is_none_or(|t| t.elapsed().as_secs() >= 5);
                if should_refresh {
                    self.volumes_query.request(scanner::list_volumes);
                }
                if !self.volumes_query.is_active() {
                    ctx.request_repaint_after(Duration::from_secs(5));
                }
                let foreground = egui::Color32::from_rgb(238, 240, 244);
                let secondary = egui::Color32::from_rgb(177, 185, 198);
                let short = ui.available_height() < 500.0;
                let width = 540.0_f32.min(ui.available_width() - 32.0);
                let rescan_path = self
                    .scan_path
                    .as_ref()
                    .filter(|last| !self.volumes.iter().any(|volume| volume.path == **last))
                    .cloned();
                let mut scan_path = None;
                let mut measured_list_height = 0.0_f32;
                let mut pick_folder = false;
                let discovering = self.volumes.is_empty() && self.volumes_query.is_active();

                ui.vertical_centered(|ui| {
                    ui.visuals_mut().override_text_color = Some(foreground);
                    ui.style_mut().spacing.scroll = egui::style::ScrollStyle::solid();
                    ui.add_space(if short { 12.0 } else { 24.0 });
                    ui.label(
                        egui::RichText::new("Choose a drive to scan")
                            .size(if short { 22.0 } else { 26.0 })
                            .strong(),
                    );
                    ui.add_space(if short { 12.0 } else { 26.0 });
                    ui.allocate_ui_with_layout(
                        egui::vec2(width, 22.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.label(egui::RichText::new("Drives").size(15.0).strong());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(if discovering {
                                            "Finding drives…".to_owned()
                                        } else {
                                            format!(
                                                "{} {} · Select to scan",
                                                self.volumes.len(),
                                                if self.volumes.len() == 1 { "drive" } else { "drives" }
                                            )
                                        })
                                        .size(12.0)
                                        .color(secondary),
                                    );
                                },
                            );
                        },
                    );
                    ui.add_space(8.0);

                    // Reserve room for actions and errors outside the scrolling list.
                    let footer_height = if self.error.is_some() { 95.0 } else { 64.0 };
                    let available = (ui.available_height() - footer_height).max(0.0);
                    // Size the list to the drives it drew last frame, so a short
                    // list keeps the actions under it rather than at the bottom.
                    let list_height = if self.volumes_list_height > 0.0 {
                        self.volumes_list_height.min(available)
                    } else {
                        available
                    };
                    let list_content = ui.allocate_ui_with_layout(
                        egui::vec2(width + 12.0, list_height),
                        egui::Layout::top_down(egui::Align::Center),
                        |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("home_drives")
                                .max_height(list_height)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    if self.volumes.is_empty() {
                                        ui.add_space(24.0);
                                        ui.label(
                                            egui::RichText::new(if discovering {
                                                "Finding drives. You can also choose a folder to scan."
                                            } else {
                                                "No drives available. Choose a folder to scan."
                                            })
                                            .size(14.0)
                                            .color(secondary),
                                        );
                                    }
                                    for vol in &self.volumes {
                                        let used =
                                            vol.total_bytes.saturating_sub(vol.available_bytes);
                                        let fraction = if vol.total_bytes > 0 {
                                            used as f32 / vol.total_bytes as f32
                                        } else {
                                            0.0
                                        };
                                        let mut truncated = false;
                                        let card = egui::Frame::new()
                                            .fill(egui::Color32::from_rgb(34, 37, 43))
                                            .stroke(egui::Stroke::new(
                                                1.0_f32,
                                                egui::Color32::from_rgb(60, 65, 76),
                                            ))
                                            .corner_radius(7.0)
                                            .inner_margin(10.0)
                                            .show(ui, |ui| {
                                                ui.set_width(width - 30.0);
                                                ui.horizontal(|ui| {
                                                    let bar_width =
                                                        if short { 80.0 } else { 160.0 };
                                                    let bar_height = if short { 6.0 } else { 7.0 };
                                                    // Room for "<free> free of <total>" at 13pt.
                                                    let label_width =
                                                        if short { 185.0 } else { 198.0 };
                                                    let (label, drive) =
                                                        split_drive_letter(&vol.name);
                                                    let drive_width =
                                                        if drive.is_empty() { 0.0 } else { 38.0 };
                                                    let name_width = (ui.available_width()
                                                        - bar_width
                                                        - label_width
                                                        - drive_width)
                                                        .max(0.0);
                                                    ui.allocate_ui_with_layout(
                                                        egui::vec2(name_width + drive_width, 20.0),
                                                        egui::Layout::left_to_right(
                                                            egui::Align::Center,
                                                        ),
                                                        |ui| {
                                                            ui.set_min_width(
                                                                name_width + drive_width,
                                                            );
                                                            ui.spacing_mut().item_spacing.x = 5.0;
                                                            truncated = ui
                                                                .painter()
                                                                .layout_no_wrap(
                                                                    label.to_owned(),
                                                                    egui::FontId::proportional(
                                                                        15.0,
                                                                    ),
                                                                    foreground,
                                                                )
                                                                .size()
                                                                .x
                                                                > name_width;
                                                            // Cap the label so a long one cannot
                                                            // eat the drive letter's room, while
                                                            // a short one still sits against it.
                                                            ui.scope(|ui| {
                                                                ui.set_max_width(name_width);
                                                                ui.add(
                                                                    egui::Label::new(
                                                                        egui::RichText::new(label)
                                                                            .size(15.0)
                                                                            .strong(),
                                                                    )
                                                                    .truncate(),
                                                                );
                                                            });
                                                            if !drive.is_empty() {
                                                                ui.label(
                                                                    egui::RichText::new(drive)
                                                                        .size(15.0)
                                                                        .strong(),
                                                                );
                                                            }
                                                        },
                                                    );
                                                    let (bar, _) = ui.allocate_exact_size(
                                                        egui::vec2(bar_width, bar_height),
                                                        egui::Sense::hover(),
                                                    );
                                                    ui.painter().rect_filled(
                                                        bar,
                                                        3.0,
                                                        egui::Color32::from_rgb(40, 45, 54),
                                                    );
                                                    let color = if fraction > 0.9 {
                                                        egui::Color32::from_rgb(239, 106, 108)
                                                    } else if fraction > 0.7 {
                                                        egui::Color32::from_rgb(230, 176, 78)
                                                    } else {
                                                        egui::Color32::from_rgb(83, 164, 233)
                                                    };
                                                    ui.painter().rect_filled(
                                                        egui::Rect::from_min_size(
                                                            bar.min,
                                                            egui::vec2(
                                                                (bar.width()
                                                                    * fraction.clamp(0.0, 1.0))
                                                                .max(1.0),
                                                                bar.height(),
                                                            ),
                                                        ),
                                                        3.0,
                                                        color,
                                                    );
                                                    ui.with_layout(
                                                        egui::Layout::right_to_left(
                                                            egui::Align::Center,
                                                        ),
                                                        |ui| {
                                                            ui.spacing_mut().item_spacing.x = 5.0;
                                                            ui.label(
                                                                egui::RichText::new(format!(
                                                                    "of {}",
                                                                    bytesize::ByteSize::b(
                                                                        vol.total_bytes
                                                                    )
                                                                ))
                                                                .size(13.0)
                                                                .color(egui::Color32::from_rgb(
                                                                    134, 143, 157,
                                                                )),
                                                            );
                                                            ui.label(
                                                                egui::RichText::new(format!(
                                                                    "{} free",
                                                                    bytesize::ByteSize::b(
                                                                        vol.available_bytes
                                                                    )
                                                                ))
                                                                .size(13.0)
                                                                .color(foreground),
                                                            );
                                                        },
                                                    );
                                                });
                                            });
                                        let response = ui
                                            .interact(
                                                card.response.rect,
                                                egui::Id::new(("vol_card", &vol.path)),
                                                egui::Sense::click(),
                                            )
                                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                                        // The row already says the rest; only the
                                        // clipped-off name is worth a tooltip.
                                        let response = if truncated {
                                            response.on_hover_text(&vol.name)
                                        } else {
                                            response
                                        };
                                        if response.hovered() || response.has_focus() {
                                            ui.painter().rect_stroke(
                                                card.response.rect,
                                                7.0,
                                                egui::Stroke::new(
                                                    1.5_f32,
                                                    egui::Color32::from_rgb(83, 164, 233),
                                                ),
                                                egui::StrokeKind::Inside,
                                            );
                                        }
                                        if response.clicked() {
                                            scan_path = Some(vol.path.clone());
                                        }
                                        ui.add_space(8.0);
                                    }
                                })
                                .content_size
                                .y
                        },
                    );
                    measured_list_height = list_content.inner;
                    ui.add_space(14.0);
                    let button_width = (width - ui.spacing().item_spacing.x) / 2.0;
                    let actions_width = if rescan_path.is_some() {
                        width
                    } else {
                        button_width
                    };
                    ui.allocate_ui_with_layout(
                        egui::vec2(actions_width, 44.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            pick_folder = ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("Scan a Folder…")
                                            .size(14.0)
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(egui::Color32::from_rgb(37, 99, 235))
                                    .corner_radius(6.0)
                                    .min_size(egui::vec2(button_width, 40.0)),
                                )
                                .clicked();
                            if let Some(last) = &rescan_path {
                                let full = last.display().to_string();
                                let name = last
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| full.clone());
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new(format!(
                                                "Rescan {}",
                                                middle_truncate(&name, 20)
                                            ))
                                            .size(14.0)
                                            .color(foreground),
                                        )
                                        .truncate()
                                        .fill(egui::Color32::from_rgb(44, 48, 56))
                                        .corner_radius(6.0)
                                        .min_size(egui::vec2(button_width, 40.0)),
                                    )
                                    .on_hover_text(format!("{full}\nStarts a fresh scan"))
                                    .clicked()
                                {
                                    scan_path = Some(last.clone());
                                }
                            }
                        },
                    );
                    if let Some(err) = &self.error {
                        ui.add_space(8.0);
                        ui.add_sized(
                            egui::vec2(width, 20.0),
                            egui::Label::new(
                                egui::RichText::new(err)
                                    .color(egui::Color32::from_rgb(239, 106, 108)),
                            )
                            .truncate(),
                        )
                        .on_hover_text(err);
                    }
                });
                // The new height only takes effect next frame, so ask for one.
                if (self.volumes_list_height - measured_list_height).abs() > 0.5 {
                    ctx.request_repaint();
                }
                self.volumes_list_height = measured_list_height;
                if pick_folder {
                    scan_path = rfd::FileDialog::new().pick_folder();
                }
                if let Some(path) = scan_path {
                    self.start_scan(path);
                }
                return;
            }

            match self.view_mode {
                ViewMode::Tree => {
                    // Lazy-load icons on first tree render (not at startup)
                    if self.icon_cache.is_none() {
                        self.icon_cache = icons::IconCache::load(ctx);
                    }
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
                                let targets = self.batch_targets(vec![path.clone()]);
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
                    for action in &actions {
                        match action {
                            ui::TreeAction::ToggleExpand(path) => {
                                self.edit_tree(TreeEdit::Toggle(path.clone()));
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
            floating_bar(ctx, "batch_actions_float", true, |ui| {
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
            floating_bar(ctx, "delete_progress_float", false, |ui| {
                ui.spinner();
                ui.label(egui::RichText::new(format!("Deleting {done}/{total}...")).strong());
                ui.add(egui::ProgressBar::new(fraction).desired_width(200.0));
            });
        }

        // Start requests queued by this frame's actions and apply completed
        // results. A slow device never blocks rendering or scan cancellation.
        self.poll_disk_queries(ctx);
        self.start_categories();
        if self.category_worker.is_active() || !self.pending_tree_edits.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn split_drive_letter_keeps_the_letter() {
        assert_eq!(split_drive_letter("Windows (C:)"), ("Windows", "(C:)"));
        assert_eq!(
            split_drive_letter("Backup and archived files (E:)"),
            ("Backup and archived files", "(E:)")
        );
    }

    #[test]
    fn split_drive_letter_leaves_other_names_alone() {
        assert_eq!(split_drive_letter("Macintosh HD"), ("Macintosh HD", ""));
        assert_eq!(split_drive_letter("Photos (2024)"), ("Photos (2024)", ""));
        assert_eq!(split_drive_letter("(C:)"), ("(C:)", ""));
        // A Unix volume that merely looks like one keeps its whole name.
        assert_eq!(split_drive_letter("Backup (1:)"), ("Backup (1:)", ""));
        assert_eq!(split_drive_letter("資料 (é:)"), ("資料 (é:)", ""));
    }

    use super::*;
    use disk_cleaner::tree::{DirNode, FileLeaf, FileNode};

    fn leaf(name: &str, size: u64) -> FileNode {
        FileNode::File(FileLeaf::new(name.into(), size, name.starts_with('.')))
    }

    fn dir(name: &str, children: Vec<FileNode>) -> FileNode {
        let size = children.iter().map(|c| c.size()).sum();
        FileNode::Dir(Box::new(DirNode {
            name: name.into(),
            size,
            children,
            expanded: false,
            hidden: name.starts_with('.'),
        }))
    }

    #[test]
    fn tree_can_render_before_category_counting_starts() {
        let mut tree = dir("root", vec![leaf("movie.mp4", 100)]);
        tree.set_expanded(true);
        let mut app = App {
            tree: Some(Arc::new(tree)),
            show_hidden: true,
            ..App::default()
        };
        app.rebuild_rows_if_dirty();
        assert_eq!(app.cached_rows.len(), 2);
        assert!(app.category_stats.is_none());
        assert!(!app.category_worker.is_active());
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui::render_tree(
                ui,
                &app.cached_rows,
                &None,
                None,
                false,
                &app.selected_paths,
            );
        });
        app.start_categories();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.category_stats.is_none() {
            app.poll_categories();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            app.category_stats.unwrap().entries,
            [(categories::FileCategory::Video, 100, 1)]
        );
    }

    #[test]
    fn removal_discards_completed_category_result() {
        let mut app = App {
            tree: Some(Arc::new(dir("root", vec![leaf("a.zip", 100)]))),
            ..App::default()
        };
        app.start_categories();
        let deadline = Instant::now() + Duration::from_secs(5);
        // Wait for counting to finish without polling its result. The worker
        // releases its tree only after computing the old totals.
        while Arc::strong_count(app.tree.as_ref().unwrap()) != 1 {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(app.category_worker.is_active());
        app.edit_tree(TreeEdit::Remove(PathBuf::from("root/a.zip")));
        assert_eq!(app.tree.as_ref().unwrap().size(), 0);
        while app.category_worker.is_active() {
            app.poll_categories();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(app.category_stats.is_none(), "old totals must be discarded");
        app.start_categories();
        while app.category_stats.is_none() {
            app.poll_categories();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(app.category_stats.unwrap().entries.is_empty());
    }

    #[test]
    fn edits_wait_without_cloning_and_recount_after_removal() {
        let tree = Arc::new(dir("root", vec![dir("folder", vec![leaf("a.zip", 100)])]));
        let reader = tree.clone();
        let mut app = App {
            category_stats: Some(categories::compute_stats(&tree)),
            tree: Some(tree),
            ..App::default()
        };
        app.edit_tree(TreeEdit::Expand(PathBuf::from("root/folder"), true));
        app.edit_tree(TreeEdit::Remove(PathBuf::from("root/folder/a.zip")));
        assert_eq!(app.pending_tree_edits.len(), 2);
        assert!(Arc::ptr_eq(app.tree.as_ref().unwrap(), &reader));
        assert_eq!(reader.size(), 100);
        drop(reader);
        app.apply_tree_edits();
        assert!(app.pending_tree_edits.is_empty());
        assert!(app.category_stats.is_none());
        let tree = app.tree.as_ref().unwrap();
        assert_eq!(tree.size(), 0);
        assert!(tree.children()[0].expanded());
        app.start_categories();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.category_stats.is_none() {
            app.poll_categories();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(app.category_stats.unwrap().entries.is_empty());
    }

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

        let got = resolve_batch_targets(
            &rows,
            Some(&tree),
            vec![PathBuf::from("root/__file_group__")],
            true,
        );

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

        let got = resolve_batch_targets(
            &rows,
            Some(&tree),
            vec![PathBuf::from("root/__file_group__")],
            true,
        );

        assert_eq!(got, vec![PathBuf::from("root/__file_group__")]);
    }

    #[test]
    fn resolve_ordinary_file_maps_to_itself() {
        let mut tree = dir("root", vec![leaf("a.txt", 10), leaf("b.txt", 20)]);
        tree.set_expanded(true);
        let rows = rows_for(&tree);

        assert_eq!(
            resolve_batch_targets(&rows, Some(&tree), vec![PathBuf::from("root/a.txt")], true),
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
            resolve_batch_targets(
                &rows,
                Some(&tree),
                vec![PathBuf::from("root/__file_group__")],
                true
            ),
            vec![PathBuf::from("root/__file_group__")]
        );
    }

    #[test]
    fn resolve_group_without_tree_is_empty() {
        let mut tree = dir("root", vec![leaf("a.txt", 10), leaf("b.txt", 20)]);
        tree.set_expanded(true);
        let rows = rows_for(&tree);
        assert!(
            resolve_batch_targets(
                &rows,
                None,
                vec![PathBuf::from("root/__file_group__")],
                true
            )
            .is_empty()
        );
    }
}
