use bytesize::ByteSize;
use eframe::egui;

use crate::tree::FileNode;

/// Actions the UI wants to perform after rendering
pub enum NodeAction {
    Trash(std::path::PathBuf),
    Delete(std::path::PathBuf),
}

fn size_color(size: u64, ui: &egui::Ui) -> egui::Color32 {
    if size > 1_000_000_000 {
        egui::Color32::from_rgb(220, 60, 60) // red >1GB
    } else if size > 100_000_000 {
        egui::Color32::from_rgb(220, 150, 50) // orange >100MB
    } else {
        ui.visuals().text_color()
    }
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

pub fn render_tree(
    ui: &mut egui::Ui,
    node: &mut FileNode,
    depth: usize,
    parent_size: u64,
    actions: &mut Vec<NodeAction>,
) {
    let indent = depth as f32 * 20.0;
    let size_str = ByteSize::b(node.size).to_string();
    let color = size_color(node.size, ui);
    let proportion = if parent_size > 0 {
        (node.size as f64 / parent_size as f64) as f32
    } else {
        1.0
    };

    ui.horizontal(|ui| {
        ui.add_space(indent);

        // Multi-select checkbox
        ui.checkbox(&mut node.selected, "");

        // Expand/collapse toggle for directories
        if node.is_dir {
            let label = if node.expanded { "v" } else { ">" };
            if ui.small_button(label).clicked() {
                node.expanded = !node.expanded;
            }
        } else {
            ui.add_space(24.0); // align with dir toggles
        }

        // Icon
        let icon = if node.is_dir { "D" } else { "F" };
        ui.monospace(icon);

        // Name (colored by size tier)
        let name_text = egui::RichText::new(&node.name).monospace().color(color);
        ui.label(name_text);

        // Size bar — proportional to parent
        let bar_width = 100.0_f32;
        let bar_height = 12.0_f32;
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(bar_width, bar_height), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
        let fill_w = (bar_width * proportion.clamp(0.0, 1.0)).max(1.0);
        let fill_rect =
            egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, bar_height));
        painter.rect_filled(fill_rect, 2.0, color);

        // Size label
        ui.monospace(&size_str);

        // Action buttons
        if ui.small_button("Trash").clicked() {
            actions.push(NodeAction::Trash(node.path.clone()));
        }
        if ui.small_button("Delete").clicked() {
            actions.push(NodeAction::Delete(node.path.clone()));
        }
    });

    // Render children if expanded
    if node.is_dir && node.expanded {
        let node_size = node.size;
        for child in &mut node.children {
            render_tree(ui, child, depth + 1, node_size, actions);
        }
    }
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
