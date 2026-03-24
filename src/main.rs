mod app_icon;
mod categories;
mod icons;
mod scanner;
mod suggestions;
mod suggestions_ui;
mod tree;
mod treemap;
mod ui;

use eframe::egui;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use scanner::ScanProgress;
use tree::FileNode;
use treemap::TreemapAction;

/// Result of background deletion: list of (path, optional error message).
type DeleteResults = Vec<(PathBuf, Option<String>)>;

#[derive(PartialEq, Clone, Copy)]
enum ViewMode {
    Tree,
    Treemap,
    Suggestions,
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
                let p = PathBuf::from(other);
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
            .with_icon(app_icon::generate()),
        ..Default::default()
    };

    eframe::run_native(
        "Disk Cleaner",
        options,
        Box::new(move |_cc| {
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

struct App {
    tree: Option<FileNode>,
    scanning: bool,
    scan_path: Option<PathBuf>,
    scan_progress: Arc<ScanProgress>,
    receiver: Option<mpsc::Receiver<FileNode>>,
    error: Option<String>,
    confirm_delete: Option<PathBuf>,
    confirm_batch_delete: bool,
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
    /// Smart cleanup suggestions computed after scan.
    suggestion_report: Option<suggestions::SuggestionReport>,
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
    deleting: bool,
    delete_progress: Arc<AtomicUsize>,
    delete_total: usize,
    delete_receiver: Option<mpsc::Receiver<DeleteResults>>,
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
                cancelled: false.into(),
            }),
            receiver: None,
            error: None,
            confirm_delete: None,
            confirm_batch_delete: false,
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
            show_categories: false,
            tree_scroll_to_focus: false,
            cached_rows: Vec::new(),
            rows_dirty: true,
            treemap_cache: None,
            treemap_dirty: true,
            selected_paths: HashSet::new(),
            selection_anchor: None,
            suggestion_report: None,
            process_start: None,
            scan_frame_times: Vec::new(),
            scan_start_time: None,
            screenshot_prefix: None,
            screenshot_state: ScreenshotState::Idle,
            screenshots_saved: 0,
            deleting: false,
            delete_progress: Arc::new(AtomicUsize::new(0)),
            delete_total: 0,
            delete_receiver: None,
        }
    }
}

impl App {
    fn cancel_scan(&mut self) {
        self.scan_progress.cancelled.store(true, Ordering::Relaxed);
        self.scanning = false;
        self.receiver = None;
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
            cancelled: false.into(),
        });
        self.scan_progress = progress.clone();

        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);

        self.scan_start_time = Some(Instant::now());
        self.scan_frame_times.clear();

        thread::spawn(move || {
            let tree = scanner::scan_directory(&path, progress);
            let _ = tx.send(tree);
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

        if let Some(ref tree) = self.tree {
            self.cached_rows = ui::collect_cached_rows(
                tree,
                &self.applied_search,
                self.category_filter,
                self.show_hidden,
                text_cache.as_ref(),
                cat_cache.as_ref(),
            );
        } else {
            self.cached_rows.clear();
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
        self.start_background_delete(paths, true);
    }

    fn batch_delete_selected(&mut self) {
        let paths: Vec<PathBuf> = self.selected_paths.drain().collect();
        self.start_background_delete(paths, false);
    }

    /// Spawn deletion on a background thread so the UI stays responsive.
    fn start_background_delete(&mut self, paths: Vec<PathBuf>, use_trash: bool) {
        if paths.is_empty() || self.deleting {
            return;
        }
        let total = paths.len();
        let progress = Arc::new(AtomicUsize::new(0));
        let progress_clone = Arc::clone(&progress);
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            // Collect results: (path, error_or_none)
            let mut results: Vec<(PathBuf, Option<String>)> = Vec::with_capacity(total);
            for path in paths {
                let result = if use_trash {
                    trash::delete(&path).map_err(|e| e.to_string())
                } else if path.is_dir() {
                    std::fs::remove_dir_all(&path).map_err(|e| e.to_string())
                } else {
                    std::fs::remove_file(&path).map_err(|e| e.to_string())
                };
                let err = result.err();
                results.push((path, err));
                progress_clone.fetch_add(1, Ordering::Relaxed);
            }
            let _ = tx.send(results);
        });

        self.deleting = true;
        self.delete_progress = progress;
        self.delete_total = total;
        self.delete_receiver = Some(rx);
    }

    /// Poll for background deletion completion and apply results to the tree.
    fn poll_delete_completion(&mut self) {
        if !self.deleting {
            return;
        }
        if let Some(ref rx) = self.delete_receiver {
            if let Ok(results) = rx.try_recv() {
                let mut any_deleted = false;
                for (path, err) in results {
                    if let Some(msg) = err {
                        self.error = Some(format!("Delete failed: {msg}"));
                    } else {
                        any_deleted = true;
                        if let Some(ref mut tree) = self.tree {
                            ui::remove_node(tree, &path);
                            self.mark_dirty();
                        }
                    }
                }
                if any_deleted {
                    self.refresh_disk_info();
                }
                self.deleting = false;
                self.delete_receiver = None;
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let frame_start = Instant::now();

        // Log startup time on first frame
        if let Some(start) = self.process_start.take() {
            eprintln!("[perf] startup → first frame: {:?}", start.elapsed());
        }

        // Apply debounced search query after 150ms of no typing
        if let Some(changed_at) = self.search_changed_at {
            if changed_at.elapsed() >= Duration::from_millis(150) {
                self.applied_search = self.search_query.clone();
                self.search_changed_at = None;
                self.mark_dirty();
            } else {
                let remaining = Duration::from_millis(150).saturating_sub(changed_at.elapsed());
                ctx.request_repaint_after(remaining);
            }
        }

        // Load system icons on first frame
        if self.icon_cache.is_none() {
            self.icon_cache = icons::IconCache::load(ctx);
        }

        // Check if scan completed
        if let Some(ref rx) = self.receiver {
            if let Ok(tree) = rx.try_recv() {
                self.category_stats = Some(categories::compute_stats(&tree));
                self.suggestion_report = Some(suggestions::analyze(&tree));
                self.tree = Some(tree);
                if let Some(ref mut t) = self.tree {
                    tree::auto_expand(t, 0, 2);
                }
                self.last_scan_file_count = self.scan_progress.file_count.load(Ordering::Relaxed);
                self.last_scan_total_size = self.scan_progress.total_size.load(Ordering::Relaxed);
                self.scanning = false;
                self.receiver = None;
                self.category_filter = None;
                self.mark_dirty();

                // Report frame-time stats for the scan
                if let Some(scan_start) = self.scan_start_time.take() {
                    let scan_dur = scan_start.elapsed();
                    let ft = &mut self.scan_frame_times;
                    ft.sort();
                    let n = ft.len();
                    if n > 0 {
                        let avg: Duration = ft.iter().sum::<Duration>() / n as u32;
                        let p99 = ft[(n as f64 * 0.99) as usize];
                        let over = ft
                            .iter()
                            .filter(|d| **d > Duration::from_millis(16))
                            .count();
                        eprintln!(
                            "[perf] scan done in {scan_dur:?} ({} files)",
                            self.last_scan_file_count
                        );
                        eprintln!("[perf] frame times (n={n}): min={:?} med={:?} avg={avg:?} p99={p99:?} max={:?}",
                            ft[0], ft[n / 2], ft[n - 1]);
                        eprintln!(
                            "[perf] frames >16ms: {over}/{n} ({:.1}%)",
                            over as f64 / n as f64 * 100.0
                        );
                    }
                    ft.clear();
                }
            }
        }

        // Check if background deletion completed
        self.poll_delete_completion();
        if self.deleting {
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
                                ViewMode::Suggestions => "suggestions",
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
                                    self.screenshot_state =
                                        ScreenshotState::NextView(ViewMode::Suggestions);
                                }
                                ViewMode::Suggestions => {
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

            if left || right {
                if let Some(ref focused) = self.focused_path.clone() {
                    if let Some(ref mut tree) = self.tree {
                        if let Some((is_dir, expanded, has_children)) =
                            ui::find_node_info(tree, focused)
                        {
                            if left {
                                if is_dir && expanded {
                                    ui::set_expanded(tree, focused, false);
                                    self.mark_dirty();
                                } else if let Some(parent) = ui::find_parent_path(tree, focused) {
                                    self.focused_path = Some(parent);
                                    self.tree_scroll_to_focus = true;
                                }
                            } else if right {
                                if is_dir && !expanded && has_children {
                                    ui::set_expanded(tree, focused, true);
                                    self.mark_dirty();
                                } else if is_dir && expanded {
                                    let rows = &self.cached_rows;
                                    if let Some(idx) = rows.iter().position(|r| &r.path == focused) {
                                        if idx + 1 < rows.len() {
                                            self.focused_path = Some(rows[idx + 1].path.clone());
                                            self.tree_scroll_to_focus = true;
                                        }
                                    }
                                }
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
                    self.confirm_delete = Some(focused.clone());
                } else if del {
                    if let Err(e) = trash::delete(focused) {
                        self.error = Some(format!("Trash failed: {e}"));
                    } else {
                        if let Some(ref mut tree) = self.tree {
                            ui::remove_node(tree, focused);
                            self.mark_dirty();
                        }
                        self.refresh_disk_info();
                    }
                    self.selected_paths.remove(focused);
                    self.focused_path = None;
                }
            }
        }

        // Batch delete confirmation dialog
        let mut do_batch_delete = false;
        let mut close_batch_dialog = false;

        if self.confirm_batch_delete {
            let selected_count = self.selected_paths.len();
            let enter_pressed = ctx.input(|i| i.key_pressed(egui::Key::Enter));
            egui::Window::new("Confirm Batch Delete")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Permanently delete {} selected item(s)? This cannot be undone.",
                        selected_count
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

        if close_batch_dialog {
            self.confirm_batch_delete = false;
        }

        if do_batch_delete {
            self.batch_delete_selected();
        }

        // Single-item delete confirmation dialog
        let mut do_delete: Option<PathBuf> = None;
        let mut close_dialog = false;

        if let Some(ref path) = self.confirm_delete {
            let enter_pressed = ctx.input(|i| i.key_pressed(egui::Key::Enter));
            egui::Window::new("Confirm Delete")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Permanently delete?\n{}", path.display()));
                    ui.horizontal(|ui| {
                        let delete_btn = egui::Button::new(
                            egui::RichText::new("Yes, delete").color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(220, 50, 50));
                        if ui.add(delete_btn).clicked() || enter_pressed {
                            do_delete = Some(path.clone());
                            close_dialog = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close_dialog = true;
                        }
                    });
                });
        }

        if close_dialog {
            self.confirm_delete = None;
        }

        if let Some(path) = do_delete {
            let result = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };

            match result {
                Ok(()) => {
                    if let Some(ref mut tree) = self.tree {
                        ui::remove_node(tree, &path);
                        self.mark_dirty();
                    }
                    self.selected_paths.remove(&path);
                    self.refresh_disk_info();
                }
                Err(e) => {
                    self.error = Some(format!("Delete failed: {e}"));
                }
            }
        }

        // Top panel with toolbar (hidden on home page where it only has "Open Directory")
        let show_toolbar = self.tree.is_some() || self.scanning;
        if show_toolbar {
            egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Standardize widget height so buttons and selectable labels align
                    ui.spacing_mut().interact_size.y = 24.0;

                    if ui.button("Open Directory...").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.start_scan(path);
                        }
                    }

                    if self.tree.is_some() && ui.button("Re-scan").clicked() {
                        if let Some(path) = self.scan_path.clone() {
                            self.start_scan(path);
                        }
                    }

                    // View mode toggle
                    if self.tree.is_some() {
                        ui.separator();
                        for (label, mode) in [
                            ("Tree", ViewMode::Tree),
                            ("Treemap", ViewMode::Treemap),
                            ("Suggestions", ViewMode::Suggestions),
                        ] {
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

                    // Search/filter bar
                    if self.tree.is_some() {
                        ui.separator();
                        ui.label("Filter:");
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut self.search_query)
                                .hint_text("file name...")
                                .desired_width(200.0),
                        );
                        if response.changed() {
                            // Convert to lowercase once; node_matches uses lowercase comparison
                            self.search_query = self.search_query.to_lowercase();
                            self.search_changed_at = Some(Instant::now());
                        }
                        if !self.search_query.is_empty() && ui.small_button("×").clicked() {
                            self.search_query.clear();
                            self.applied_search.clear();
                            self.search_changed_at = None;
                            self.mark_dirty();
                        }
                    }

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
                        // Full-page scanning UI is in CentralPanel; just keep repainting
                        ctx.request_repaint();
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

        // Bottom status bar with scan info + selection + keyboard hints
        egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                // Left: scan info or focused path + selection count
                if self.tree.is_some() && !self.scanning {
                    let selected_count = self.selected_paths.len();
                    if let Some(ref focused) = self.focused_path {
                        let display = focused
                            .file_name()
                            .map(|f| f.to_string_lossy().into_owned())
                            .unwrap_or_else(|| focused.display().to_string());
                        let mut status = display;
                        if selected_count > 1 {
                            status = format!("{status} ({selected_count} selected)");
                        } else if selected_count == 1 {
                            status = format!("{status} (1 selected)");
                        }
                        ui.label(egui::RichText::new(status).small());
                    } else if selected_count > 0 {
                        ui.label(egui::RichText::new(format!("{selected_count} selected")).small());
                    } else if let Some(ref path) = self.scan_path {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} files \u{2014} {}",
                                self.last_scan_file_count,
                                bytesize::ByteSize::b(self.last_scan_total_size)
                            ))
                            .small(),
                        );
                        let _ = path; // used for context above
                    }
                } else if let Some(ref path) = self.scan_path {
                    if !self.scanning && self.last_scan_file_count > 0 {
                        ui.label(
                            egui::RichText::new(format!(
                                "Scanned: {} \u{2014} {} files \u{2014} {}",
                                path.display(),
                                self.last_scan_file_count,
                                bytesize::ByteSize::b(self.last_scan_total_size)
                            ))
                            .small(),
                        );
                    }
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
                    if self.scan_is_volume {
                        if let Some((total, available)) = self.scan_disk_info {
                            let used = total.saturating_sub(available);
                            if used > 0 {
                                ui.add_space(12.0);
                                let fraction = (size as f32 / used as f32).clamp(0.0, 1.0);
                                let bar = egui::ProgressBar::new(fraction).desired_width(300.0);
                                ui.add(bar);
                            }
                        }
                    }

                    // Elapsed time
                    if let Some(start) = self.scan_start_time {
                        ui.add_space(8.0);
                        let elapsed = start.elapsed();
                        let secs = elapsed.as_secs();
                        let elapsed_str = if secs >= 60 {
                            format!("{}m {:02}s", secs / 60, secs % 60)
                        } else {
                            format!("{secs}s")
                        };
                        ui.label(
                            egui::RichText::new(format!("Elapsed: {elapsed_str}"))
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
                    if ui.button("Open Directory...").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.start_scan(path);
                        }
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
                    if render_elapsed > std::time::Duration::from_millis(16) {
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
                                        let anchor_idx = rows.iter().position(|r| &r.path == anchor);
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
                                if let Err(e) = trash::delete(path) {
                                    self.error = Some(format!("Trash failed: {e}"));
                                } else {
                                    if let Some(ref mut tree) = self.tree {
                                        ui::remove_node(tree, path);
                                        self.mark_dirty();
                                    }
                                    self.refresh_disk_info();
                                }
                                self.selected_paths.remove(path);
                            }
                            ui::TreeAction::TrashSelected => {
                                self.batch_trash_selected();
                            }
                            ui::TreeAction::ConfirmDelete(path) => {
                                self.confirm_delete = Some(path.clone());
                            }
                            ui::TreeAction::ConfirmDeleteSelected => {
                                self.confirm_batch_delete = true;
                            }
                            ui::TreeAction::RevealInFinder(path) => {
                                let _ = std::process::Command::new("open")
                                    .arg("-R")
                                    .arg(path)
                                    .spawn();
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
                            if let ui::TreeAction::ToggleExpand(path) = action {
                                ui::toggle_expand(tree, path);
                                self.rows_dirty = true;
                                self.treemap_dirty = true;
                            }
                        }
                    }
                }
                ViewMode::Treemap => {
                    if let Some(ref tree) = self.tree {
                        let available = ui.available_size();
                        let needs_rebuild = self.treemap_dirty
                            || self.treemap_cache.is_none()
                            || self.treemap_cache.as_ref().is_some_and(|c| {
                                (c.layout_size.0 - available.x).abs() > 1.0
                                    || (c.layout_size.1 - available.y).abs() > 1.0
                            });

                        if needs_rebuild {
                            let rect = egui::Rect::from_min_size(
                                ui.cursor().min,
                                available,
                            );
                            self.treemap_cache = Some(treemap::build_treemap_cache(
                                tree,
                                &self.treemap_zoom,
                                self.category_filter,
                                self.show_hidden,
                                rect,
                            ));
                            self.treemap_dirty = false;
                        }

                        let tm_actions = if let Some(ref cache) = self.treemap_cache {
                            treemap::render_treemap(
                                ui,
                                cache,
                                &self.focused_path,
                                self.treemap_zoom_anim,
                            )
                        } else {
                            Vec::new()
                        };

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
                ViewMode::Suggestions => {
                    let mut needs_disk_refresh = false;
                    if let Some(ref mut report) = self.suggestion_report {
                        let sg_actions = suggestions_ui::render_suggestions(ui, report);
                        for action in sg_actions {
                            match action {
                                suggestions_ui::SuggestionAction::ToggleGroup(idx) => {
                                    report.groups[idx].expanded = !report.groups[idx].expanded;
                                }
                                suggestions_ui::SuggestionAction::TrashItem(path) => {
                                    if let Err(e) = trash::delete(&path) {
                                        self.error = Some(format!("Trash failed: {e}"));
                                    } else {
                                        if let Some(ref mut tree) = self.tree {
                                            ui::remove_node(tree, &path);
                                            self.rows_dirty = true;
                                            self.treemap_dirty = true;
                                        }
                                        needs_disk_refresh = true;
                                    }
                                }
                                suggestions_ui::SuggestionAction::TrashGroup(idx) => {
                                    let paths: Vec<PathBuf> = report.groups[idx]
                                        .items
                                        .iter()
                                        .map(|i| i.path.clone())
                                        .collect();
                                    for path in paths {
                                        if let Err(e) = trash::delete(&path) {
                                            self.error = Some(format!("Trash failed: {e}"));
                                            break;
                                        } else {
                                            if let Some(ref mut tree) = self.tree {
                                                ui::remove_node(tree, &path);
                                                self.rows_dirty = true;
                                                self.treemap_dirty = true;
                                            }
                                            needs_disk_refresh = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if needs_disk_refresh {
                        self.refresh_disk_info();
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
                                    self.confirm_batch_delete = true;
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
        if self.deleting {
            let done = self.delete_progress.load(Ordering::Relaxed);
            let total = self.delete_total;
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

        // Record frame time while scanning
        if self.scanning {
            self.scan_frame_times.push(frame_start.elapsed());
        }
    }
}
