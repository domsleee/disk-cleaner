use std::collections::HashSet;
use std::path::{Path, PathBuf};

use bytesize::ByteSize;
use eframe::egui;

use crate::categories::FileCategory;
use crate::icons::IconCache;
use crate::tree::FileNode;

/// Paint a disclosure triangle (▶ or ▼). Visual only — click detection is
/// handled by the unified row interaction.
fn paint_disclosure(ui: &mut egui::Ui, expanded: bool) -> egui::Rect {
    let size = egui::vec2(16.0, 16.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let color = ui.visuals().text_color();
        let center = rect.center();
        let half = 4.0;
        let triangle = if expanded {
            // Down-pointing triangle
            vec![
                egui::pos2(center.x - half, center.y - half * 0.5),
                egui::pos2(center.x + half, center.y - half * 0.5),
                egui::pos2(center.x, center.y + half * 0.75),
            ]
        } else {
            // Right-pointing triangle
            vec![
                egui::pos2(center.x - half * 0.5, center.y - half),
                egui::pos2(center.x + half * 0.75, center.y),
                egui::pos2(center.x - half * 0.5, center.y + half),
            ]
        };
        ui.painter().add(egui::Shape::convex_polygon(
            triangle,
            color,
            egui::Stroke::NONE,
        ));
    }
    rect
}

fn bar_color(size: u64, ui: &egui::Ui) -> egui::Color32 {
    if size > 1_000_000_000 {
        egui::Color32::from_rgb(52, 152, 219) // blue >1GB
    } else if size > 100_000_000 {
        egui::Color32::from_rgb(100, 170, 220) // lighter blue >100MB
    } else {
        ui.visuals().weak_text_color()
    }
}

/// Returns true if this node's name matches the query or any descendant does.
pub fn node_matches(node: &FileNode, query: &str) -> bool {
    contains_case_insensitive(node.name(), query)
        || node.children().iter().any(|c| node_matches(c, query))
}

/// Pre-compute which subtrees contain nodes matching the text query.
/// Match caches key nodes by identity (address) instead of storing a cloned
/// `PathBuf` per matching node — on a million-node tree the path clones cost
/// ~150 bytes each (~100 MB per cache) while node ids cost 8.
///
/// Invariant: a match set is only valid for lookups against the same tree it
/// was built from, unmoved — build it and consume it within one borrow of the
/// tree (as `refresh_rows` does). It must never be retained across a rescan.
pub type NodeMatchSet = HashSet<usize>;

#[inline]
fn node_id(node: &FileNode) -> usize {
    node as *const FileNode as usize
}

/// Pre-compute which subtrees match the query or have matching descendants.
pub fn build_text_match_cache(node: &FileNode, query: &str) -> NodeMatchSet {
    let mut cache = HashSet::new();
    build_text_match_inner(node, query, &mut cache);
    cache
}

fn build_text_match_inner(node: &FileNode, query: &str, cache: &mut NodeMatchSet) -> bool {
    let self_matches = contains_case_insensitive(node.name(), query);
    // Must visit ALL children (not short-circuit) so every matching subtree is cached.
    let child_matches = node.children().iter().fold(false, |acc, c| {
        acc | build_text_match_inner(c, query, cache)
    });
    if self_matches || child_matches {
        cache.insert(node_id(node));
        true
    } else {
        false
    }
}

/// Pre-compute which subtrees contain nodes matching the given category.
pub fn build_category_match_cache(
    node: &FileNode,
    cat: crate::categories::FileCategory,
) -> NodeMatchSet {
    let mut cache = HashSet::new();
    build_cat_match_inner(node, cat, &mut cache);
    cache
}

fn build_cat_match_inner(
    node: &FileNode,
    cat: crate::categories::FileCategory,
    cache: &mut NodeMatchSet,
) -> bool {
    let self_matches = if node.is_dir() {
        false
    } else {
        crate::categories::categorize(node.name()) == cat
    };
    // Must visit ALL children (not short-circuit) so every matching subtree is cached.
    let child_matches = node
        .children()
        .iter()
        .fold(false, |acc, c| acc | build_cat_match_inner(c, cat, cache));
    if self_matches || child_matches {
        cache.insert(node_id(node));
        true
    } else {
        false
    }
}

/// ASCII case-insensitive substring search without allocating.
/// Only folds a-z/A-Z; non-ASCII characters are compared as-is.
fn contains_case_insensitive(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

/// Actions produced by tree rendering, applied after the frame.
pub enum TreeAction {
    ToggleExpand(PathBuf),
    ToggleFileGroup(PathBuf),
    Click {
        path: PathBuf,
        shift: bool,
        toggle: bool,
    },
    Focus(PathBuf),
    Trash(PathBuf),
    TrashSelected,
    ConfirmDelete(PathBuf),
    ConfirmDeleteSelected,
    RevealInFinder(PathBuf),
    CopyPath(PathBuf),
}

/// Label for the "reveal in file manager" context-menu entry, named for the
/// platform's native file manager.
pub fn reveal_in_file_manager_label() -> &'static str {
    if cfg!(windows) {
        "Reveal in File Explorer"
    } else if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else {
        "Open Containing Folder"
    }
}

/// Size column layout, shared by the header and the rows.
const BAR_WIDTH: f32 = 80.0;
const BAR_GAP: f32 = 4.0;
/// Clears the floating scrollbar's expanded width.
const TEXT_MARGIN: f32 = 14.0;

/// Minimum number of loose files in a folder to trigger grouping.
const FILE_GROUP_THRESHOLD: usize = 2;

/// Final path component of a synthetic file-group row: `<dir>/__file_group__`.
pub const FILE_GROUP_MARKER: &str = "__file_group__";

/// Cached row data for the visible tree. Rebuilt only when the tree state changes.
/// Owns all data so it can outlive a single frame.
pub struct CachedRow {
    pub path: PathBuf,
    pub name: Box<str>,
    pub size: u64,
    pub is_dir: bool,
    pub expanded: bool,
    pub depth: usize,
    pub parent_size: u64,
    pub children_count: usize,
    pub category: crate::categories::FileCategory,
    /// True when the file/folder name starts with `.` or has OS-level hidden flag.
    pub is_hidden: bool,
    /// True for synthetic "N files" summary rows that group loose files.
    pub is_file_group: bool,
    /// True when this file is a hard link (shares its inode / storage with
    /// other directory entries; counted once toward totals).
    pub is_hard_link: bool,
}

/// Collect all visible rows into owned `CachedRow` structs. This replaces both
/// `collect_visible_rows` (rendering data) and `collect_visible_paths` (keyboard
/// nav), producing a single flat list that can be cached across frames.
///
/// When `text_cache` / `cat_cache` are provided, filter checks are O(1) lookups
/// instead of O(N) recursive descents, bringing overall cost from O(N^2) to O(N).
pub fn collect_cached_rows(
    node: &FileNode,
    filter: &str,
    category_filter: Option<FileCategory>,
    show_hidden: bool,
    text_cache: Option<&NodeMatchSet>,
    cat_cache: Option<&NodeMatchSet>,
    expanded_file_groups: Option<&HashSet<PathBuf>>,
) -> Vec<CachedRow> {
    let mut ctx = RowCtx {
        filter,
        category_filter,
        show_hidden,
        text_cache,
        cat_cache,
        expanded_file_groups,
        path: PathBuf::from(node.name()),
        rows: Vec::new(),
    };
    ctx.visit(node, 0, node.size());
    ctx.rows
}

struct RowCtx<'a> {
    filter: &'a str,
    category_filter: Option<FileCategory>,
    show_hidden: bool,
    text_cache: Option<&'a NodeMatchSet>,
    cat_cache: Option<&'a NodeMatchSet>,
    expanded_file_groups: Option<&'a HashSet<PathBuf>>,
    path: PathBuf,
    rows: Vec<CachedRow>,
}

impl RowCtx<'_> {
    fn visit_child(&mut self, child: &FileNode, depth: usize, parent_size: u64) {
        self.path.push(child.name());
        self.visit(child, depth, parent_size);
        self.path.pop();
    }

    fn visit(&mut self, node: &FileNode, depth: usize, parent_size: u64) {
        if !self.show_hidden && node.is_hidden() {
            return;
        }
        // Use pre-computed caches for O(1) lookup when available,
        // fall back to recursive match for backwards compatibility.
        if let Some(tc) = self.text_cache {
            if !tc.contains(&node_id(node)) {
                return;
            }
        } else if !self.filter.is_empty() && !node_matches(node, self.filter) {
            return;
        }
        if let Some(cc) = self.cat_cache {
            if !cc.contains(&node_id(node)) {
                return;
            }
        } else if let Some(cat) = self.category_filter
            && !crate::categories::node_matches_category(node, cat)
        {
            return;
        }

        self.rows.push(CachedRow {
            path: self.path.clone(),
            name: node.name().into(),
            size: node.size(),
            is_dir: node.is_dir(),
            expanded: node.expanded(),
            depth,
            parent_size,
            children_count: node.children().len(),
            category: if node.is_dir() {
                FileCategory::Other
            } else {
                crate::categories::categorize(node.name())
            },
            is_hidden: node.is_hidden(),
            is_file_group: false,
            is_hard_link: node.is_hard_link(),
        });

        if !node.is_dir() || !(node.expanded() || !self.filter.is_empty()) {
            return;
        }
        // Separate children into dirs and files for grouping.
        // Only consider visible files (respecting show_hidden).
        let dirs: Vec<_> = node.children().iter().filter(|c| c.is_dir()).collect();
        let files: Vec<_> = node
            .children()
            .iter()
            .filter(|c| !c.is_dir() && (self.show_hidden || !c.is_hidden()))
            .collect();
        // Never group when any child (visible, hidden, or a dir) is named
        // `__file_group__`, so the synthetic group path stays unambiguous.
        let has_file_group_marker = node
            .children()
            .iter()
            .any(|c| c.name() == FILE_GROUP_MARKER);
        let should_group_files = files.len() >= FILE_GROUP_THRESHOLD
            && self.filter.is_empty()
            && self.category_filter.is_none()
            && !has_file_group_marker;

        if !should_group_files {
            for child in node.children() {
                self.visit_child(child, depth + 1, node.size());
            }
            return;
        }

        // The group sits among the dirs by size (children are already
        // size-sorted from the scanner).
        let file_size: u64 = files.iter().map(|f| f.size()).sum();
        let split = dirs
            .iter()
            .position(|d| d.size() < file_size)
            .unwrap_or(dirs.len());
        for child in &dirs[..split] {
            self.visit_child(child, depth + 1, node.size());
        }
        let group_expanded = self
            .expanded_file_groups
            .is_some_and(|s| s.contains(self.path.as_path()));
        self.rows.push(CachedRow {
            path: self.path.join(FILE_GROUP_MARKER),
            name: format!("[{} files]", files.len()).into(),
            size: file_size,
            is_dir: false,
            expanded: group_expanded,
            depth: depth + 1,
            parent_size: node.size(),
            children_count: files.len(),
            category: FileCategory::Other,
            is_hidden: false,
            is_file_group: true,
            is_hard_link: false,
        });
        if group_expanded {
            for child in &files {
                self.visit_child(child, depth + 2, file_size);
            }
        }
        for child in &dirs[split..] {
            self.visit_child(child, depth + 1, node.size());
        }
    }
}

/// Render the tree view with virtualized scrolling. Returns actions to apply.
/// Accepts pre-built `CachedRow` data so the caller can cache and reuse it.
pub fn render_tree(
    ui: &mut egui::Ui,
    rows: &[CachedRow],
    focused_path: &Option<PathBuf>,
    icon_cache: Option<&IconCache>,
    scroll_to_focus: bool,
    selected_paths: &HashSet<PathBuf>,
) -> Vec<TreeAction> {
    let total_rows = rows.len();
    let row_height = 20.0_f32;
    let mut actions = Vec::new();

    let focused_idx = focused_path
        .as_ref()
        .and_then(|fp| rows.iter().position(|r| r.path == *fp));

    let row_total = row_height + ui.spacing().item_spacing.y;

    // --- Sticky column header, aligned to the same columns as the rows ---
    {
        let right = ui.max_rect().right();
        let font_id = egui::FontId::monospace(ui.style().text_styles[&egui::TextStyle::Body].size);
        let col = ui.visuals().weak_text_color();
        let sample = ui
            .painter()
            .layout_no_wrap("0000000000".to_string(), font_id.clone(), col);
        let text_width = sample.size().x;

        let (hrect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), row_height),
            egui::Sense::hover(),
        );
        let painter = ui.painter();
        let cy = hrect.center().y;
        painter.text(
            egui::pos2(hrect.left() + 4.0, cy),
            egui::Align2::LEFT_CENTER,
            "Name",
            font_id.clone(),
            col,
        );
        painter.text(
            egui::pos2(right - TEXT_MARGIN, cy),
            egui::Align2::RIGHT_CENTER,
            "Size",
            font_id.clone(),
            col,
        );
        let text_x = right - TEXT_MARGIN - text_width;
        let bar_center = text_x - BAR_GAP - BAR_WIDTH / 2.0;
        painter.text(
            egui::pos2(bar_center, cy),
            egui::Align2::CENTER_CENTER,
            "Share",
            font_id,
            col,
        );
        painter.hline(
            hrect.left()..=right,
            hrect.bottom(),
            ui.visuals().widgets.noninteractive.bg_stroke,
        );
    }

    let mut scroll_area = egui::ScrollArea::vertical().auto_shrink([false, false]);
    #[cfg(windows)]
    {
        // Desktop Windows users expect wheel/scrollbar scrolling in list views,
        // not click-and-drag panning anywhere in the content area.
        scroll_area = scroll_area.scroll_source(egui::containers::scroll_area::ScrollSource {
            drag: false,
            ..Default::default()
        });
    }

    // Scroll to focused row when arrow keys move focus
    if scroll_to_focus && let Some(idx) = focused_idx {
        let target_y = idx as f32 * row_total;
        let viewport_h = ui.available_height();
        scroll_area = scroll_area
            .vertical_scroll_offset((target_y - viewport_h / 2.0 + row_height / 2.0).max(0.0));
    }

    scroll_area.show_rows(ui, row_height, total_rows, |ui, range| {
        // Prevent shift+click from selecting label text (OS text highlight).
        ui.style_mut().interaction.selectable_labels = false;
        let full_width = ui.max_rect();
        for i in range {
            let row = &rows[i];
            let indent = row.depth as f32 * 20.0;
            let bcolor = if row.is_dir {
                bar_color(row.size, ui)
            } else {
                row.category.color()
            };
            let proportion = if row.parent_size > 0 {
                (row.size as f64 / row.parent_size as f64) as f32
            } else {
                1.0
            };
            let is_focused = Some(i) == focused_idx;

            // Placeholder for background fill (painted after we know the row rect)
            let bg_idx = ui.painter().add(egui::Shape::Noop);

            let row_response = ui.horizontal(|ui| {
                ui.set_min_height(row_height);
                // Cap indentation so the disclosure/icon can't slide under the
                // size bar on narrow windows with deeply nested trees.
                let indent = indent.min((ui.available_width() - 260.0).max(0.0));
                ui.add_space(indent);

                // Disclosure toggle (visual only — click handled by row interaction)
                let toggle_right = if row.is_dir || row.is_file_group {
                    paint_disclosure(ui, row.expanded).right()
                } else {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                    rect.right()
                };

                // Icon (file groups have no icon — flush left)
                if row.is_file_group {
                    // No icon, no gap
                } else if let Some(icons) = icon_cache {
                    let tex = if row.is_dir {
                        &icons.folder
                    } else {
                        &icons.file
                    };
                    ui.image(egui::load::SizedTexture::new(
                        tex.id(),
                        egui::vec2(16.0, 16.0),
                    ));
                } else {
                    let icon = if row.is_dir { "\u{1F4C1}" } else { "\u{1F4C4}" };
                    ui.label(icon);
                }

                // Size bar + label dimensions (computed first to reserve space
                // for name truncation).
                let bar_h = 10.0_f32;
                let size_str = ByteSize::b(row.size).to_string();
                let size_text = format!("{:>10}", size_str);
                let font_id =
                    egui::FontId::monospace(ui.style().text_styles[&egui::TextStyle::Body].size);
                let text_galley =
                    ui.painter()
                        .layout_no_wrap(size_text, font_id, ui.visuals().text_color());
                let text_width = text_galley.size().x;
                let right_reserved = TEXT_MARGIN + text_width + BAR_GAP + BAR_WIDTH;

                // Name — truncate so it never overlaps the size bar area.
                // Floor at 0 (not a fixed 20px) so a deeply-indented name clips
                // to nothing rather than spilling over the pulled-in size bar.
                let name_max_w = (ui.available_width() - right_reserved - 4.0).max(0.0);
                // Hard links get a 🔗 prefix (survives right-truncation) and are
                // dimmed, since their storage is shared and counted once.
                let name_text = if row.is_hard_link {
                    egui::RichText::new(format!("\u{1F517} {}", row.name))
                        .monospace()
                        .weak()
                } else if row.is_hidden || row.is_file_group {
                    egui::RichText::new(&*row.name).monospace().weak()
                } else {
                    egui::RichText::new(&*row.name).monospace()
                };
                ui.allocate_ui_with_layout(
                    egui::vec2(name_max_w, row_height),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.add(egui::Label::new(name_text).truncate());
                    },
                );

                // Paint size bar + label at fixed positions anchored to the
                // outer scroll-area rect (`full_width`) for a stable right
                // edge across all rows.  Use min_rect for vertical centering
                // (max_rect extends to the bottom of the scroll area and its
                // center().y drifts with scroll position).
                let row_center_y = ui.min_rect().center().y;
                let painter = ui.painter();
                let text_x = full_width.right() - TEXT_MARGIN - text_width;
                let text_y = row_center_y - text_galley.size().y / 2.0;
                painter.galley(
                    egui::pos2(text_x, text_y),
                    text_galley,
                    ui.visuals().text_color(),
                );

                let bar_x = text_x - BAR_GAP - BAR_WIDTH;
                let bar_y = row_center_y - bar_h / 2.0;
                let bar_rect = egui::Rect::from_min_size(
                    egui::pos2(bar_x, bar_y),
                    egui::vec2(BAR_WIDTH, bar_h),
                );
                painter.rect_filled(bar_rect, 2.0, ui.visuals().extreme_bg_color);
                let fill_w = (BAR_WIDTH * proportion.clamp(0.0, 1.0)).max(1.0);
                let fill_rect = egui::Rect::from_min_size(bar_rect.min, egui::vec2(fill_w, bar_h));
                painter.rect_filled(fill_rect, 2.0, bcolor);

                toggle_right
            });

            let toggle_right = row_response.inner;
            // Match the interaction area to the visible content width so clicks
            // and hover in the blank right-hand gutter don't target the row.
            let row_rect = egui::Rect::from_x_y_ranges(
                full_width.x_range(),
                row_response.response.rect.y_range(),
            );

            // Single row interaction — toggle vs click determined by pointer position
            let row_id = egui::Id::new(("tree_row", row.path.as_os_str()));
            let row_interact = ui.interact(row_rect, row_id, egui::Sense::click());

            // Use PointingHand only when hovering over the disclosure triangle area
            if row_interact.hovered()
                && let Some(pos) = ui.input(|i| i.pointer.hover_pos())
                && (row.is_dir || row.is_file_group)
                && pos.x <= toggle_right
            {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }

            if row_interact.clicked()
                && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
            {
                if row.is_file_group {
                    // Any click on file group row → toggle expand
                    actions.push(TreeAction::ToggleFileGroup(row.path.clone()));
                    actions.push(TreeAction::Focus(row.path.clone()));
                } else if row.is_dir && pos.x <= toggle_right {
                    // Click on disclosure triangle area → toggle expand + focus
                    actions.push(TreeAction::ToggleExpand(row.path.clone()));
                    actions.push(TreeAction::Focus(row.path.clone()));
                } else {
                    // Click on content area → select/focus
                    let (shift, toggle) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
                    actions.push(TreeAction::Click {
                        path: row.path.clone(),
                        shift,
                        toggle,
                    });
                    actions.push(TreeAction::Focus(row.path.clone()));
                }
            }

            // Right-click: select the row before showing context menu
            // (preserve existing multi-selection if right-clicked row is already selected)
            if row_interact.secondary_clicked() {
                let already_selected = selected_paths.contains(&row.path);
                if !already_selected {
                    actions.push(TreeAction::Click {
                        path: row.path.clone(),
                        shift: false,
                        toggle: false,
                    });
                }
                actions.push(TreeAction::Focus(row.path.clone()));
            }

            // Right-click context menu
            let ctx_path = row.path.clone();
            let selection_count = if selected_paths.contains(&row.path) {
                selected_paths.len()
            } else {
                1
            };
            row_interact.context_menu(|ui| {
                if selection_count > 1 {
                    // Multi-select context menu
                    ui.label(
                        egui::RichText::new(format!("{selection_count} items selected"))
                            .weak()
                            .size(12.0),
                    );
                    ui.separator();
                    if ui
                        .button(format!("Move {selection_count} Items to Trash"))
                        .clicked()
                    {
                        actions.push(TreeAction::TrashSelected);
                        ui.close();
                    }
                    if ui
                        .button(
                            egui::RichText::new(format!(
                                "Delete {selection_count} Items Permanently"
                            ))
                            .color(egui::Color32::RED),
                        )
                        .clicked()
                    {
                        actions.push(TreeAction::ConfirmDeleteSelected);
                        ui.close();
                    }
                } else {
                    // Single-item context menu
                    if ui.button(reveal_in_file_manager_label()).clicked() {
                        actions.push(TreeAction::RevealInFinder(ctx_path.clone()));
                        ui.close();
                    }
                    if ui.button("Copy Path").clicked() {
                        actions.push(TreeAction::CopyPath(ctx_path.clone()));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Move to Trash").clicked() {
                        actions.push(TreeAction::Trash(ctx_path.clone()));
                        ui.close();
                    }
                    if ui
                        .button(egui::RichText::new("Delete Permanently").color(egui::Color32::RED))
                        .clicked()
                    {
                        actions.push(TreeAction::ConfirmDelete(ctx_path.clone()));
                        ui.close();
                    }
                }
            });

            // Capture hover state before on_hover_ui consumes row_interact
            let is_hovered = row_interact.hovered();

            // Tooltip reveals the full path (rows show only the leaf name) plus
            // item count for dirs. Size is intentionally omitted — it's already
            // in the Size column, so repeating it here is redundant.
            if row.is_file_group {
                // Synthetic group row: its path ends in an internal marker, so
                // show the containing directory instead. The name ("[N files]")
                // already carries the count. Format lazily inside the closure.
                let path = &row.path;
                row_interact.on_hover_ui(|ui| {
                    let dir = path.parent().unwrap_or(path.as_path()).display();
                    ui.label(dir.to_string());
                });
            } else if row.is_dir {
                let children_count = row.children_count;
                let path = &row.path;
                row_interact.on_hover_ui(|ui| {
                    ui.label(format!("{}\n{} items", path.display(), children_count));
                });
            } else {
                let path = &row.path;
                let hard_link = row.is_hard_link;
                row_interact.on_hover_ui(|ui| {
                    ui.label(path.display().to_string());
                    if hard_link {
                        ui.label(
                            egui::RichText::new(
                                "\u{1F517} Hard link — shares storage with other files. \
                                 Counted once; deleting frees space only once every copy is gone.",
                            )
                            .weak()
                            .size(12.0),
                        );
                    }
                });
            }

            // Row background: selection/focus/hover take priority; otherwise a
            // subtle zebra tint on alternate rows to guide the eye across the
            // gap between name and size (hover/selection override it).
            let is_selected = selected_paths.contains(&row.path);
            let bg_color = if is_selected && is_focused {
                Some(ui.visuals().selection.bg_fill.linear_multiply(0.5))
            } else if is_selected {
                Some(ui.visuals().selection.bg_fill.linear_multiply(0.35))
            } else if is_focused {
                Some(ui.visuals().selection.bg_fill.linear_multiply(0.2))
            } else if is_hovered {
                Some(ui.visuals().widgets.hovered.bg_fill.linear_multiply(0.3))
            } else if i % 2 == 1 {
                Some(ui.visuals().faint_bg_color)
            } else {
                None
            };
            if let Some(bg_color) = bg_color {
                let spacing_half = ui.spacing().item_spacing.y / 2.0;
                let y = row_rect.y_range();
                let bg_rect = egui::Rect::from_x_y_ranges(
                    full_width.x_range(),
                    (y.min - spacing_half)..=(y.max + spacing_half),
                );
                ui.painter()
                    .set(bg_idx, egui::Shape::rect_filled(bg_rect, 0.0, bg_color));
            }
        }
    });

    actions
}

/// Remove a node from the tree by path, returning the removed size so parents can update.
pub fn remove_node(node: &mut FileNode, target: &Path) -> Option<u64> {
    let rest = target.strip_prefix(node.name()).ok()?;
    remove_node_inner(node, rest.components())
}

fn remove_node_inner(node: &mut FileNode, mut rest: std::path::Components) -> Option<u64> {
    let name = rest.next()?.as_os_str().to_str()?;
    let d = node.as_dir_mut()?;
    let pos = d.children.iter().position(|c| c.name() == name)?;
    let removed_size = if rest.clone().next().is_none() {
        d.children.remove(pos).size()
    } else {
        remove_node_inner(&mut d.children[pos], rest)?
    };
    d.size -= removed_size;
    Some(removed_size)
}

/// Resolve a file-group row to the loose-file paths it represents: `dir`'s
/// non-directory children, with hidden files included only when `show_hidden`.
/// Empty if `dir` is not found.
pub fn file_group_files(root: &FileNode, dir: &Path, show_hidden: bool) -> Vec<PathBuf> {
    let Some(node) = root.find(dir) else {
        return Vec::new();
    };
    node.children()
        .iter()
        .filter(|c| !c.is_dir() && (show_hidden || !c.is_hidden()))
        .map(|c| dir.join(c.name()))
        .collect()
}

/// Format a percentage, showing "<1%" rather than a flat "0%" for small non-zero shares.
pub fn fmt_pct(pct: f64) -> String {
    if pct > 0.0 && pct < 1.0 {
        "<1%".to_string()
    } else {
        format!("{pct:.0}%")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{dir, leaf};

    #[test]
    fn node_matches_direct_name() {
        let node = leaf("readme.md", 10);
        assert!(node_matches(&node, "readme"));
        assert!(node_matches(&node, "readme")); // query is pre-lowercased by caller
        assert!(!node_matches(&node, "cargo"));
    }

    #[test]
    fn node_matches_descendant() {
        let tree = dir("root", vec![dir("src", vec![leaf("main.rs", 50)])]);
        assert!(node_matches(&tree, "main"));
        assert!(node_matches(&tree, "src"));
        assert!(!node_matches(&tree, "missing"));
    }

    #[test]
    fn remove_node_direct_child() {
        let mut tree = dir("root", vec![leaf("a.txt", 10), leaf("b.txt", 20)]);
        assert_eq!(tree.size(), 30);

        let removed = remove_node(&mut tree, Path::new("root/a.txt"));
        assert_eq!(removed, Some(10));
        assert_eq!(tree.size(), 20);
        assert_eq!(tree.children().len(), 1);
        assert_eq!(tree.children()[0].name(), "b.txt");
    }

    #[test]
    fn remove_node_nested() {
        let mut tree = dir("root", vec![dir("sub", vec![leaf("deep.txt", 100)])]);
        assert_eq!(tree.size(), 100);

        let removed = remove_node(&mut tree, Path::new("root/sub/deep.txt"));
        assert_eq!(removed, Some(100));
        assert_eq!(tree.size(), 0);
        assert_eq!(tree.children()[0].size(), 0);
        assert!(tree.children()[0].children().is_empty());
    }

    #[test]
    fn remove_node_returns_none_for_missing() {
        let mut tree = dir("root", vec![leaf("a.txt", 10)]);
        assert_eq!(remove_node(&mut tree, Path::new("nope")), None);
        assert_eq!(tree.size(), 10); // unchanged
    }

    #[test]
    fn file_group_files_returns_loose_files() {
        // Group resolves to the loose files only, never the subdir.
        let tree = dir(
            "root",
            vec![
                dir("sub", vec![leaf("deep.txt", 5)]),
                leaf("a.txt", 10),
                leaf("b.txt", 20),
            ],
        );

        let files = file_group_files(&tree, Path::new("root"), true);

        assert_eq!(
            files,
            vec![PathBuf::from("root/a.txt"), PathBuf::from("root/b.txt")]
        );
    }

    #[test]
    fn file_group_files_respects_show_hidden() {
        let tree = dir("root", vec![leaf("a.txt", 10), leaf(".secret", 20)]);

        // Hidden files excluded when show_hidden is false.
        let visible = file_group_files(&tree, Path::new("root"), false);
        assert_eq!(visible, vec![PathBuf::from("root/a.txt")]);

        // Included when show_hidden is true.
        let all = file_group_files(&tree, Path::new("root"), true);
        assert_eq!(
            all,
            vec![PathBuf::from("root/a.txt"), PathBuf::from("root/.secret")]
        );
    }

    #[test]
    fn file_group_files_excludes_os_hidden_non_dotfiles() {
        use crate::tree::{DirNode, FileLeaf};
        // No leading dot, but the OS hidden flag is set.
        let hidden = FileNode::File(FileLeaf::new("hidden.dat".into(), 5, true));
        let tree = FileNode::Dir(Box::new(DirNode {
            name: "root".into(),
            size: 15,
            children: vec![leaf("a.txt", 10), hidden],
            expanded: false,
            hidden: false,
        }));

        assert_eq!(
            file_group_files(&tree, Path::new("root"), false),
            vec![PathBuf::from("root/a.txt")]
        );
        assert_eq!(
            file_group_files(&tree, Path::new("root"), true),
            vec![
                PathBuf::from("root/a.txt"),
                PathBuf::from("root/hidden.dat")
            ]
        );
    }

    #[test]
    fn file_group_files_empty_for_missing_dir() {
        let tree = dir("root", vec![leaf("a.txt", 10)]);
        assert!(file_group_files(&tree, Path::new("root/nope"), true).is_empty());
    }

    #[test]
    fn no_synthetic_group_when_real_file_group_marker_present() {
        // A real __file_group__ file suppresses grouping; all three files
        // render as individual rows.
        let mut tree = dir(
            "root",
            vec![
                leaf("a.txt", 10),
                leaf("b.txt", 20),
                leaf("__file_group__", 5),
            ],
        );
        tree.set_expanded(true);

        let rows = collect_cached_rows(&tree, "", None, true, None, None, None);

        assert!(!rows.iter().any(|r| r.is_file_group));
        // The real __file_group__ file appears once, as a normal file.
        let marker_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.path.as_path() == Path::new("root/__file_group__"))
            .collect();
        assert_eq!(marker_rows.len(), 1);
        assert!(!marker_rows[0].is_file_group);
    }

    #[test]
    fn no_synthetic_group_when_hidden_file_group_marker_present() {
        use crate::tree::{DirNode, FileLeaf};
        // A file named __file_group__ that is OS-hidden (no dot, hidden flag).
        let hidden_marker = FileNode::File(FileLeaf::new(FILE_GROUP_MARKER.into(), 1, true));
        let tree = FileNode::Dir(Box::new(DirNode {
            name: "root".into(),
            size: 31,
            children: vec![leaf("a.txt", 10), leaf("b.txt", 20), hidden_marker],
            expanded: true,
            hidden: false,
        }));

        // Even with show_hidden = false (marker not rendered), grouping must be
        // suppressed: otherwise the synthetic path would collide with the real
        // marker once "Show hidden" is toggled on.
        let rows = collect_cached_rows(&tree, "", None, false, None, None, None);
        assert!(!rows.iter().any(|r| r.is_file_group));
    }

    #[test]
    fn no_synthetic_group_when_dir_named_file_group_marker() {
        // A real subdirectory named __file_group__ shares the synthetic path,
        // so grouping must be suppressed for it too (not just files).
        let mut tree = dir(
            "root",
            vec![
                leaf("a.txt", 10),
                leaf("b.txt", 20),
                dir(FILE_GROUP_MARKER, vec![leaf("inner.txt", 3)]),
            ],
        );
        tree.set_expanded(true);

        let rows = collect_cached_rows(&tree, "", None, true, None, None, None);
        assert!(!rows.iter().any(|r| r.is_file_group));
    }

    #[test]
    fn no_synthetic_group_when_hidden_dir_named_file_group_marker() {
        use crate::tree::DirNode;
        // Hidden subdirectory named __file_group__, show_hidden = false.
        let hidden_dir = FileNode::Dir(Box::new(DirNode {
            name: FILE_GROUP_MARKER.into(),
            size: 3,
            children: vec![leaf("inner.txt", 3)],
            expanded: false,
            hidden: true,
        }));
        let mut tree = dir("root", vec![leaf("a.txt", 10), leaf("b.txt", 20)]);
        if let FileNode::Dir(d) = &mut tree {
            d.children.push(hidden_dir);
        }
        tree.set_expanded(true);

        let rows = collect_cached_rows(&tree, "", None, false, None, None, None);
        assert!(!rows.iter().any(|r| r.is_file_group));
    }

    #[test]
    fn file_group_files_resolves_a_nested_dir() {
        // A group living in a subdirectory resolves against that dir, not root.
        let tree = dir(
            "root",
            vec![dir("sub", vec![leaf("x.txt", 1), leaf("y.txt", 2)])],
        );
        assert_eq!(
            file_group_files(&tree, Path::new("root/sub"), true),
            vec![
                PathBuf::from("root/sub/x.txt"),
                PathBuf::from("root/sub/y.txt")
            ]
        );
    }

    #[test]
    fn file_group_files_empty_when_dir_has_no_loose_files() {
        // Only subdirectories, no loose files → nothing to delete.
        let tree = dir("root", vec![dir("sub", vec![leaf("x.txt", 1)])]);
        assert!(file_group_files(&tree, Path::new("root"), true).is_empty());
    }

    #[test]
    fn file_group_files_includes_a_real_file_named_group_marker() {
        // If a loose file is literally named __file_group__, it is part of the
        // group like any other loose file (path-level disambiguation happens in
        // resolve_batch_targets, not here).
        let tree = dir("root", vec![leaf("a.txt", 10), leaf("__file_group__", 1)]);
        assert_eq!(
            file_group_files(&tree, Path::new("root"), true),
            vec![
                PathBuf::from("root/a.txt"),
                PathBuf::from("root/__file_group__")
            ]
        );
    }

    #[test]
    fn collect_cached_rows_is_deterministic() {
        let mut tree = dir(
            "root",
            vec![
                dir("src", vec![leaf("main.rs", 50), leaf("lib.rs", 30)]),
                leaf("Cargo.toml", 10),
            ],
        );
        // Expand "src" so its children are visible
        tree.as_dir_mut().unwrap().children[0].set_expanded(true);

        let rows_a = collect_cached_rows(&tree, "", None, true, None, None, None);
        let rows_b = collect_cached_rows(&tree, "", None, true, None, None, None);

        assert_eq!(rows_a.len(), rows_b.len());
        for (a, b) in rows_a.iter().zip(rows_b.iter()) {
            assert_eq!(a.path, b.path);
            assert_eq!(&*a.name, &*b.name);
            assert_eq!(a.size, b.size);
            assert_eq!(a.is_dir, b.is_dir);
            assert_eq!(a.expanded, b.expanded);
            assert_eq!(a.depth, b.depth);
            assert_eq!(a.parent_size, b.parent_size);
            assert_eq!(a.children_count, b.children_count);
        }
    }

    #[test]
    fn collect_cached_rows_filters_hidden() {
        let mut tree = dir("root", vec![leaf(".hidden", 5), leaf("visible.txt", 10)]);
        tree.set_expanded(true);

        let rows = collect_cached_rows(&tree, "", None, false, None, None, None);
        // Root + visible.txt (hidden file excluded, 1 file = no grouping)
        assert_eq!(rows.len(), 2);
        assert_eq!(&*rows[1].name, "visible.txt");

        let rows_all = collect_cached_rows(&tree, "", None, true, None, None, None);
        // Root + "2 files" group (both files visible → grouped)
        assert_eq!(rows_all.len(), 2);
        assert!(rows_all[1].is_file_group);
        assert_eq!(&*rows_all[1].name, "[2 files]");
    }

    #[test]
    fn build_text_match_cache_marks_matching_subtrees() {
        let tree = dir(
            "root",
            vec![
                dir("src", vec![leaf("main.rs", 50)]),
                dir("docs", vec![leaf("readme.md", 10)]),
            ],
        );
        let cache = build_text_match_cache(&tree, "main");
        let src = &tree.children()[0];
        let docs = &tree.children()[1];
        assert!(cache.contains(&node_id(&tree))); // has matching descendant
        assert!(cache.contains(&node_id(src))); // has matching descendant
        assert!(cache.contains(&node_id(&src.children()[0]))); // direct match
        assert!(!cache.contains(&node_id(docs))); // no matching descendant
        assert!(!cache.contains(&node_id(&docs.children()[0]))); // no match
    }

    #[test]
    fn build_text_match_cache_visits_all_siblings() {
        // Regression: any() short-circuits, so second matching sibling could be missed.
        let tree = dir(
            "root",
            vec![
                dir("a", vec![leaf("main.rs", 50)]),
                dir("b", vec![leaf("main.py", 30)]),
            ],
        );
        let cache = build_text_match_cache(&tree, "main");
        let a = &tree.children()[0];
        let b = &tree.children()[1];
        assert!(cache.contains(&node_id(a)));
        assert!(cache.contains(&node_id(&a.children()[0])));
        assert!(cache.contains(&node_id(b)));
        assert!(cache.contains(&node_id(&b.children()[0])));
    }

    #[test]
    fn build_category_match_cache_visits_all_siblings() {
        let tree = dir(
            "root",
            vec![
                dir("a", vec![leaf("clip1.mp4", 100)]),
                dir("b", vec![leaf("clip2.mp4", 200)]),
            ],
        );
        let cache = build_category_match_cache(&tree, crate::categories::FileCategory::Video);
        let a = &tree.children()[0];
        let b = &tree.children()[1];
        assert!(cache.contains(&node_id(a)));
        assert!(cache.contains(&node_id(&a.children()[0])));
        assert!(cache.contains(&node_id(b)));
        assert!(cache.contains(&node_id(&b.children()[0])));
    }

    #[test]
    fn build_category_match_cache_marks_matching_subtrees() {
        let tree = dir(
            "root",
            vec![
                dir("media", vec![leaf("movie.mp4", 1000)]),
                dir("src", vec![leaf("main.rs", 50)]),
            ],
        );
        let cache = build_category_match_cache(&tree, crate::categories::FileCategory::Video);
        let media = &tree.children()[0];
        let src = &tree.children()[1];
        assert!(cache.contains(&node_id(&tree))); // has matching descendant
        assert!(cache.contains(&node_id(media))); // has matching descendant
        assert!(cache.contains(&node_id(&media.children()[0]))); // direct match
        assert!(!cache.contains(&node_id(src))); // no matching descendant
        assert!(!cache.contains(&node_id(&src.children()[0]))); // wrong category
    }

    #[test]
    fn cached_rows_with_text_cache_matches_uncached() {
        let tree = dir(
            "root",
            vec![
                dir("src", vec![leaf("main.rs", 50), leaf("lib.rs", 30)]),
                dir("docs", vec![leaf("readme.md", 10)]),
            ],
        );
        let query = "main";
        let cache = build_text_match_cache(&tree, query);

        let rows_uncached = collect_cached_rows(&tree, query, None, true, None, None, None);
        let rows_cached = collect_cached_rows(&tree, query, None, true, Some(&cache), None, None);

        assert_eq!(rows_uncached.len(), rows_cached.len());
        for (a, b) in rows_uncached.iter().zip(rows_cached.iter()) {
            assert_eq!(a.path, b.path);
            assert_eq!(&*a.name, &*b.name);
        }
    }

    #[test]
    fn cached_rows_with_cat_cache_matches_uncached() {
        let tree = dir(
            "root",
            vec![
                dir("media", vec![leaf("movie.mp4", 1000)]),
                dir("src", vec![leaf("main.rs", 50)]),
            ],
        );
        let cat = crate::categories::FileCategory::Video;
        let cache = build_category_match_cache(&tree, cat);

        let rows_uncached = collect_cached_rows(&tree, "", Some(cat), true, None, None, None);
        let rows_cached = collect_cached_rows(&tree, "", Some(cat), true, None, Some(&cache), None);

        assert_eq!(rows_uncached.len(), rows_cached.len());
        for (a, b) in rows_uncached.iter().zip(rows_cached.iter()) {
            assert_eq!(a.path, b.path);
            assert_eq!(&*a.name, &*b.name);
        }
    }
}
