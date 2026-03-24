use std::collections::HashSet;
use std::path::{Path, PathBuf};

use bytesize::ByteSize;
use eframe::egui;

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

/// Case-insensitive substring search without allocating.
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
}

/// Collect all visible rows into owned `CachedRow` structs. This replaces both
/// `collect_visible_rows` (rendering data) and `collect_visible_paths` (keyboard
/// nav), producing a single flat list that can be cached across frames.
pub fn collect_cached_rows(
    node: &FileNode,
    filter: &str,
    category_filter: Option<crate::categories::FileCategory>,
    show_hidden: bool,
) -> Vec<CachedRow> {
    let mut result = Vec::new();
    let mut path_buf = PathBuf::from(node.name());
    collect_cached_rows_inner(
        node,
        0,
        node.size(),
        &mut path_buf,
        filter,
        category_filter,
        show_hidden,
        &mut result,
    );
    result
}

#[allow(clippy::too_many_arguments)]
fn collect_cached_rows_inner(
    node: &FileNode,
    depth: usize,
    parent_size: u64,
    current_path: &mut PathBuf,
    filter: &str,
    category_filter: Option<crate::categories::FileCategory>,
    show_hidden: bool,
    result: &mut Vec<CachedRow>,
) {
    if !show_hidden && node.name().starts_with('.') {
        return;
    }
    if !filter.is_empty() && !node_matches(node, filter) {
        return;
    }
    if let Some(cat) = category_filter {
        if !crate::categories::node_matches_category(node, cat) {
            return;
        }
    }

    result.push(CachedRow {
        path: current_path.clone(),
        name: node.name().into(),
        size: node.size(),
        is_dir: node.is_dir(),
        expanded: node.expanded(),
        depth,
        parent_size,
        children_count: node.children().len(),
        category: if node.is_dir() {
            crate::categories::FileCategory::Other
        } else {
            crate::categories::categorize(node.name())
        },
    });

    let show_children = node.is_dir() && (node.expanded() || !filter.is_empty());
    if show_children {
        for child in node.children() {
            current_path.push(child.name());
            collect_cached_rows_inner(
                child,
                depth + 1,
                node.size(),
                current_path,
                filter,
                category_filter,
                show_hidden,
                result,
            );
            current_path.pop();
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

    let mut scroll_area = egui::ScrollArea::vertical().auto_shrink([false, false]);

    // Scroll to focused row when arrow keys move focus
    if scroll_to_focus {
        if let Some(idx) = focused_idx {
            let target_y = idx as f32 * row_total;
            let viewport_h = ui.available_height();
            scroll_area = scroll_area
                .vertical_scroll_offset((target_y - viewport_h / 2.0 + row_height / 2.0).max(0.0));
        }
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
                ui.add_space(indent);

                // Disclosure toggle (visual only — click handled by row interaction)
                let toggle_right = if row.is_dir {
                    paint_disclosure(ui, row.expanded).right()
                } else {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                    rect.right()
                };

                // Icon
                if let Some(icons) = icon_cache {
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

                // Name
                ui.label(egui::RichText::new(&*row.name).monospace());

                // Size bar + label (painted at fixed right-edge positions for alignment)
                let row_max = ui.max_rect();
                let painter = ui.painter();
                let bar_width = 80.0_f32;
                let bar_h = 10.0_f32;
                let text_margin = 8.0_f32;
                let size_str = ByteSize::b(row.size).to_string();
                let size_text = format!("{:>10}", size_str);
                let font_id =
                    egui::FontId::monospace(ui.style().text_styles[&egui::TextStyle::Body].size);
                let text_galley =
                    painter.layout_no_wrap(size_text, font_id, ui.visuals().text_color());
                let text_width = text_galley.size().x;
                let text_x = row_max.right() - text_margin - text_width;
                let text_y = row_max.center().y - text_galley.size().y / 2.0;
                painter.galley(
                    egui::pos2(text_x, text_y),
                    text_galley,
                    ui.visuals().text_color(),
                );

                let bar_gap = 4.0_f32;
                let bar_x = text_x - bar_gap - bar_width;
                let bar_y = row_max.center().y - bar_h / 2.0;
                let bar_rect = egui::Rect::from_min_size(
                    egui::pos2(bar_x, bar_y),
                    egui::vec2(bar_width, bar_h),
                );
                painter.rect_filled(bar_rect, 2.0, ui.visuals().extreme_bg_color);
                let fill_w = (bar_width * proportion.clamp(0.0, 1.0)).max(1.0);
                let fill_rect = egui::Rect::from_min_size(bar_rect.min, egui::vec2(fill_w, bar_h));
                painter.rect_filled(fill_rect, 2.0, bcolor);

                toggle_right
            });

            let toggle_right = row_response.inner;
            let row_rect = egui::Rect::from_x_y_ranges(
                full_width.x_range(),
                row_response.response.rect.y_range(),
            );

            // Single row interaction — toggle vs click determined by pointer position
            let row_id = egui::Id::new(("tree_row", row.path.as_os_str()));
            let row_interact = ui.interact(row_rect, row_id, egui::Sense::click());

            // Use PointingHand only when hovering over the disclosure triangle area
            if row_interact.hovered() {
                if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                    if row.is_dir && pos.x <= toggle_right {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                }
            }

            if row_interact.clicked() {
                if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                    if row.is_dir && pos.x <= toggle_right {
                        // Click on disclosure triangle area → toggle expand
                        actions.push(TreeAction::ToggleExpand(row.path.clone()));
                    } else {
                        // Click on content area → select/focus
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

            // Capture hover state before on_hover_ui consumes row_interact
            let is_hovered = row_interact.hovered();

            // Tooltip (lazy via closure — avoids format! allocation unless hovered)
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

            // Focus/selection/hover background
            let is_selected = selected_paths.contains(&row.path);
            if is_selected || is_focused || is_hovered {
                let bg_color = if is_selected && is_focused {
                    // Selected + focused: strongest highlight
                    ui.visuals().selection.bg_fill.linear_multiply(0.5)
                } else if is_selected {
                    ui.visuals().selection.bg_fill.linear_multiply(0.35)
                } else if is_focused {
                    ui.visuals().selection.bg_fill.linear_multiply(0.2)
                } else {
                    // Hover only
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

/// Toggle expand/collapse for the node at `target`. Returns true if found.
pub fn toggle_expand(node: &mut FileNode, target: &Path) -> bool {
    let mut buf = PathBuf::from(node.name());
    toggle_expand_inner(node, target, &mut buf)
}

fn toggle_expand_inner(node: &mut FileNode, target: &Path, buf: &mut PathBuf) -> bool {
    if buf.as_path() == target {
        let new_val = !node.expanded();
        node.set_expanded(new_val);
        return true;
    }
    if let FileNode::Dir(d) = node {
        for child in &mut d.children {
            buf.push(child.name());
            if toggle_expand_inner(child, target, buf) {
                buf.pop();
                return true;
            }
            buf.pop();
        }
    }
    false
}

/// Remove a node from the tree by path, returning the removed size so parents can update.
pub fn remove_node(node: &mut FileNode, target: &Path) -> Option<u64> {
    let mut buf = PathBuf::from(node.name());
    remove_node_inner(node, target, &mut buf)
}

fn remove_node_inner(node: &mut FileNode, target: &Path, buf: &mut PathBuf) -> Option<u64> {
    let d = node.as_dir_mut()?;

    // Check direct children
    let found_pos = d.children.iter().enumerate().find_map(|(i, c)| {
        let child_path = buf.join(c.name());
        if child_path == target {
            Some(i)
        } else {
            None
        }
    });

    if let Some(pos) = found_pos {
        let removed_size = d.children[pos].size();
        d.children.remove(pos);
        d.size -= removed_size;
        return Some(removed_size);
    }

    // Recurse into child directories
    for child in &mut d.children {
        if child.is_dir() {
            buf.push(child.name());
            if let Some(removed_size) = remove_node_inner(child, target, buf) {
                buf.pop();
                d.size -= removed_size;
                return Some(removed_size);
            }
            buf.pop();
        }
    }

    None
}


/// Find the parent path of a node in the tree.
pub fn find_parent_path(node: &FileNode, target: &Path) -> Option<PathBuf> {
    let mut buf = PathBuf::from(node.name());
    find_parent_path_inner(node, target, &mut buf)
}

fn find_parent_path_inner(node: &FileNode, target: &Path, buf: &mut PathBuf) -> Option<PathBuf> {
    for child in node.children() {
        let child_path = buf.join(child.name());
        if child_path == target {
            return Some(buf.clone());
        }
        if child.is_dir() {
            buf.push(child.name());
            if let Some(parent) = find_parent_path_inner(child, target, buf) {
                buf.pop();
                return Some(parent);
            }
            buf.pop();
        }
    }
    None
}

/// Find a node by path and return (is_dir, expanded, has_children).
pub fn find_node_info(node: &FileNode, target: &Path) -> Option<(bool, bool, bool)> {
    let mut buf = PathBuf::from(node.name());
    find_node_info_inner(node, target, &mut buf)
}

fn find_node_info_inner(
    node: &FileNode,
    target: &Path,
    buf: &mut PathBuf,
) -> Option<(bool, bool, bool)> {
    if buf.as_path() == target {
        return Some((node.is_dir(), node.expanded(), !node.children().is_empty()));
    }
    for child in node.children() {
        buf.push(child.name());
        if let Some(info) = find_node_info_inner(child, target, buf) {
            buf.pop();
            return Some(info);
        }
        buf.pop();
    }
    None
}

/// Set expanded state for a node at target path. Returns true if found.
pub fn set_expanded(node: &mut FileNode, target: &Path, expanded: bool) -> bool {
    let mut buf = PathBuf::from(node.name());
    set_expanded_inner(node, target, expanded, &mut buf)
}

fn set_expanded_inner(
    node: &mut FileNode,
    target: &Path,
    expanded: bool,
    buf: &mut PathBuf,
) -> bool {
    if buf.as_path() == target {
        node.set_expanded(expanded);
        return true;
    }
    if let FileNode::Dir(d) = node {
        for child in &mut d.children {
            buf.push(child.name());
            if set_expanded_inner(child, target, expanded, buf) {
                buf.pop();
                return true;
            }
            buf.pop();
        }
    }
    false
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
    fn toggle_expand_flips_target() {
        let mut tree = dir("root", vec![dir("sub", vec![leaf("f.txt", 1)])]);
        assert!(!tree.children()[0].expanded());

        toggle_expand(&mut tree, Path::new("root/sub"));
        assert!(tree.children()[0].expanded());

        toggle_expand(&mut tree, Path::new("root/sub"));
        assert!(!tree.children()[0].expanded());
    }

    #[test]
    fn toggle_expand_returns_false_for_missing() {
        let mut tree = dir("root", vec![]);
        assert!(!toggle_expand(&mut tree, Path::new("nope")));
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

        let rows_a = collect_cached_rows(&tree, "", None, true);
        let rows_b = collect_cached_rows(&tree, "", None, true);

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
        let mut tree = dir(
            "root",
            vec![leaf(".hidden", 5), leaf("visible.txt", 10)],
        );
        tree.set_expanded(true);

        let rows = collect_cached_rows(&tree, "", None, false);
        // Root + visible.txt (hidden file excluded)
        assert_eq!(rows.len(), 2);
        assert_eq!(&*rows[1].name, "visible.txt");

        let rows_all = collect_cached_rows(&tree, "", None, true);
        // Root + .hidden + visible.txt
        assert_eq!(rows_all.len(), 3);
    }
}
