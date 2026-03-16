mod scanner;
mod tree;
mod ui;

use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use scanner::ScanProgress;
use tree::FileNode;
use ui::NodeAction;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1200.0, 800.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Disk Cleaner",
        options,
        Box::new(|_cc| Ok(Box::new(App::default()))),
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
}

impl Default for App {
    fn default() -> Self {
        Self {
            tree: None,
            scanning: false,
            scan_path: None,
            scan_progress: Arc::new(ScanProgress {
                file_count: 0.into(),
                total_size: 0.into(),
            }),
            receiver: None,
            error: None,
            confirm_delete: None,
            confirm_batch_delete: false,
            search_query: String::new(),
        }
    }
}

impl App {
    fn start_scan(&mut self, path: PathBuf) {
        self.scanning = true;
        self.error = None;
        self.tree = None;
        self.scan_path = Some(path.clone());

        let progress = Arc::new(ScanProgress {
            file_count: 0.into(),
            total_size: 0.into(),
        });
        self.scan_progress = progress.clone();

        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);

        thread::spawn(move || {
            let tree = scanner::scan_directory(&path, progress);
            let _ = tx.send(tree);
        });
    }

    fn process_actions(&mut self, actions: Vec<NodeAction>) {
        for action in actions {
            match action {
                NodeAction::Trash(path) => {
                    if let Err(e) = trash::delete(&path) {
                        self.error = Some(format!("Trash failed: {e}"));
                    } else if let Some(ref mut tree) = self.tree {
                        ui::remove_node(tree, &path);
                    }
                }
                NodeAction::Delete(path) => {
                    self.confirm_delete = Some(path);
                }
            }
        }
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
        // Check if scan completed
        if let Some(ref rx) = self.receiver {
            if let Ok(tree) = rx.try_recv() {
                self.tree = Some(tree);
                self.scanning = false;
                self.receiver = None;
            }
        }

        // Batch delete confirmation dialog
        let mut do_batch_delete = false;
        let mut close_batch_dialog = false;

        if self.confirm_batch_delete {
            let selected_count = self
                .tree
                .as_ref()
                .map(ui::count_selected)
                .unwrap_or(0);
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

                // Batch operation buttons (only shown when items are selected)
                let selected_count = self
                    .tree
                    .as_ref()
                    .map(ui::count_selected)
                    .unwrap_or(0);
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

                if self.scanning {
                    ui.spinner();
                    let files = self.scan_progress.file_count.load(Ordering::Relaxed);
                    let size = self.scan_progress.total_size.load(Ordering::Relaxed);
                    let size_str = bytesize::ByteSize::b(size).to_string();
                    let path_str = self.scan_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                    ui.monospace(format!("Indexing: {path_str} {files} files, {size_str} ..."));
                    ctx.request_repaint();
                }

                if let Some(ref err) = self.error {
                    ui.colored_label(egui::Color32::RED, err);
                }
            });
        });

        // Main content
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.tree.is_none() && !self.scanning {
                ui.centered_and_justified(|ui| {
                    ui.heading("Click \"Open Directory...\" to scan a folder");
                });
                return;
            }

            let filter = self.search_query.clone();
            egui::ScrollArea::vertical().show(ui, |ui| {
                let mut actions = Vec::new();
                if let Some(ref mut tree) = self.tree {
                    let root_size = tree.size;
                    ui::render_tree(ui, tree, 0, root_size, &mut actions, &filter);
                }
                self.process_actions(actions);
            });
        });
    }
}
