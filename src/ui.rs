use std::collections::HashSet;
use std::path::{Path, PathBuf};

use bytesize::ByteSize;
use eframe::egui;

use crate::icons::IconCache;
use crate::tree::{FileTree, NodeId};

/// Paint a disclosure triangle (> or v). Visual only — click detection is
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
pub fn node_matches(tree: &FileTree, id: NodeId, query: &str) -> bool {
    contains_case_insensitive(tree.name(id), query)
        || tree
            .children(id)
            .iter()
            .any(|&c| node_matches(tree, c, query))
}

/// Pre-compute which subtrees contain nodes matching the text query.
/// Returns a set of paths that match or have matching descendants.
pub fn build_text_match_cache(tree: &FileTree, query: &str) -> HashSet<PathBuf> {
    let mut cache = HashSet::new();
    let root = tree.root();
    let mut buf = PathBuf::from(tree.name(root));
    build_text_match_inner(tree, root, query, &mut buf, &mut cache);
    cache
}

fn build_text_match_inner(
    tree: &FileTree,
    id: NodeId,
    query: &str,
    buf: &mut PathBuf,
    cache: &mut HashSet<PathBuf>,
) -> bool {
    let self_matches = contains_case_insensitive(tree.name(id), query);
    // Must visit ALL children (not short-circuit) so every matching subtree is cached.
    let child_matches = tree.children(id).to_vec().iter().fold(false, |acc, &c| {
        buf.push(tree.name(c));
        let m = build_text_match_inner(tree, c, query, buf, cache);
        buf.pop();
        acc || m
    });
    if self_matches || child_matches {
        cache.insert(buf.clone());
        true
    } else {
        false
    }
}

/// Pre-compute which subtrees contain nodes matching the given category.
/// Returns a set of paths that match or have matching descendants.
pub fn build_category_match_cache(
    tree: &FileTree,
    cat: crate::categories::FileCategory,
) -> HashSet<PathBuf> {
    let mut cache = HashSet::new();
    let root = tree.root();
    let mut buf = PathBuf::from(tree.name(root));
    build_cat_match_inner(tree, root, cat, &mut buf, &mut cache);
    cache
}

fn build_cat_match_inner(
    tree: &FileTree,
    id: NodeId,
    cat: crate::categories::FileCategory,
    buf: &mut PathBuf,
    cache: &mut HashSet<PathBuf>,
) -> bool {
    let self_matches = if tree.is_dir(id) {
        false
    } else {
        crate::categories::categorize(tree.name(id)) == cat
    };
    // Must visit ALL children (not short-circuit) so every matching subtree is cached.
    let child_matches = tree.children(id).to_vec().iter().fold(false, |acc, &c| {
        buf.push(tree.name(c));
        let m = build_cat_match_inner(tree, c, cat, buf, cache);
        buf.pop();
        acc || m
    });
    if self_matches || child_matches {
        cache.insert(buf.clone());
        true
    } else {
        false
    }
}

/// ASCII case-insensitive substring search without allocating.
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

/// Minimum number of loose files in a folder to trigger grouping.
const FILE_GROUP_THRESHOLD: usize = 2;

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
}

/// Collect all visible rows into owned `CachedRow` structs.
pub fn collect_cached_rows(
    tree: &FileTree,
    filter: &str,
    category_filter: Option<crate::categories::FileCategory>,
    show_hidden: bool,
    text_cache: Option<&HashSet<PathBuf>>,
    cat_cache: Option<&HashSet<PathBuf>>,
    expanded_file_groups: Option<&HashSet<PathBuf>>,
) -> Vec<CachedRow> {
    let mut result = Vec::new();
    let root = tree.root();
    let mut path_buf = PathBuf::from(tree.name(root));
    collect_cached_rows_inner(
        tree,
        root,
        0,
        tree.size(root),
        &mut path_buf,
        filter,
        category_filter,
        show_hidden,
        text_cache,
        cat_cache,
        expanded_file_groups,
        &mut result,
    );
    result
}

#[allow(clippy::too_many_arguments)]
fn emit_file_group(
    tree: &FileTree,
    result: &mut Vec<CachedRow>,
    current_path: &mut PathBuf,
    file_count: usize,
    file_size: u64,
    group_expanded: bool,
    depth: usize,
    parent_size: u64,
    files: &[NodeId],
    filter: &str,
    category_filter: Option<crate::categories::FileCategory>,
    show_hidden: bool,
    text_cache: Option<&HashSet<PathBuf>>,
    cat_cache: Option<&HashSet<PathBuf>>,
    expanded_file_groups: Option<&HashSet<PathBuf>>,
) {
    result.push(CachedRow {
        path: current_path.join("__file_group__"),
        name: format!("[{file_count} files]").into(),
        size: file_size,
        is_dir: false,
        expanded: group_expanded,
        depth: depth + 1,
        parent_size,
        children_count: file_count,
        category: crate::categories::FileCategory::Other,
        is_hidden: false,
        is_file_group: true,
    });

    if group_expanded {
        for &child in files {
            current_path.push(tree.name(child));
            collect_cached_rows_inner(
                tree,
                child,
                depth + 2,
                file_size,
                current_path,
                filter,
                category_filter,
                show_hidden,
                text_cache,
                cat_cache,
                expanded_file_groups,
                result,
            );
            current_path.pop();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_cached_rows_inner(
    tree: &FileTree,
    id: NodeId,
    depth: usize,
    parent_size: u64,
    current_path: &mut PathBuf,
    filter: &str,
    category_filter: Option<crate::categories::FileCategory>,
    show_hidden: bool,
    text_cache: Option<&HashSet<PathBuf>>,
    cat_cache: Option<&HashSet<PathBuf>>,
    expanded_file_groups: Option<&HashSet<PathBuf>>,
    result: &mut Vec<CachedRow>,
) {
    if !show_hidden && tree.is_hidden(id) {
        return;
    }
    if let Some(tc) = text_cache {
        if !tc.contains(current_path.as_path()) {
            return;
        }
    } else if !filter.is_empty() && !node_matches(tree, id, filter) {
        return;
    }
    if let Some(cc) = cat_cache {
        if !cc.contains(current_path.as_path()) {
            return;
        }
    } else if let Some(cat) = category_filter {
        if !crate::categories::node_matches_category(tree, id, cat) {
            return;
        }
    }

    result.push(CachedRow {
        path: current_path.clone(),
        name: tree.name(id).into(),
        size: tree.size(id),
        is_dir: tree.is_dir(id),
        expanded: tree.expanded(id),
        depth,
        parent_size,
        children_count: tree.children_count(id),
        category: if tree.is_dir(id) {
            crate::categories::FileCategory::Other
        } else {
            crate::categories::categorize(tree.name(id))
        },
        is_hidden: tree.is_hidden(id),
        is_file_group: false,
    });

    let show_children = tree.is_dir(id) && (tree.expanded(id) || !filter.is_empty());
    if show_children {
        let children: Vec<NodeId> = tree.children(id).to_vec();
        let dirs: Vec<NodeId> = children.iter().copied().filter(|&c| tree.is_dir(c)).collect();
        let files: Vec<NodeId> = children
            .iter()
            .copied()
            .filter(|&c| !tree.is_dir(c) && (show_hidden || !tree.name(c).starts_with('.')))
            .collect();
        let should_group_files = files.len() >= FILE_GROUP_THRESHOLD
            && filter.is_empty()
            && category_filter.is_none();

        if should_group_files {
            let file_size: u64 = files.iter().map(|&f| tree.size(f)).sum();
            let group_expanded = expanded_file_groups
                .is_some_and(|s| s.contains(current_path.as_path()));
            let file_count = files.len();
            let mut file_group_emitted = false;

            for &child in &dirs {
                if !file_group_emitted && tree.size(child) < file_size {
                    emit_file_group(
                        tree,
                        result,
                        current_path,
                        file_count,
                        file_size,
                        group_expanded,
                        depth,
                        tree.size(id),
                        &files,
                        filter,
                        category_filter,
                        show_hidden,
                        text_cache,
                        cat_cache,
                        expanded_file_groups,
                    );
                    file_group_emitted = true;
                }
                current_path.push(tree.name(child));
                collect_cached_rows_inner(
                    tree,
                    child,
                    depth + 1,
                    tree.size(id),
                    current_path,
                    filter,
                    category_filter,
                    show_hidden,
                    text_cache,
                    cat_cache,
                    expanded_file_groups,
                    result,
                );
                current_path.pop();
            }
            if !file_group_emitted {
                emit_file_group(
                    tree,
                    result,
                    current_path,
                    file_count,
                    file_size,
                    group_expanded,
                    depth,
                    tree.size(id),
                    &files,
                    filter,
                    category_filter,
                    show_hidden,
                    text_cache,
                    cat_cache,
                    expanded_file_groups,
                );
            }
        } else {
            for &child in &children {
                if !show_hidden && tree.name(child).starts_with('.') {
                    continue;
                }
                current_path.push(tree.name(child));
                collect_cached_rows_inner(
                    tree,
                    child,
                    depth + 1,
                    tree.size(id),
                    current_path,
                    filter,
                    category_filter,
                    show_hidden,
                    text_cache,
                    cat_cache,
                    expanded_file_groups,
                    result,
                );
                current_path.pop();
            }
        }
    }
}

/// Render the tree view with virtualized scrolling. Returns actions to apply.
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

    let mut scroll_area = egui::ScrollArea::vertical().auto_shrink([false, false]);

    if scroll_to_focus {
        if let Some(idx) = focused_idx {
            let target_y = idx as f32 * row_total;
            let viewport_h = ui.available_height();
            scroll_area = scroll_area
                .vertical_scroll_offset((target_y - viewport_h / 2.0 + row_height / 2.0).max(0.0));
        }
    }

    scroll_area.show_rows(ui, row_height, total_rows, |ui, range| {
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

            let bg_idx = ui.painter().add(egui::Shape::Noop);

            let row_response = ui.horizontal(|ui| {
                ui.set_min_height(row_height);
                ui.add_space(indent);

                let toggle_right = if row.is_dir || row.is_file_group {
                    paint_disclosure(ui, row.expanded).right()
                } else {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                    rect.right()
                };

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

                let bar_width = 80.0_f32;
                let bar_h = 10.0_f32;
                let text_margin = 8.0_f32;
                let bar_gap = 4.0_f32;

                let size_str = ByteSize::b(row.size).to_string();
                let size_text = format!("{:>10}", size_str);
                let font_id =
                    egui::FontId::monospace(ui.style().text_styles[&egui::TextStyle::Body].size);
                let text_galley = ui.painter().layout_no_wrap(
                    size_text,
                    font_id,
                    ui.visuals().text_color(),
                );
                let text_width = text_galley.size().x;
                let right_reserved = text_margin + text_width + bar_gap + bar_width;

                let name_max_w =
                    (ui.available_width() - right_reserved - 4.0).max(20.0);
                let name_text = if row.is_hidden || row.is_file_group {
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

                let row_center_y = ui.min_rect().center().y;
                let painter = ui.painter();
                let text_x = full_width.right() - text_margin - text_width;
                let text_y = row_center_y - text_galley.size().y / 2.0;
                painter.galley(
                    egui::pos2(text_x, text_y),
                    text_galley,
                    ui.visuals().text_color(),
                );

                let bar_x = text_x - bar_gap - bar_width;
                let bar_y = row_center_y - bar_h / 2.0;
                let bar_rect = egui::Rect::from_min_size(
                    egui::pos2(bar_x, bar_y),
                    egui::vec2(bar_width, bar_h),
                );
                painter.rect_filled(bar_rect, 2.0, ui.visuals().extreme_bg_color);
                let fill_w = (bar_width * proportion.clamp(0.0, 1.0)).max(1.0);
                let fill_rect =
                    egui::Rect::from_min_size(bar_rect.min, egui::vec2(fill_w, bar_h));
                painter.rect_filled(fill_rect, 2.0, bcolor);

                toggle_right
            });

            let toggle_right = row_response.inner;
            let row_rect = egui::Rect::from_x_y_ranges(
                full_width.x_range(),
                row_response.response.rect.y_range(),
            );

            let row_id = egui::Id::new(("tree_row", row.path.as_os_str()));
            let row_interact = ui.interact(row_rect, row_id, egui::Sense::click());

            if row_interact.hovered() {
                if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                    if (row.is_dir || row.is_file_group) && pos.x <= toggle_right {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                }
            }

            if row_interact.clicked() {
                if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                    if row.is_file_group {
                        actions.push(TreeAction::ToggleFileGroup(row.path.clone()));
                        actions.push(TreeAction::Focus(row.path.clone()));
                    } else if row.is_dir && pos.x <= toggle_right {
                        actions.push(TreeAction::ToggleExpand(row.path.clone()));
                        actions.push(TreeAction::Focus(row.path.clone()));
                    } else {
                        let (shift, toggle) =
                            ui.input(|i| (i.modifiers.shift, i.modifiers.command));
                        actions.push(TreeAction::Click {
                            path: row.path.clone(),
                            shift,
                            toggle,
                        });
                        actions.push(TreeAction::Focus(row.path.clone()));
                    }
                }
            }

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

            let ctx_path = row.path.clone();
            let selection_count = if selected_paths.contains(&row.path) {
                selected_paths.len()
            } else {
                1
            };
            row_interact.context_menu(|ui| {
                if selection_count > 1 {
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
                    if ui.button("Open in Finder").clicked() {
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

            let is_hovered = row_interact.hovered();

            if row.is_dir {
                let children_count = row.children_count;
                let size = row.size;
                let path = &row.path;
                row_interact.on_hover_ui(|ui| {
                    ui.label(format!(
                        "{}\n{} \u{2014} {} items",
                        path.display(),
                        ByteSize::b(size),
                        children_count
                    ));
                });
            } else {
                let size = row.size;
                let path = &row.path;
                row_interact.on_hover_ui(|ui| {
                    ui.label(format!("{}\n{}", path.display(), ByteSize::b(size)));
                });
            }

            let is_selected = selected_paths.contains(&row.path);
            if is_selected || is_focused || is_hovered {
                let bg_color = if is_selected && is_focused {
                    ui.visuals().selection.bg_fill.linear_multiply(0.5)
                } else if is_selected {
                    ui.visuals().selection.bg_fill.linear_multiply(0.35)
                } else if is_focused {
                    ui.visuals().selection.bg_fill.linear_multiply(0.2)
                } else {
                    ui.visuals().widgets.hovered.bg_fill.linear_multiply(0.3)
                };
                let spacing_half = ui.spacing().item_spacing.y / 2.0;
                let y = row_rect.y_range();
                let highlight_rect = egui::Rect::from_x_y_ranges(
                    full_width.x_range(),
                    (y.min - spacing_half)..=(y.max + spacing_half),
                );
                ui.painter().set(
                    bg_idx,
                    egui::Shape::rect_filled(highlight_rect, 0.0, bg_color),
                );
            }
        }
    });

    actions
}

// ─── Tree navigation / mutation helpers ────────────────────────

/// Get the next path component name to navigate toward when searching for `target`
/// from the current position `buf`.
fn next_component_name<'a>(target: &'a Path, buf: &Path) -> Option<&'a str> {
    target
        .strip_prefix(buf)
        .ok()
        .and_then(|remaining| remaining.components().next())
        .and_then(|c| c.as_os_str().to_str())
}

/// Toggle expand/collapse for the node at `target`. Returns true if found.
pub fn toggle_expand(tree: &mut FileTree, target: &Path) -> bool {
    let root = tree.root();
    let mut buf = PathBuf::from(tree.name(root));
    toggle_expand_inner(tree, root, target, &mut buf)
}

fn toggle_expand_inner(
    tree: &mut FileTree,
    id: NodeId,
    target: &Path,
    buf: &mut PathBuf,
) -> bool {
    if buf.as_path() == target {
        let new_val = !tree.expanded(id);
        tree.set_expanded(id, new_val);
        return true;
    }
    if tree.is_dir(id) {
        if let Some(next) = next_component_name(target, buf) {
            let children: Vec<NodeId> = tree.children(id).to_vec();
            for child in children {
                if tree.name(child) == next {
                    buf.push(tree.name(child));
                    let found = toggle_expand_inner(tree, child, target, buf);
                    buf.pop();
                    return found;
                }
            }
        }
    }
    false
}

/// Remove a node from the tree by path, returning the removed size so parents can update.
pub fn remove_node(tree: &mut FileTree, target: &Path) -> Option<u64> {
    let root = tree.root();
    let mut buf = PathBuf::from(tree.name(root));
    remove_node_inner(tree, root, target, &mut buf)
}

fn remove_node_inner(
    tree: &mut FileTree,
    id: NodeId,
    target: &Path,
    buf: &mut PathBuf,
) -> Option<u64> {
    if !tree.is_dir(id) {
        return None;
    }

    if let Some(next) = next_component_name(target, buf) {
        // Check if a direct child matches the full target
        let children: Vec<NodeId> = tree.children(id).to_vec();
        let found_pos = children.iter().enumerate().find_map(|(i, &c)| {
            if tree.name(c) == next && buf.join(tree.name(c)) == target {
                Some(i)
            } else {
                None
            }
        });

        if let Some(pos) = found_pos {
            let removed_size = tree.remove_child(id, pos);
            return Some(removed_size);
        }

        // Navigate to the matching child directory
        for &child in &children {
            if tree.is_dir(child) && tree.name(child) == next {
                buf.push(tree.name(child));
                if let Some(removed_size) = remove_node_inner(tree, child, target, buf) {
                    buf.pop();
                    // Update this node's size too
                    tree.sub_size(id, removed_size);
                    return Some(removed_size);
                }
                buf.pop();
                return None;
            }
        }
    }

    None
}

/// Find the parent path of a node in the tree.
pub fn find_parent_path(tree: &FileTree, target: &Path) -> Option<PathBuf> {
    let root = tree.root();
    let mut buf = PathBuf::from(tree.name(root));
    find_parent_path_inner(tree, root, target, &mut buf)
}

fn find_parent_path_inner(
    tree: &FileTree,
    id: NodeId,
    target: &Path,
    buf: &mut PathBuf,
) -> Option<PathBuf> {
    if let Some(next) = next_component_name(target, buf) {
        for &child in tree.children(id) {
            if tree.name(child) == next {
                let child_path = buf.join(tree.name(child));
                if child_path == target {
                    return Some(buf.clone());
                }
                if tree.is_dir(child) {
                    buf.push(tree.name(child));
                    let result = find_parent_path_inner(tree, child, target, buf);
                    buf.pop();
                    return result;
                }
            }
        }
    }
    None
}

/// Find a node by path and return (is_dir, expanded, has_children).
pub fn find_node_info(tree: &FileTree, target: &Path) -> Option<(bool, bool, bool)> {
    let root = tree.root();
    let mut buf = PathBuf::from(tree.name(root));
    find_node_info_inner(tree, root, target, &mut buf)
}

fn find_node_info_inner(
    tree: &FileTree,
    id: NodeId,
    target: &Path,
    buf: &mut PathBuf,
) -> Option<(bool, bool, bool)> {
    if buf.as_path() == target {
        return Some((
            tree.is_dir(id),
            tree.expanded(id),
            tree.children_count(id) > 0,
        ));
    }
    if let Some(next) = next_component_name(target, buf) {
        for &child in tree.children(id) {
            if tree.name(child) == next {
                buf.push(tree.name(child));
                let result = find_node_info_inner(tree, child, target, buf);
                buf.pop();
                return result;
            }
        }
    }
    None
}

/// Set expanded state for a node at target path. Returns true if found.
pub fn set_expanded(tree: &mut FileTree, target: &Path, expanded: bool) -> bool {
    let root = tree.root();
    let mut buf = PathBuf::from(tree.name(root));
    set_expanded_inner(tree, root, target, expanded, &mut buf)
}

fn set_expanded_inner(
    tree: &mut FileTree,
    id: NodeId,
    target: &Path,
    expanded: bool,
    buf: &mut PathBuf,
) -> bool {
    if buf.as_path() == target {
        tree.set_expanded(id, expanded);
        return true;
    }
    if tree.is_dir(id) {
        if let Some(next) = next_component_name(target, buf) {
            let children: Vec<NodeId> = tree.children(id).to_vec();
            for child in children {
                if tree.name(child) == next {
                    buf.push(tree.name(child));
                    let found = set_expanded_inner(tree, child, target, expanded, buf);
                    buf.pop();
                    return found;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{build_test_tree, dir, leaf};

    #[test]
    fn node_matches_direct_name() {
        let tree = build_test_tree(leaf("readme.md", 10));
        assert!(node_matches(&tree, tree.root(), "readme"));
        assert!(!node_matches(&tree, tree.root(), "cargo"));
    }

    #[test]
    fn node_matches_descendant() {
        let tree = build_test_tree(dir("root", vec![dir("src", vec![leaf("main.rs", 50)])]));
        let root = tree.root();
        assert!(node_matches(&tree, root, "main"));
        assert!(node_matches(&tree, root, "src"));
        assert!(!node_matches(&tree, root, "missing"));
    }

    #[test]
    fn toggle_expand_flips_target() {
        let mut tree = build_test_tree(dir("root", vec![dir("sub", vec![leaf("f.txt", 1)])]));
        let sub = tree.children(tree.root())[0];
        assert!(!tree.expanded(sub));

        toggle_expand(&mut tree, Path::new("root/sub"));
        assert!(tree.expanded(sub));

        toggle_expand(&mut tree, Path::new("root/sub"));
        assert!(!tree.expanded(sub));
    }

    #[test]
    fn toggle_expand_returns_false_for_missing() {
        let mut tree = build_test_tree(dir("root", vec![]));
        assert!(!toggle_expand(&mut tree, Path::new("nope")));
    }

    #[test]
    fn remove_node_direct_child() {
        let mut tree = build_test_tree(dir("root", vec![leaf("a.txt", 10), leaf("b.txt", 20)]));
        let root = tree.root();
        assert_eq!(tree.size(root), 30);

        let removed = remove_node(&mut tree, Path::new("root/a.txt"));
        assert_eq!(removed, Some(10));
        assert_eq!(tree.size(root), 20);
        assert_eq!(tree.children_count(root), 1);
    }

    #[test]
    fn remove_node_nested() {
        let mut tree =
            build_test_tree(dir("root", vec![dir("sub", vec![leaf("deep.txt", 100)])]));
        let root = tree.root();
        assert_eq!(tree.size(root), 100);

        let removed = remove_node(&mut tree, Path::new("root/sub/deep.txt"));
        assert_eq!(removed, Some(100));
        assert_eq!(tree.size(root), 0);
        let sub = tree.children(root)[0];
        assert_eq!(tree.size(sub), 0);
        assert!(tree.children(sub).is_empty());
    }

    #[test]
    fn remove_node_returns_none_for_missing() {
        let mut tree = build_test_tree(dir("root", vec![leaf("a.txt", 10)]));
        assert_eq!(remove_node(&mut tree, Path::new("nope")), None);
        assert_eq!(tree.size(tree.root()), 10);
    }

    #[test]
    fn collect_cached_rows_is_deterministic() {
        let mut tree = build_test_tree(dir(
            "root",
            vec![
                dir("src", vec![leaf("main.rs", 50), leaf("lib.rs", 30)]),
                leaf("Cargo.toml", 10),
            ],
        ));
        let src = tree.children(tree.root())[0];
        tree.set_expanded(src, true);

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
        let mut tree = build_test_tree(dir("root", vec![leaf(".hidden", 5), leaf("visible.txt", 10)]));
        tree.set_expanded(tree.root(), true);

        let rows = collect_cached_rows(&tree, "", None, false, None, None, None);
        // Root + visible.txt (hidden file excluded, 1 file = no grouping)
        assert_eq!(rows.len(), 2);
        assert_eq!(&*rows[1].name, "visible.txt");

        let rows_all = collect_cached_rows(&tree, "", None, true, None, None, None);
        // Root + "2 files" group (both files visible -> grouped)
        assert_eq!(rows_all.len(), 2);
        assert!(rows_all[1].is_file_group);
        assert_eq!(&*rows_all[1].name, "[2 files]");
    }

    #[test]
    fn build_text_match_cache_marks_matching_subtrees() {
        let tree = build_test_tree(dir(
            "root",
            vec![
                dir("src", vec![leaf("main.rs", 50)]),
                dir("docs", vec![leaf("readme.md", 10)]),
            ],
        ));
        let cache = build_text_match_cache(&tree, "main");
        assert!(cache.contains(&PathBuf::from("root")));
        assert!(cache.contains(&PathBuf::from("root/src")));
        assert!(cache.contains(&PathBuf::from("root/src/main.rs")));
        assert!(!cache.contains(&PathBuf::from("root/docs")));
        assert!(!cache.contains(&PathBuf::from("root/docs/readme.md")));
    }

    #[test]
    fn build_text_match_cache_visits_all_siblings() {
        let tree = build_test_tree(dir(
            "root",
            vec![
                dir("a", vec![leaf("main.rs", 50)]),
                dir("b", vec![leaf("main.py", 30)]),
            ],
        ));
        let cache = build_text_match_cache(&tree, "main");
        assert!(cache.contains(&PathBuf::from("root/a")));
        assert!(cache.contains(&PathBuf::from("root/a/main.rs")));
        assert!(cache.contains(&PathBuf::from("root/b")));
        assert!(cache.contains(&PathBuf::from("root/b/main.py")));
    }

    #[test]
    fn build_category_match_cache_visits_all_siblings() {
        let tree = build_test_tree(dir(
            "root",
            vec![
                dir("a", vec![leaf("clip1.mp4", 100)]),
                dir("b", vec![leaf("clip2.mp4", 200)]),
            ],
        ));
        let cache = build_category_match_cache(&tree, crate::categories::FileCategory::Video);
        assert!(cache.contains(&PathBuf::from("root/a")));
        assert!(cache.contains(&PathBuf::from("root/a/clip1.mp4")));
        assert!(cache.contains(&PathBuf::from("root/b")));
        assert!(cache.contains(&PathBuf::from("root/b/clip2.mp4")));
    }

    #[test]
    fn build_category_match_cache_marks_matching_subtrees() {
        let tree = build_test_tree(dir(
            "root",
            vec![
                dir("media", vec![leaf("movie.mp4", 1000)]),
                dir("src", vec![leaf("main.rs", 50)]),
            ],
        ));
        let cache = build_category_match_cache(&tree, crate::categories::FileCategory::Video);
        assert!(cache.contains(&PathBuf::from("root")));
        assert!(cache.contains(&PathBuf::from("root/media")));
        assert!(cache.contains(&PathBuf::from("root/media/movie.mp4")));
        assert!(!cache.contains(&PathBuf::from("root/src")));
        assert!(!cache.contains(&PathBuf::from("root/src/main.rs")));
    }

    #[test]
    fn cached_rows_with_text_cache_matches_uncached() {
        let tree = build_test_tree(dir(
            "root",
            vec![
                dir("src", vec![leaf("main.rs", 50), leaf("lib.rs", 30)]),
                dir("docs", vec![leaf("readme.md", 10)]),
            ],
        ));
        let query = "main";
        let cache = build_text_match_cache(&tree, query);

        let rows_uncached = collect_cached_rows(&tree, query, None, true, None, None, None);
        let rows_cached = collect_cached_rows(&tree, query, None, true, Some(&cache), None, None);

        assert_eq!(rows_uncached.len(), rows_cached.len());
        for (a, b) in rows_uncached.iter().zip(rows_cached.iter()) {
            assert_eq!(a.path, b.path);
        }
    }
}
