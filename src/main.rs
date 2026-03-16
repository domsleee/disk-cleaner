mod app_icon;
mod categories;
mod icons;
mod scanner;
mod tree;
mod treemap;
mod ui;

use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use scanner::ScanProgress;
use tree::FileNode;
use treemap::TreemapAction;

#[derive(PartialEq)]
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

fn print_help() {
    eprintln!("Usage: disk-cleaner [OPTIONS] [PATH]");
    eprintln!();
    eprintln!("Arguments:");
    eprintln!("  [PATH]  Directory to scan on launch");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -h, --help  Print this help message");
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut initial_path: Option<PathBuf> = None;

    for arg in &args {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
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
            let mut app = App::default();
            if let Some(path) = initial_path {
                app.start_scan(path);
            }
            Ok(Box::new(app))
        }),
    )
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
    focused_path: Option<PathBuf>,
    last_scan_path: Option<PathBuf>,
    view_mode: ViewMode,
    treemap_zoom: Option<PathBuf>,
    treemap_zoom_anim: Option<f64>,
    volumes: Vec<scanner::VolumeInfo>,
    volumes_last_refresh: Option<std::time::Instant>,
    scan_disk_info: Option<(u64, u64)>, // (total, available) for scan path
    category_filter: Option<categories::FileCategory>,
    category_stats: Option<categories::CategoryStats>,
    show_hidden: bool,
    icon_cache: Option<icons::IconCache>,
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
            focused_path: None,
            last_scan_path,
            view_mode: ViewMode::Tree,
            treemap_zoom: None,
            treemap_zoom_anim: None,
            volumes: scanner::list_volumes(),
            volumes_last_refresh: Some(std::time::Instant::now()),
            scan_disk_info: None,
            category_filter: None,
            category_stats: None,
            show_hidden,
            icon_cache: None,
        }
    }
}

impl App {
    fn cancel_scan(&mut self) {
        self.scan_progress
            .cancelled
            .store(true, Ordering::Relaxed);
        self.scanning = false;
        self.receiver = None;
    }

    fn start_scan(&mut self, path: PathBuf) {
        // Cancel any in-progress scan so its threads release the rayon pool
        self.scan_progress
            .cancelled
            .store(true, Ordering::Relaxed);

        save_config(&path, self.show_hidden);
        self.last_scan_path = Some(path.clone());
        self.scanning = true;
        self.error = None;
        self.tree = None;
        self.scan_path = Some(path.clone());
        self.scan_disk_info = scanner::disk_space(&path);

        let progress = Arc::new(ScanProgress {
            file_count: 0.into(),
            total_size: 0.into(),
            cancelled: false.into(),
        });
        self.scan_progress = progress.clone();

        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);

        thread::spawn(move || {
            let tree = scanner::scan_directory(&path, progress);
            let _ = tx.send(tree);
        });
    }

    fn batch_trash_selected(&mut self) {
        let paths = self
            .tree
            .as_ref()
            .map(ui::collect_selected)
            .unwrap_or_default();
        for path in paths {
            if let Err(e) = trash::delete(&path) {
                self.error = Some(format!("Trash failed: {e}"));
                break;
            } else if let Some(ref mut tree) = self.tree {
                ui::remove_node(tree, &path);
            }
        }
    }

    fn batch_delete_selected(&mut self) {
        let paths = self
            .tree
            .as_ref()
            .map(ui::collect_selected)
            .unwrap_or_default();
        for path in paths {
            let result = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            match result {
                Ok(()) => {
                    if let Some(ref mut tree) = self.tree {
                        ui::remove_node(tree, &path);
                    }
                }
                Err(e) => {
                    self.error = Some(format!("Delete failed: {e}"));
                    break;
                }
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Load system icons on first frame
        if self.icon_cache.is_none() {
            self.icon_cache = icons::IconCache::load(ctx);
        }

        // Check if scan completed
        if let Some(ref rx) = self.receiver {
            if let Ok(tree) = rx.try_recv() {
                self.category_stats = Some(categories::compute_stats(&tree));
                self.tree = Some(tree);
                if let Some(ref mut t) = self.tree {
                    tree::auto_expand(t, 0, 2);
                }
                self.scanning = false;
                self.receiver = None;
                self.category_filter = None;
            }
        }

        // Keyboard shortcuts (only when no text input is focused)
        let has_text_focus = ctx.memory(|m| m.focused().is_some());
        if !has_text_focus {
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
                    }
                } else if shift_del {
                    self.confirm_delete = Some(focused.clone());
                } else if del {
                    if let Err(e) = trash::delete(focused) {
                        self.error = Some(format!("Trash failed: {e}"));
                    } else if let Some(ref mut tree) = self.tree {
                        ui::remove_node(tree, focused);
                    }
                    self.focused_path = None;
                }
            }
        }

        // Batch delete confirmation dialog
        let mut do_batch_delete = false;
        let mut close_batch_dialog = false;

        if self.confirm_batch_delete {
            let selected_count = self.tree.as_ref().map(ui::count_selected).unwrap_or(0);
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
                        if ui.button("Yes, delete all").clicked() {
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
            egui::Window::new("Confirm Delete")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Permanently delete?\n{}", path.display()));
                    ui.horizontal(|ui| {
                        if ui.button("Yes, delete").clicked() {
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
                    }
                }
                Err(e) => {
                    self.error = Some(format!("Delete failed: {e}"));
                }
            }
        }

        // Top panel with toolbar
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open Directory...").clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        self.start_scan(path);
                    }
                }

                if self.tree.is_some() && ui.button("Re-scan").clicked() {
                    if let Some(ref tree) = self.tree {
                        let path = tree.path.clone();
                        self.start_scan(path);
                    }
                }

                // View mode toggle
                if self.tree.is_some() {
                    ui.separator();
                    let tree_label = if self.view_mode == ViewMode::Tree {
                        egui::RichText::new("Tree").strong()
                    } else {
                        egui::RichText::new("Tree")
                    };
                    let map_label = if self.view_mode == ViewMode::Treemap {
                        egui::RichText::new("Treemap").strong()
                    } else {
                        egui::RichText::new("Treemap")
                    };
                    if ui
                        .selectable_label(self.view_mode == ViewMode::Tree, tree_label)
                        .clicked()
                    {
                        self.view_mode = ViewMode::Tree;
                    }
                    if ui
                        .selectable_label(self.view_mode == ViewMode::Treemap, map_label)
                        .clicked()
                    {
                        self.view_mode = ViewMode::Treemap;
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
                    }
                    if !self.search_query.is_empty() && ui.small_button("✕").clicked() {
                        self.search_query.clear();
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

                // Batch operation buttons (only shown when items are selected)
                let selected_count = self.tree.as_ref().map(ui::count_selected).unwrap_or(0);
                if selected_count > 0 {
                    ui.separator();
                    if ui
                        .button(format!("Trash Selected ({selected_count})"))
                        .clicked()
                    {
                        self.batch_trash_selected();
                    }
                    if ui
                        .button(format!("Delete Selected ({selected_count})"))
                        .clicked()
                    {
                        self.confirm_batch_delete = true;
                    }
                }

                // Disk space info (shown when scan is done)
                if self.tree.is_some() && !self.scanning {
                    if let Some((total, available)) = self.scan_disk_info {
                        ui.separator();
                        let used = total.saturating_sub(available);
                        ui.monospace(format!(
                            "{} used / {} ({} free)",
                            bytesize::ByteSize::b(used),
                            bytesize::ByteSize::b(total),
                            bytesize::ByteSize::b(available),
                        ));
                    }
                }

                if self.scanning {
                    if ui.small_button("Cancel").clicked() {
                        self.cancel_scan();
                    }
                    ui.spinner();
                    let files = self.scan_progress.file_count.load(Ordering::Relaxed);
                    let size = self.scan_progress.total_size.load(Ordering::Relaxed);
                    let size_str = bytesize::ByteSize::b(size).to_string();
                    let path_str = self
                        .scan_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    ui.monospace(format!(
                        "Indexing: {path_str} {files} files, {size_str} ..."
                    ));
                    ctx.request_repaint();
                }

                if let Some(ref err) = self.error {
                    ui.colored_label(egui::Color32::RED, err);
                }
            });
        });

        // Category side panel (only when scan results are available)
        if self.tree.is_some() && !self.scanning {
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
                                    "{} | {} files",
                                    bytesize::ByteSize::b(size),
                                    count
                                ));
                            });

                            if response.clicked() {
                                if is_active {
                                    self.category_filter = None;
                                } else {
                                    self.category_filter = Some(cat);
                                }
                            }

                            ui.add_space(2.0);
                        }
                    }
                });
        }

        // Bottom status bar with version
        egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                        .small()
                        .weak(),
                );
            });
        });

        // Main content
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.tree.is_none() && !self.scanning {
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

                            egui::Frame::group(ui.style())
                                .inner_margin(12.0)
                                .show(ui, |ui| {
                                    ui.set_width(400.0);
                                    ui.horizontal(|ui| {
                                        ui.strong(&vol.name);
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.monospace(
                                                    bytesize::ByteSize::b(vol.total_bytes)
                                                        .to_string(),
                                                );
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

                                    ui.horizontal(|ui| {
                                        ui.label(format!(
                                            "{:.0}% used — {} free",
                                            fraction * 100.0,
                                            bytesize::ByteSize::b(vol.available_bytes)
                                        ));
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui.button("Scan").clicked() {
                                                    scan_path = Some(vol.path.clone());
                                                }
                                            },
                                        );
                                    });
                                });
                            ui.add_space(4.0);
                        }

                        if let Some(path) = scan_path {
                            self.start_scan(path);
                        }

                        ui.add_space(12.0);
                        ui.separator();
                        ui.add_space(8.0);
                    }

                    // Folder pick alternative
                    if ui.button("Open Directory...").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.start_scan(path);
                        }
                    }

                    // Resume last scan
                    if let Some(ref last) = self.last_scan_path.clone() {
                        ui.add_space(8.0);
                        let label = format!("Resume last scan: {}", last.display());
                        if ui.button(label).clicked() {
                            self.start_scan(last.clone());
                        }
                    }
                });
                return;
            }

            match self.view_mode {
                ViewMode::Tree => {
                    let filter = self.search_query.clone();
                    let cat_filter = self.category_filter;
                    let show_hidden = self.show_hidden;
                    let mut focused_path = self.focused_path.clone();
                    let mut row_clicks = Vec::new();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                        if let Some(ref mut tree) = self.tree {
                            let root_size = tree.size;
                            ui::render_tree(
                                ui,
                                tree,
                                0,
                                root_size,
                                &filter,
                                &mut focused_path,
                                cat_filter,
                                show_hidden,
                                self.icon_cache.as_ref(),
                                &mut row_clicks,
                            );
                        }
                    });
                    self.focused_path = focused_path;
                    // Apply selection changes from row clicks
                    if !row_clicks.is_empty() {
                        if let Some(ref mut tree) = self.tree {
                            ui::apply_row_clicks(tree, &row_clicks);
                        }
                    }
                }
                ViewMode::Treemap => {
                    if let Some(ref tree) = self.tree {
                        let tm_actions = treemap::render_treemap(
                            ui,
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
                                    let is_root = tree.path == path;
                                    let new_zoom = if is_root { None } else { Some(path) };
                                    if new_zoom != self.treemap_zoom {
                                        self.treemap_zoom_anim = Some(ctx.input(|i| i.time));
                                        self.treemap_zoom = new_zoom;
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
    }
}
