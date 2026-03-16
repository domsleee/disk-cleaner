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
    node.name.to_lowercase().contains(query) || node.children.iter().any(|c| node_matches(c, query))
}

pub fn collect_selected(node: &FileNode) -> Vec<std::path::PathBuf> {
    let mut result = Vec::new();
    if node.selected {
        result.push(node.path.clone());
    } else {
        for child in &node.children {
            result.extend(collect_selected(child));
        }
    }
    result
}

pub fn count_selected(node: &FileNode) -> usize {
    if node.selected {
        1
    } else {
        node.children.iter().map(count_selected).sum()
    }
}

/// Clear the `selected` flag on all nodes in the tree.
pub fn clear_selection(node: &mut FileNode) {
    node.selected = false;
    for child in &mut node.children {
        clear_selection(child);
    }
}

/// Set `selected = true` on the node matching `target`. Returns true if found.
fn select_node(node: &mut FileNode, target: &std::path::Path) -> bool {
    if node.path == target {
        node.selected = true;
        return true;
    }
    node.children.iter_mut().any(|c| select_node(c, target))
}

/// Actions produced by tree rendering, applied after the frame.
pub enum TreeAction {
    ToggleExpand(std::path::PathBuf),
    Click {
        path: std::path::PathBuf,
        shift: bool,
    },
    Focus(std::path::PathBuf),
}

/// Flattened row data for virtualized rendering.
struct VisibleRow {
    path: std::path::PathBuf,
    name: String,
    size: u64,
    is_dir: bool,
    expanded: bool,
    selected: bool,
    depth: usize,
    parent_size: u64,
    children_count: usize,
}

fn collect_visible_rows(
    node: &FileNode,
    depth: usize,
    parent_size: u64,
    filter: &str,
    category_filter: Option<crate::categories::FileCategory>,
    show_hidden: bool,
    result: &mut Vec<VisibleRow>,
) {
    if !show_hidden && node.name.starts_with('.') {
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

    result.push(VisibleRow {
        path: node.path.clone(),
        name: node.name.clone(),
        size: node.size,
        is_dir: node.is_dir,
        expanded: node.expanded,
        selected: node.selected,
        depth,
        parent_size,
        children_count: node.children.len(),
    });

    let show_children = node.is_dir && (node.expanded || !filter.is_empty());
    if show_children {
        for child in &node.children {
            collect_visible_rows(
                child,
                depth + 1,
                node.size,
                filter,
                category_filter,
                show_hidden,
                result,
            );
        }
    }
}

/// Render the tree view with virtualized scrolling. Returns actions to apply.
#[allow(clippy::too_many_arguments)]
pub fn render_tree(
    ui: &mut egui::Ui,
    tree: &FileNode,
    root_size: u64,
    filter: &str,
    focused_path: &Option<std::path::PathBuf>,
    category_filter: Option<crate::categories::FileCategory>,
    show_hidden: bool,
    icon_cache: Option<&IconCache>,
    scroll_to_focus: bool,
) -> Vec<TreeAction> {
    let mut rows = Vec::new();
    collect_visible_rows(
        tree,
        0,
        root_size,
        filter,
        category_filter,
        show_hidden,
        &mut rows,
    );

    let total_rows = rows.len();
    let row_height = 20.0_f32;
    let mut actions = Vec::new();

    let focused_idx =
        focused_path
            .as_ref()
            .and_then(|fp| rows.iter().position(|r| r.path == *fp));

    let row_total = row_height + ui.spacing().item_spacing.y;

    let mut scroll_area = egui::ScrollArea::vertical().auto_shrink([false, false]);

    // Scroll to focused row when arrow keys move focus
    if scroll_to_focus {
        if let Some(idx) = focused_idx {
            let target_y = idx as f32 * row_total;
            let viewport_h = ui.available_height();
            scroll_area = scroll_area.vertical_scroll_offset(
                (target_y - viewport_h / 2.0 + row_height / 2.0).max(0.0),
            );
        }
    }

    scroll_area.show_rows(ui, row_height, total_rows, |ui, range| {
        for i in range {
            let row = &rows[i];
            let indent = row.depth as f32 * 20.0;
            let bcolor = bar_color(row.size, ui);
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
                    let tex = if row.is_dir { &icons.folder } else { &icons.file };
                    ui.image(egui::load::SizedTexture::new(
                        tex.id(),
                        egui::vec2(16.0, 16.0),
                    ));
                } else {
                    let icon = if row.is_dir { "\u{1F4C1}" } else { "\u{1F4C4}" };
                    ui.label(icon);
                }

                // Name
                ui.label(egui::RichText::new(&row.name).monospace());

                // Size bar + label (right-aligned)
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        let size_str = ByteSize::b(row.size).to_string();
                        ui.label(
                            egui::RichText::new(format!("{:>10}", size_str)).monospace(),
                        );

                        let bar_width = 80.0_f32;
                        let bar_h = 10.0_f32;
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(bar_width, bar_h),
                            egui::Sense::hover(),
                        );
                        let painter = ui.painter();
                        painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
                        let fill_w = (bar_width * proportion.clamp(0.0, 1.0)).max(1.0);
                        let fill_rect =
                            egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, bar_h));
                        painter.rect_filled(fill_rect, 2.0, bcolor);
                    },
                );

                toggle_right
            });

            let toggle_right = row_response.inner;
            let row_rect = row_response.response.rect;

            // Single row interaction — toggle vs click determined by pointer position
            let row_id = egui::Id::new(("tree_row", row.path.as_os_str()));
            let row_interact = ui.interact(row_rect, row_id, egui::Sense::click());

            if row_interact.clicked() {
                if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                    if row.is_dir && pos.x <= toggle_right {
                        // Click on disclosure triangle area → toggle expand
                        actions.push(TreeAction::ToggleExpand(row.path.clone()));
                    } else {
                        // Click on content area → select/focus
                        let shift = ui.input(|i| i.modifiers.shift || i.modifiers.command);
                        actions.push(TreeAction::Click {
                            path: row.path.clone(),
                            shift,
                        });
                        actions.push(TreeAction::Focus(row.path.clone()));
                    }
                }
            }

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

            // Focus/selection background — both use blue selection color
            if row.selected || is_focused {
                let bg_color = if row.selected {
                    ui.visuals().selection.bg_fill.linear_multiply(0.3)
                } else {
                    ui.visuals().selection.bg_fill.linear_multiply(0.4)
                };
                ui.painter().set(
                    bg_idx,
                    egui::Shape::rect_filled(row_rect, 0.0, bg_color),
                );
            }
        }
    });

    actions
}

/// Apply tree actions to update tree state after rendering.
pub fn apply_tree_actions(tree: &mut FileNode, actions: &[TreeAction]) {
    for action in actions {
        match action {
            TreeAction::ToggleExpand(path) => {
                toggle_expand(tree, path);
            }
            TreeAction::Click { path, shift } => {
                if *shift {
                    toggle_selected(tree, path);
                } else {
                    clear_selection(tree);
                    select_node(tree, path);
                }
            }
            TreeAction::Focus(_) => {} // handled by caller updating focused_path
        }
    }
}

fn toggle_selected(node: &mut FileNode, target: &std::path::Path) -> bool {
    if node.path == target {
        node.selected = !node.selected;
        return true;
    }
    node.children
        .iter_mut()
        .any(|c| toggle_selected(c, target))
}

/// Toggle expand/collapse for the node at `target`. Returns true if found.
pub fn toggle_expand(node: &mut FileNode, target: &std::path::Path) -> bool {
    if node.path == target {
        node.expanded = !node.expanded;
        return true;
    }
    node.children.iter_mut().any(|c| toggle_expand(c, target))
}

/// Remove a node from the tree by path, returning the removed size so parents can update.
pub fn remove_node(node: &mut FileNode, target: &std::path::Path) -> Option<u64> {
    if let Some(pos) = node.children.iter().position(|c| c.path == target) {
        let removed_size = node.children[pos].size;
        node.children.remove(pos);
        node.size -= removed_size;
        return Some(removed_size);
    }

    for child in &mut node.children {
        if child.is_dir {
            if let Some(removed_size) = remove_node(child, target) {
                node.size -= removed_size;
                return Some(removed_size);
            }
        }
    }

    None
}

/// Collect paths of all visible nodes in render order (for keyboard navigation).
pub fn collect_visible_paths(
    node: &FileNode,
    filter: &str,
    category_filter: Option<crate::categories::FileCategory>,
    show_hidden: bool,
    result: &mut Vec<std::path::PathBuf>,
) {
    if !show_hidden && node.name.starts_with('.') {
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

    result.push(node.path.clone());

    let show_children = node.is_dir && (node.expanded || !filter.is_empty());
    if show_children {
        for child in &node.children {
            collect_visible_paths(child, filter, category_filter, show_hidden, result);
        }
    }
}

/// Find the parent path of a node in the tree.
pub fn find_parent_path(node: &FileNode, target: &std::path::Path) -> Option<std::path::PathBuf> {
    for child in &node.children {
        if child.path == target {
            return Some(node.path.clone());
        }
        if let Some(parent) = find_parent_path(child, target) {
            return Some(parent);
        }
    }
    None
}

/// Find a node by path and return (is_dir, expanded, has_children).
pub fn find_node_info(node: &FileNode, target: &std::path::Path) -> Option<(bool, bool, bool)> {
    if node.path == target {
        return Some((node.is_dir, node.expanded, !node.children.is_empty()));
    }
    node.children
        .iter()
        .find_map(|c| find_node_info(c, target))
}

/// Set expanded state for a node at target path. Returns true if found.
pub fn set_expanded(node: &mut FileNode, target: &std::path::Path, expanded: bool) -> bool {
    if node.path == target {
        node.expanded = expanded;
        return true;
    }
    node.children
        .iter_mut()
        .any(|c| set_expanded(c, target, expanded))
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
    fn collect_selected_returns_selected_nodes() {
        let mut tree = dir(
            "root",
            vec![leaf("a.txt", 10), leaf("b.txt", 20), leaf("c.txt", 30)],
        );
        tree.children[0].selected = true;
        tree.children[2].selected = true;

        let selected = collect_selected(&tree);
        assert_eq!(selected.len(), 2);
        assert!(selected.contains(&tree.children[0].path));
        assert!(selected.contains(&tree.children[2].path));
    }

    #[test]
    fn collect_selected_stops_at_selected_parent() {
        let mut tree = dir("root", vec![dir("sub", vec![leaf("deep.txt", 5)])]);
        tree.children[0].selected = true;

        let selected = collect_selected(&tree);
        // Should return the parent, not recurse into children
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0], tree.children[0].path);
    }

    #[test]
    fn count_selected_counts_correctly() {
        let mut tree = dir(
            "root",
            vec![leaf("a.txt", 10), leaf("b.txt", 20), leaf("c.txt", 30)],
        );
        assert_eq!(count_selected(&tree), 0);
        tree.children[1].selected = true;
        assert_eq!(count_selected(&tree), 1);
        tree.children[2].selected = true;
        assert_eq!(count_selected(&tree), 2);
    }

    #[test]
    fn toggle_expand_flips_target() {
        let mut tree = dir("root", vec![dir("sub", vec![leaf("f.txt", 1)])]);
        assert!(!tree.children[0].expanded);

        toggle_expand(&mut tree, std::path::Path::new("sub"));
        assert!(tree.children[0].expanded);

        toggle_expand(&mut tree, std::path::Path::new("sub"));
        assert!(!tree.children[0].expanded);
    }

    #[test]
    fn toggle_expand_returns_false_for_missing() {
        let mut tree = dir("root", vec![]);
        assert!(!toggle_expand(&mut tree, std::path::Path::new("nope")));
    }

    #[test]
    fn remove_node_direct_child() {
        let mut tree = dir("root", vec![leaf("a.txt", 10), leaf("b.txt", 20)]);
        assert_eq!(tree.size, 30);

        let removed = remove_node(&mut tree, std::path::Path::new("a.txt"));
        assert_eq!(removed, Some(10));
        assert_eq!(tree.size, 20);
        assert_eq!(tree.children.len(), 1);
        assert_eq!(tree.children[0].name, "b.txt");
    }

    #[test]
    fn remove_node_nested() {
        let mut tree = dir("root", vec![dir("sub", vec![leaf("deep.txt", 100)])]);
        assert_eq!(tree.size, 100);

        let removed = remove_node(&mut tree, std::path::Path::new("deep.txt"));
        assert_eq!(removed, Some(100));
        assert_eq!(tree.size, 0);
        assert_eq!(tree.children[0].size, 0);
        assert!(tree.children[0].children.is_empty());
    }

    #[test]
    fn remove_node_returns_none_for_missing() {
        let mut tree = dir("root", vec![leaf("a.txt", 10)]);
        assert_eq!(remove_node(&mut tree, std::path::Path::new("nope")), None);
        assert_eq!(tree.size, 10); // unchanged
    }
}
