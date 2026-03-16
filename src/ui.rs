use bytesize::ByteSize;
use eframe::egui;

use crate::icons::IconCache;
use crate::tree::FileNode;

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

#[allow(clippy::too_many_arguments)]
pub fn render_tree(
    ui: &mut egui::Ui,
    node: &mut FileNode,
    depth: usize,
    parent_size: u64,
    filter: &str,
    focused_path: &mut Option<std::path::PathBuf>,
    category_filter: Option<crate::categories::FileCategory>,
    show_hidden: bool,
    icon_cache: Option<&IconCache>,
) {
    // Skip hidden files unless show_hidden is enabled
    if !show_hidden && node.name.starts_with('.') {
        return;
    }

    // Skip nodes that don't match the active text filter
    if !filter.is_empty() && !node_matches(node, filter) {
        return;
    }

    // Skip nodes that don't match the category filter
    if let Some(cat) = category_filter {
        if !crate::categories::node_matches_category(node, cat) {
            return;
        }
    }

    let indent = depth as f32 * 20.0;
    let size_str = ByteSize::b(node.size).to_string();
    let bcolor = bar_color(node.size, ui);
    let proportion = if parent_size > 0 {
        (node.size as f64 / parent_size as f64) as f32
    } else {
        1.0
    };
    let is_focused = focused_path.as_deref() == Some(node.path.as_path());

    ui.horizontal(|ui| {
        ui.add_space(indent);

        // Multi-select checkbox
        ui.checkbox(&mut node.selected, "");

        // Expand/collapse toggle for directories
        if node.is_dir {
            let label = if node.expanded { "\u{25BE}" } else { "\u{25B8}" };
            if ui.small_button(label).clicked() {
                node.expanded = !node.expanded;
            }
        } else {
            ui.add_space(24.0); // align with dir toggles
        }

        // Icon — native system icons (with emoji fallback)
        if let Some(icons) = icon_cache {
            let tex = if node.is_dir { &icons.folder } else { &icons.file };
            let icon_size = egui::vec2(16.0, 16.0);
            ui.image(egui::load::SizedTexture::new(tex.id(), icon_size));
        } else {
            let icon = if node.is_dir { "\u{1F4C1}" } else { "\u{1F4C4}" };
            ui.label(icon);
        }

        // Name — selectable for keyboard focus (highlighted when focused)
        let name_text = egui::RichText::new(&node.name).monospace();
        if ui.selectable_label(is_focused, name_text).clicked() {
            *focused_path = Some(node.path.clone());
        }

        // Size bar — proportional to parent
        let bar_width = 80.0_f32;
        let bar_height = 10.0_f32;
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(bar_width, bar_height), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
        let fill_w = (bar_width * proportion.clamp(0.0, 1.0)).max(1.0);
        let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, bar_height));
        painter.rect_filled(fill_rect, 2.0, bcolor);

        // Size label — right-aligned with fixed width for alignment
        let size_text = egui::RichText::new(format!("{:>10}", size_str)).monospace();
        ui.label(size_text);
    });

    // Render children if expanded (or auto-expanded by active filter)
    let show_children = node.is_dir && (node.expanded || !filter.is_empty());
    if show_children {
        let node_size = node.size;
        for child in &mut node.children {
            render_tree(
                ui,
                child,
                depth + 1,
                node_size,
                filter,
                focused_path,
                category_filter,
                show_hidden,
                icon_cache,
            );
        }
    }
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
