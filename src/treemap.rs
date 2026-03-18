use crate::tree::FileNode;
use bytesize::ByteSize;
use eframe::egui;
use std::path::{Path, PathBuf};

// ─── File-type colors ───────────────────────────────────────────

/// Returns a fill color based on file extension category.
pub fn extension_color(name: &str, is_dir: bool) -> egui::Color32 {
    if is_dir {
        return egui::Color32::from_rgb(70, 75, 85);
    }
    let ext = name.rsplit('.').next().unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        // Video — red
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "m4v" => {
            egui::Color32::from_rgb(192, 57, 43)
        }
        // Image — green
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "svg" | "webp" | "tiff" | "ico" | "heic" => {
            egui::Color32::from_rgb(39, 174, 96)
        }
        // Audio — purple
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "wma" | "m4a" | "opus" => {
            egui::Color32::from_rgb(142, 68, 173)
        }
        // Documents — blue
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "rtf" | "csv"
        | "pages" | "numbers" | "key" => egui::Color32::from_rgb(41, 128, 185),
        // Archives — orange
        "zip" | "tar" | "gz" | "rar" | "7z" | "bz2" | "xz" | "tgz" | "zst" | "dmg" | "iso" => {
            egui::Color32::from_rgb(211, 84, 0)
        }
        // Source code — teal
        "rs" | "js" | "ts" | "py" | "go" | "c" | "cpp" | "h" | "hpp" | "java" | "rb" | "swift"
        | "kt" | "cs" | "jsx" | "tsx" | "vue" | "svelte" => egui::Color32::from_rgb(22, 160, 133),
        // Config/data — dark blue-gray
        "json" | "yaml" | "yml" | "toml" | "xml" | "ini" | "cfg" | "conf" | "lock" => {
            egui::Color32::from_rgb(44, 62, 80)
        }
        // Web/markup — light teal
        "html" | "htm" | "css" | "scss" | "sass" | "less" | "md" | "mdx" => {
            egui::Color32::from_rgb(26, 188, 156)
        }
        // Build artifacts — dark red
        "o" | "obj" | "a" | "lib" | "rlib" | "d" | "rmeta" | "wasm" | "class" => {
            egui::Color32::from_rgb(146, 43, 33)
        }
        // Executables — bright orange
        "exe" | "dll" | "so" | "dylib" | "app" | "bin" | "msi" | "deb" | "rpm" => {
            egui::Color32::from_rgb(230, 126, 34)
        }
        // Temp/logs — gray
        "log" | "tmp" | "cache" | "bak" | "swp" | "swo" => egui::Color32::from_rgb(127, 140, 141),
        _ => egui::Color32::from_rgb(93, 109, 126),
    }
}

fn darken(c: egui::Color32, amount: u8) -> egui::Color32 {
    egui::Color32::from_rgb(
        c.r().saturating_sub(amount),
        c.g().saturating_sub(amount),
        c.b().saturating_sub(amount),
    )
}

fn text_color_for_bg(bg: egui::Color32) -> egui::Color32 {
    let lum = 0.299 * bg.r() as f32 + 0.587 * bg.g() as f32 + 0.114 * bg.b() as f32;
    if lum > 140.0 {
        egui::Color32::from_rgb(20, 20, 20)
    } else {
        egui::Color32::from_rgb(240, 240, 240)
    }
}

// ─── Squarified treemap layout ──────────────────────────────────

/// Worst aspect ratio for items in a strip laid against `side`.
///
/// `areas` are pixel-area values; `side` is the length of the shorter
/// dimension of the remaining rectangle.
fn worst_ratio(areas: &[f64], side: f64) -> f64 {
    let sum: f64 = areas.iter().sum();
    if sum <= 0.0 || side <= 0.0 {
        return f64::MAX;
    }
    // strip dimension perpendicular to `side`
    let d = sum / side;
    areas
        .iter()
        .map(|&a| {
            if a <= 0.0 {
                return f64::MAX;
            }
            let r = d * d / a;
            r.max(1.0 / r)
        })
        .fold(0.0f64, f64::max)
}

/// Compute squarified treemap layout.
///
/// `sizes` must be sorted **descending**. Returns egui `Rect`s in the
/// same order, filling the rectangle at (`x`, `y`, `w`, `h`).
pub fn squarify(sizes: &[f64], x: f32, y: f32, w: f32, h: f32) -> Vec<egui::Rect> {
    let n = sizes.len();
    if n == 0 || w <= 0.0 || h <= 0.0 {
        return vec![egui::Rect::NOTHING; n];
    }
    let total: f64 = sizes.iter().sum();
    if total <= 0.0 {
        return vec![egui::Rect::NOTHING; n];
    }
    let area = w as f64 * h as f64;
    let areas: Vec<f64> = sizes.iter().map(|&s| s / total * area).collect();
    let mut out = vec![egui::Rect::NOTHING; n];
    squarify_impl(&areas, x, y, w, h, &mut out, 0);
    out
}

fn squarify_impl(
    areas: &[f64],
    bx: f32,
    by: f32,
    bw: f32,
    bh: f32,
    out: &mut [egui::Rect],
    offset: usize,
) {
    if areas.is_empty() || bw <= 0.0 || bh <= 0.0 {
        return;
    }
    if areas.len() == 1 {
        out[offset] = egui::Rect::from_min_size(egui::pos2(bx, by), egui::vec2(bw, bh));
        return;
    }

    let shorter = (bw as f64).min(bh as f64);

    // Greedy strip building
    let mut strip_end = 1;
    let mut strip = vec![areas[0]];
    let mut best = worst_ratio(&strip, shorter);

    for (i, &area) in areas.iter().enumerate().skip(1) {
        strip.push(area);
        let w = worst_ratio(&strip, shorter);
        if w <= best {
            best = w;
            strip_end = i + 1;
        } else {
            strip.pop();
            break;
        }
    }

    let strip_areas = &areas[..strip_end];
    let strip_sum: f64 = strip_areas.iter().sum();

    if bw >= bh {
        // Strip fills full height, laid out vertically on the left
        let sw = (strip_sum / bh as f64) as f32;
        let mut cy = by;
        for (i, &a) in strip_areas.iter().enumerate() {
            let ch = (a / sw as f64) as f32;
            out[offset + i] = egui::Rect::from_min_size(egui::pos2(bx, cy), egui::vec2(sw, ch));
            cy += ch;
        }
        squarify_impl(
            &areas[strip_end..],
            bx + sw,
            by,
            bw - sw,
            bh,
            out,
            offset + strip_end,
        );
    } else {
        // Strip fills full width, laid out horizontally on top
        let sh = (strip_sum / bw as f64) as f32;
        let mut cx = bx;
        for (i, &a) in strip_areas.iter().enumerate() {
            let cw = (a / sh as f64) as f32;
            out[offset + i] = egui::Rect::from_min_size(egui::pos2(cx, by), egui::vec2(cw, sh));
            cx += cw;
        }
        squarify_impl(
            &areas[strip_end..],
            bx,
            by + sh,
            bw,
            bh - sh,
            out,
            offset + strip_end,
        );
    }
}

// ─── Tree navigation helpers ────────────────────────────────────

/// Find a node by path in the file tree.
pub fn find_node<'a>(node: &'a FileNode, target: &Path) -> Option<&'a FileNode> {
    let mut buf = PathBuf::from(node.name());
    find_node_inner(node, target, &mut buf)
}

fn find_node_inner<'a>(
    node: &'a FileNode,
    target: &Path,
    buf: &mut PathBuf,
) -> Option<&'a FileNode> {
    if buf.as_path() == target {
        return Some(node);
    }
    for child in node.children() {
        buf.push(child.name());
        if let Some(found) = find_node_inner(child, target, buf) {
            buf.pop();
            return Some(found);
        }
        buf.pop();
    }
    None
}

/// Build breadcrumb trail from root to `target`.
pub fn breadcrumbs(root: &FileNode, target: &Path) -> Vec<(String, PathBuf)> {
    let root_path = PathBuf::from(root.name());
    let mut trail = vec![(root.name().to_string(), root_path.clone())];
    if root_path.as_path() == target {
        return trail;
    }
    let mut buf = root_path;
    if breadcrumbs_walk(root, target, &mut buf, &mut trail) {
        trail
    } else {
        vec![(root.name().to_string(), PathBuf::from(root.name()))]
    }
}

fn breadcrumbs_walk(
    node: &FileNode,
    target: &Path,
    buf: &mut PathBuf,
    trail: &mut Vec<(String, PathBuf)>,
) -> bool {
    for child in node.children() {
        buf.push(child.name());
        let child_path = buf.clone();
        if child_path.as_path() == target {
            trail.push((child.name().to_string(), child_path));
            buf.pop();
            return true;
        }
        if child.is_dir() {
            trail.push((child.name().to_string(), child_path));
            if breadcrumbs_walk(child, target, buf, trail) {
                buf.pop();
                return true;
            }
            trail.pop();
        }
        buf.pop();
    }
    false
}

// ─── Treemap actions ────────────────────────────────────────────

pub enum TreemapAction {
    ZoomTo(PathBuf),
    Focus(PathBuf),
}

// ─── Rendering ──────────────────────────────────────────────────

const GAP: f32 = 1.5;
const DIR_HEADER_H: f32 = 20.0;
const MIN_LABEL_W: f32 = 32.0;

/// Render the full treemap view (breadcrumbs + map). Returns user-triggered actions.
pub fn render_treemap(
    ui: &mut egui::Ui,
    root: &FileNode,
    zoom_path: &Option<PathBuf>,
    focused_path: &Option<PathBuf>,
    zoom_anim_start: Option<f64>,
    category_filter: Option<crate::categories::FileCategory>,
    show_hidden: bool,
) -> Vec<TreemapAction> {
    let mut actions = Vec::new();
    let root_path = PathBuf::from(root.name());

    // Resolve the node we're viewing and its full path
    let (view_node, view_path) = if let Some(ref zp) = zoom_path {
        match find_node(root, zp) {
            Some(n) => (n, zp.clone()),
            None => (root, root_path.clone()),
        }
    } else {
        (root, root_path.clone())
    };

    // ── Breadcrumb bar ──
    let crumbs = zoom_path
        .as_ref()
        .map(|p| breadcrumbs(root, p))
        .unwrap_or_else(|| vec![(root.name().to_string(), root_path.clone())]);

    ui.horizontal(|ui| {
        // Back button — only shown when zoomed into a subdirectory
        if crumbs.len() > 1 {
            let parent_path = crumbs[crumbs.len() - 2].1.clone();
            if ui.button("< Back").clicked() {
                actions.push(TreemapAction::ZoomTo(parent_path));
            }
            ui.separator();
        }

        for (i, (name, path)) in crumbs.iter().enumerate() {
            if i > 0 {
                ui.label(">");
            }
            let label = if i == crumbs.len() - 1 {
                egui::RichText::new(name).strong()
            } else {
                egui::RichText::new(name)
            };
            if ui.link(label).clicked() {
                actions.push(TreemapAction::ZoomTo(path.clone()));
            }
        }
        ui.label(format!("  ({})", ByteSize::b(view_node.size())));
    });

    ui.add_space(4.0);

    // ── Zoom transition opacity ──
    let alpha = if let Some(start) = zoom_anim_start {
        let elapsed = (ui.input(|i| i.time) - start) as f32;
        let t = (elapsed / 0.2).clamp(0.0, 1.0);
        if t < 1.0 {
            ui.ctx().request_repaint();
        }
        t
    } else {
        1.0
    };

    // ── Treemap canvas ──
    let available = ui.available_size();
    let (full_rect, response) = ui.allocate_exact_size(available, egui::Sense::click());
    let painter = ui.painter_at(full_rect);

    // Background
    painter.rect_filled(full_rect, 0.0, ui.visuals().extreme_bg_color);

    if view_node.children().is_empty() {
        painter.text(
            full_rect.center(),
            egui::Align2::CENTER_CENTER,
            "Empty directory",
            egui::FontId::proportional(16.0),
            ui.visuals().text_color(),
        );
        return actions;
    }

    // Filter children by size, hidden status, and optional category
    let all_children: Vec<&FileNode> = view_node
        .children()
        .iter()
        .filter(|c| c.size() > 0)
        .filter(|c| show_hidden || !c.name().starts_with('.'))
        .filter(|c| {
            category_filter.is_none_or(|cat| crate::categories::node_matches_category(c, cat))
        })
        .collect();
    if all_children.is_empty() {
        return actions;
    }

    // Collapse tiny files into an "Other" bucket to reduce visual noise.
    // Files below 0.5% of total size are grouped together.
    let total_size: u64 = all_children.iter().map(|c| c.size()).sum();
    let threshold = (total_size as f64 * 0.005) as u64; // 0.5%
    let mut children: Vec<&FileNode> = Vec::new();
    let mut other_size: u64 = 0;
    let mut other_count: usize = 0;
    for c in &all_children {
        if c.size() < threshold && !c.is_dir() {
            other_size += c.size();
            other_count += 1;
        } else {
            children.push(c);
        }
    }

    // Build display entries: real children + optional "Other" bucket
    // We use an enum to track which entries are real nodes vs the bucket.
    let has_other = other_count > 0 && other_size > 0;
    let entry_count = children.len() + if has_other { 1 } else { 0 };
    if entry_count == 0 {
        return actions;
    }

    // Pre-compute child paths (None for the "Other" bucket)
    let child_paths: Vec<Option<PathBuf>> = children
        .iter()
        .map(|c| Some(view_path.join(c.name())))
        .chain(if has_other { vec![None] } else { vec![] })
        .collect();

    let mut sizes: Vec<f64> = children.iter().map(|c| c.size() as f64).collect();
    if has_other {
        sizes.push(other_size as f64);
    }
    let rects = squarify(
        &sizes,
        full_rect.min.x,
        full_rect.min.y,
        full_rect.width(),
        full_rect.height(),
    );

    let hover_pos = response.hover_pos();
    let mut hovered_idx: Option<usize> = None;

    for i in 0..entry_count {
        let r = rects[i].shrink(GAP);
        if r.width() <= 0.0 || r.height() <= 0.0 {
            continue;
        }

        if i < children.len() {
            // Real child
            let child = children[i];
            let is_focused = focused_path
                .as_ref()
                .is_some_and(|fp| child_paths[i].as_ref().is_some_and(|cp| *fp == *cp));

            if child.is_dir() && r.width() > 24.0 && r.height() > DIR_HEADER_H + 12.0 {
                paint_directory(
                    &painter,
                    child,
                    r,
                    is_focused,
                    focused_path,
                    alpha,
                    child_paths[i].as_ref().unwrap(),
                );
            } else {
                paint_leaf(&painter, child, r, is_focused, alpha);
            }
        } else {
            // "Other" bucket
            paint_other_bucket(&painter, other_count, other_size, r, alpha);
        }

        if let Some(pos) = hover_pos {
            if r.contains(pos) {
                hovered_idx = Some(i);
            }
        }
    }

    // Hover tooltip
    if let Some(idx) = hovered_idx {
        if idx < children.len() {
            let child = children[idx];
            let child_path = child_paths[idx].as_ref().unwrap();
            egui::Tooltip::always_open(
                ui.ctx().clone(),
                ui.layer_id(),
                ui.id().with("treemap_tip"),
                egui::PopupAnchor::Pointer,
            )
            .gap(12.0)
            .show(|ui| {
                ui.label(egui::RichText::new(child.name()).strong());
                ui.label(ByteSize::b(child.size()).to_string());
                if child.is_dir() {
                    ui.label(format!("{} items", child.children().len()));
                }
                ui.label(child_path.display().to_string());
            });
        } else {
            // Tooltip for the "Other" bucket
            egui::Tooltip::always_open(
                ui.ctx().clone(),
                ui.layer_id(),
                ui.id().with("treemap_tip"),
                egui::PopupAnchor::Pointer,
            )
            .gap(12.0)
            .show(|ui| {
                ui.label(
                    egui::RichText::new(format!("Other ({} files)", other_count)).strong(),
                );
                ui.label(ByteSize::b(other_size).to_string());
                ui.label("Small files collapsed into one block");
            });
        }
    }

    // Handle click
    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            for i in 0..entry_count {
                let r = rects[i].shrink(GAP);
                if r.contains(pos) {
                    if i < children.len() {
                        if children[i].is_dir() {
                            actions.push(TreemapAction::ZoomTo(
                                child_paths[i].clone().unwrap(),
                            ));
                        }
                        actions.push(TreemapAction::Focus(child_paths[i].clone().unwrap()));
                    }
                    break;
                }
            }
        }
    }

    actions
}

// ─── Painting helpers ───────────────────────────────────────────

fn apply_alpha(c: egui::Color32, alpha: f32) -> egui::Color32 {
    if alpha >= 1.0 {
        return c;
    }
    egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (c.a() as f32 * alpha) as u8)
}

fn paint_leaf(
    painter: &egui::Painter,
    node: &FileNode,
    rect: egui::Rect,
    is_focused: bool,
    alpha: f32,
) {
    let color = apply_alpha(extension_color(node.name(), node.is_dir()), alpha);
    painter.rect_filled(rect, 2.0, color);

    if is_focused {
        painter.rect_stroke(
            rect,
            2.0,
            egui::Stroke::new(2.0, egui::Color32::WHITE),
            egui::StrokeKind::Inside,
        );
    }

    // Label if large enough
    if rect.width() > MIN_LABEL_W && rect.height() > 14.0 {
        let tc = apply_alpha(
            text_color_for_bg(extension_color(node.name(), node.is_dir())),
            alpha,
        );
        let font = egui::FontId::proportional(11.0);
        let text = if rect.height() > 30.0 {
            format!("{}\n{}", node.name(), ByteSize::b(node.size()))
        } else {
            node.name().to_string()
        };
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, text, font, tc);
    }
}

fn paint_other_bucket(
    painter: &egui::Painter,
    count: usize,
    total_size: u64,
    rect: egui::Rect,
    alpha: f32,
) {
    let bg = apply_alpha(egui::Color32::from_rgb(80, 80, 80), alpha);
    painter.rect_filled(rect, 2.0, bg);

    // Dashed-style border to distinguish from real blocks
    painter.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0, apply_alpha(egui::Color32::from_rgb(120, 120, 120), alpha)),
        egui::StrokeKind::Inside,
    );

    if rect.width() > MIN_LABEL_W && rect.height() > 14.0 {
        let tc = apply_alpha(egui::Color32::from_rgb(200, 200, 200), alpha);
        let font = egui::FontId::proportional(11.0);
        let text = if rect.height() > 30.0 {
            format!(
                "Other ({} files)\n{}",
                count,
                ByteSize::b(total_size)
            )
        } else {
            format!("Other ({})", count)
        };
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, text, font, tc);
    }
}

fn paint_directory(
    painter: &egui::Painter,
    node: &FileNode,
    rect: egui::Rect,
    is_focused: bool,
    focused_path: &Option<PathBuf>,
    alpha: f32,
    node_path: &Path,
) {
    let bg = apply_alpha(extension_color(node.name(), true), alpha);
    let header_bg = apply_alpha(darken(extension_color(node.name(), true), 15), alpha);

    // Background
    painter.rect_filled(rect, 2.0, bg);

    if is_focused {
        painter.rect_stroke(
            rect,
            2.0,
            egui::Stroke::new(2.0, egui::Color32::WHITE),
            egui::StrokeKind::Inside,
        );
    }

    // Header
    let header_rect = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), DIR_HEADER_H));
    painter.rect_filled(header_rect, 2.0, header_bg);

    // Header text
    let tc = apply_alpha(
        text_color_for_bg(darken(extension_color(node.name(), true), 15)),
        alpha,
    );
    if rect.width() > MIN_LABEL_W {
        let label = format!("{} ({})", node.name(), ByteSize::b(node.size()));
        painter.text(
            header_rect.center(),
            egui::Align2::CENTER_CENTER,
            &label,
            egui::FontId::proportional(13.0),
            tc,
        );
    }

    // Nested children
    let content_rect = egui::Rect::from_min_max(
        egui::pos2(rect.min.x + 1.0, rect.min.y + DIR_HEADER_H),
        egui::pos2(rect.max.x - 1.0, rect.max.y - 1.0),
    );

    if content_rect.width() > 4.0 && content_rect.height() > 4.0 && !node.children().is_empty() {
        let nested: Vec<&FileNode> = node.children().iter().filter(|c| c.size() > 0).collect();
        if nested.is_empty() {
            return;
        }
        let child_sizes: Vec<f64> = nested.iter().map(|c| c.size() as f64).collect();
        let child_rects = squarify(
            &child_sizes,
            content_rect.min.x,
            content_rect.min.y,
            content_rect.width(),
            content_rect.height(),
        );

        for (j, child) in nested.iter().enumerate() {
            let cr = child_rects[j].shrink(0.5);
            if cr.width() <= 0.0 || cr.height() <= 0.0 {
                continue;
            }
            let color = apply_alpha(extension_color(child.name(), child.is_dir()), alpha);
            painter.rect_filled(cr, 1.0, color);

            let child_path = node_path.join(child.name());
            let child_focused = focused_path.as_ref().is_some_and(|fp| *fp == child_path);
            if child_focused {
                painter.rect_stroke(
                    cr,
                    1.0,
                    egui::Stroke::new(2.0, egui::Color32::WHITE),
                    egui::StrokeKind::Inside,
                );
            }

            // Label if large enough
            if cr.width() > MIN_LABEL_W && cr.height() > 12.0 {
                let tc = apply_alpha(
                    text_color_for_bg(extension_color(child.name(), child.is_dir())),
                    alpha,
                );
                painter.text(
                    cr.center(),
                    egui::Align2::CENTER_CENTER,
                    child.name(),
                    egui::FontId::proportional(10.0),
                    tc,
                );
            }
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{dir, leaf};

    #[test]
    fn squarify_single_item() {
        let rects = squarify(&[100.0], 0.0, 0.0, 200.0, 100.0);
        assert_eq!(rects.len(), 1);
        assert!((rects[0].width() - 200.0).abs() < 0.1);
        assert!((rects[0].height() - 100.0).abs() < 0.1);
    }

    #[test]
    fn squarify_two_equal() {
        let rects = squarify(&[50.0, 50.0], 0.0, 0.0, 200.0, 100.0);
        assert_eq!(rects.len(), 2);
        let total_area: f32 = rects.iter().map(|r| r.width() * r.height()).sum();
        assert!((total_area - 20000.0).abs() < 1.0);
    }

    #[test]
    fn squarify_preserves_area() {
        let sizes = vec![60.0, 30.0, 10.0];
        let rects = squarify(&sizes, 10.0, 20.0, 300.0, 200.0);
        assert_eq!(rects.len(), 3);
        let total_area: f32 = rects.iter().map(|r| r.width() * r.height()).sum();
        assert!((total_area - 60000.0).abs() < 10.0);
    }

    #[test]
    fn squarify_proportional() {
        // 75/25 split in a 100x100 rect
        let rects = squarify(&[75.0, 25.0], 0.0, 0.0, 100.0, 100.0);
        let a0 = rects[0].width() * rects[0].height();
        let a1 = rects[1].width() * rects[1].height();
        assert!((a0 / (a0 + a1) - 0.75).abs() < 0.01);
    }

    #[test]
    fn squarify_empty() {
        let rects = squarify(&[], 0.0, 0.0, 100.0, 100.0);
        assert!(rects.is_empty());
    }

    #[test]
    fn squarify_no_overlap() {
        let sizes = vec![50.0, 30.0, 15.0, 5.0];
        let rects = squarify(&sizes, 0.0, 0.0, 400.0, 300.0);
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let overlap = rects[i].intersect(rects[j]);
                assert!(
                    overlap.area() < 2.0,
                    "rects {i} and {j} overlap by {}",
                    overlap.area()
                );
            }
        }
    }

    #[test]
    fn squarify_many_items() {
        let sizes: Vec<f64> = (1..=20).rev().map(|i| i as f64).collect();
        let rects = squarify(&sizes, 0.0, 0.0, 800.0, 600.0);
        assert_eq!(rects.len(), 20);
        // All rects should have positive area
        for (i, r) in rects.iter().enumerate() {
            assert!(r.width() > 0.0, "rect {i} has zero width");
            assert!(r.height() > 0.0, "rect {i} has zero height");
        }
    }

    #[test]
    fn find_node_root() {
        let tree = dir("root", vec![leaf("a.txt", 10)]);
        assert!(find_node(&tree, Path::new("root")).is_some());
    }

    #[test]
    fn find_node_child() {
        let tree = dir("root", vec![leaf("a.txt", 10)]);
        assert!(find_node(&tree, Path::new("root/a.txt")).is_some());
    }

    #[test]
    fn find_node_missing() {
        let tree = dir("root", vec![leaf("a.txt", 10)]);
        assert!(find_node(&tree, Path::new("missing")).is_none());
    }

    #[test]
    fn find_node_nested() {
        let tree = dir("root", vec![dir("sub", vec![leaf("deep.txt", 5)])]);
        assert!(find_node(&tree, Path::new("root/sub/deep.txt")).is_some());
    }

    #[test]
    fn breadcrumbs_root() {
        let tree = dir("root", vec![]);
        let bc = breadcrumbs(&tree, Path::new("root"));
        assert_eq!(bc.len(), 1);
        assert_eq!(bc[0].0, "root");
    }

    #[test]
    fn breadcrumbs_nested() {
        let tree = dir("root", vec![dir("sub", vec![leaf("f.txt", 10)])]);
        let bc = breadcrumbs(&tree, Path::new("root/sub"));
        assert_eq!(bc.len(), 2);
        assert_eq!(bc[0].0, "root");
        assert_eq!(bc[1].0, "sub");
    }

    #[test]
    fn breadcrumbs_deep() {
        let tree = dir(
            "root",
            vec![dir("a", vec![dir("b", vec![leaf("c.txt", 1)])])],
        );
        let bc = breadcrumbs(&tree, Path::new("root/a/b"));
        assert_eq!(bc.len(), 3);
        assert_eq!(bc[2].0, "b");
    }

    #[test]
    fn breadcrumbs_missing_returns_root() {
        let tree = dir("root", vec![leaf("a.txt", 10)]);
        let bc = breadcrumbs(&tree, Path::new("missing"));
        assert_eq!(bc.len(), 1);
    }

    #[test]
    fn extension_color_video() {
        let c = extension_color("movie.mp4", false);
        assert_eq!(c, egui::Color32::from_rgb(192, 57, 43));
    }

    #[test]
    fn extension_color_dir() {
        let c = extension_color("src", true);
        assert_eq!(c, egui::Color32::from_rgb(70, 75, 85));
    }

    #[test]
    fn text_contrast() {
        // Dark bg should give light text
        let tc = text_color_for_bg(egui::Color32::from_rgb(20, 20, 20));
        assert!(tc.r() > 200);
        // Light bg should give dark text
        let tc = text_color_for_bg(egui::Color32::from_rgb(220, 220, 220));
        assert!(tc.r() < 50);
    }

    // ── worst_ratio tests ──

    #[test]
    fn worst_ratio_single_square() {
        // A single 100-area item on a 10-length side → strip is 10×10, ratio = 1
        let r = worst_ratio(&[100.0], 10.0);
        assert!((r - 1.0).abs() < 1e-9);
    }

    #[test]
    fn worst_ratio_zero_side() {
        assert_eq!(worst_ratio(&[100.0], 0.0), f64::MAX);
    }

    #[test]
    fn worst_ratio_zero_area() {
        assert_eq!(worst_ratio(&[0.0], 10.0), f64::MAX);
    }

    #[test]
    fn worst_ratio_empty() {
        assert_eq!(worst_ratio(&[], 10.0), f64::MAX);
    }

    #[test]
    fn worst_ratio_equal_items() {
        // Two 50-area items on a 10-length side → strip is 10×10, each 10×5, ratio = 2
        let r = worst_ratio(&[50.0, 50.0], 10.0);
        assert!((r - 2.0).abs() < 1e-9);
    }

    // ── squarify bounds and ordering tests ──

    #[test]
    fn squarify_rects_within_bounds() {
        let sizes = vec![50.0, 30.0, 15.0, 5.0];
        let (x, y, w, h) = (10.0, 20.0, 400.0, 300.0);
        let rects = squarify(&sizes, x, y, w, h);
        let bounds = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h));
        for (i, r) in rects.iter().enumerate() {
            assert!(
                r.min.x >= bounds.min.x - 0.1
                    && r.min.y >= bounds.min.y - 0.1
                    && r.max.x <= bounds.max.x + 0.1
                    && r.max.y <= bounds.max.y + 0.1,
                "rect {i} ({:?}) outside bounds ({:?})",
                r,
                bounds
            );
        }
    }

    #[test]
    fn squarify_with_offset_origin() {
        let rects = squarify(&[100.0], 50.0, 75.0, 200.0, 100.0);
        assert_eq!(rects.len(), 1);
        assert!((rects[0].min.x - 50.0).abs() < 0.1);
        assert!((rects[0].min.y - 75.0).abs() < 0.1);
        assert!((rects[0].width() - 200.0).abs() < 0.1);
        assert!((rects[0].height() - 100.0).abs() < 0.1);
    }

    #[test]
    fn squarify_zero_width() {
        let rects = squarify(&[100.0, 50.0], 0.0, 0.0, 0.0, 100.0);
        assert_eq!(rects.len(), 2);
        // All should be NOTHING rects
        for r in &rects {
            assert_eq!(*r, egui::Rect::NOTHING);
        }
    }

    #[test]
    fn squarify_zero_height() {
        let rects = squarify(&[100.0, 50.0], 0.0, 0.0, 100.0, 0.0);
        assert_eq!(rects.len(), 2);
        for r in &rects {
            assert_eq!(*r, egui::Rect::NOTHING);
        }
    }

    #[test]
    fn squarify_all_zero_sizes() {
        let rects = squarify(&[0.0, 0.0, 0.0], 0.0, 0.0, 100.0, 100.0);
        assert_eq!(rects.len(), 3);
        for r in &rects {
            assert_eq!(*r, egui::Rect::NOTHING);
        }
    }

    #[test]
    fn squarify_ordering_largest_gets_largest_rect() {
        let sizes = vec![100.0, 50.0, 25.0, 10.0];
        let rects = squarify(&sizes, 0.0, 0.0, 400.0, 300.0);
        let areas: Vec<f32> = rects.iter().map(|r| r.width() * r.height()).collect();
        // Each rect's area should be >= the next (matching descending size order)
        for i in 0..areas.len() - 1 {
            assert!(
                areas[i] >= areas[i + 1] - 1.0,
                "area[{}] = {} < area[{}] = {}",
                i,
                areas[i],
                i + 1,
                areas[i + 1]
            );
        }
    }

    #[test]
    fn squarify_tall_narrow_rect() {
        let sizes = vec![60.0, 30.0, 10.0];
        let rects = squarify(&sizes, 0.0, 0.0, 50.0, 600.0);
        assert_eq!(rects.len(), 3);
        let total_area: f32 = rects.iter().map(|r| r.width() * r.height()).sum();
        assert!((total_area - 30000.0).abs() < 10.0);
        for (i, r) in rects.iter().enumerate() {
            assert!(r.width() > 0.0, "rect {i} has zero width");
            assert!(r.height() > 0.0, "rect {i} has zero height");
        }
    }

    #[test]
    fn squarify_square_canvas() {
        let sizes = vec![25.0, 25.0, 25.0, 25.0];
        let rects = squarify(&sizes, 0.0, 0.0, 100.0, 100.0);
        let total_area: f32 = rects.iter().map(|r| r.width() * r.height()).sum();
        assert!((total_area - 10000.0).abs() < 1.0);
        // All rects should have equal area
        let expected_each = 2500.0f32;
        for (i, r) in rects.iter().enumerate() {
            let a = r.width() * r.height();
            assert!(
                (a - expected_each).abs() < 1.0,
                "rect {i} area = {a}, expected {expected_each}"
            );
        }
    }

    #[test]
    fn squarify_no_overlap_many_items() {
        let sizes: Vec<f64> = (1..=50).rev().map(|i| i as f64).collect();
        let rects = squarify(&sizes, 0.0, 0.0, 1000.0, 800.0);
        assert_eq!(rects.len(), 50);
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let overlap = rects[i].intersect(rects[j]);
                assert!(
                    overlap.area() < 2.0,
                    "rects {i} and {j} overlap by {}",
                    overlap.area()
                );
            }
        }
    }

    #[test]
    fn squarify_extreme_skew() {
        // One huge item and several tiny ones
        let sizes = vec![10000.0, 1.0, 1.0, 1.0];
        let rects = squarify(&sizes, 0.0, 0.0, 400.0, 300.0);
        assert_eq!(rects.len(), 4);
        let a0 = rects[0].width() * rects[0].height();
        let total_area: f32 = rects.iter().map(|r| r.width() * r.height()).sum();
        // First rect should dominate
        assert!(a0 / total_area > 0.99);
    }

    // ── darken / apply_alpha tests ──

    #[test]
    fn darken_reduces_rgb() {
        let c = egui::Color32::from_rgb(100, 150, 200);
        let d = darken(c, 30);
        assert_eq!(d, egui::Color32::from_rgb(70, 120, 170));
    }

    #[test]
    fn darken_saturates_at_zero() {
        let c = egui::Color32::from_rgb(10, 20, 30);
        let d = darken(c, 50);
        assert_eq!(d, egui::Color32::from_rgb(0, 0, 0));
    }

    #[test]
    fn apply_alpha_full() {
        let c = egui::Color32::from_rgb(100, 150, 200);
        let result = apply_alpha(c, 1.0);
        assert_eq!(result, c);
    }

    #[test]
    fn apply_alpha_half() {
        let c = egui::Color32::from_rgb(100, 150, 200);
        let result = apply_alpha(c, 0.5);
        // Alpha should be halved (255 * 0.5 ≈ 127)
        assert!((result.a() as f32 - 127.0).abs() < 2.0);
        // RGB premultiplied, so values are halved too
        assert!((result.r() as f32 - 50.0).abs() < 2.0);
    }

    // ── extension_color category coverage ──

    #[test]
    fn extension_color_categories() {
        // Audio
        assert_eq!(
            extension_color("song.mp3", false),
            egui::Color32::from_rgb(142, 68, 173)
        );
        // Image
        assert_eq!(
            extension_color("photo.png", false),
            egui::Color32::from_rgb(39, 174, 96)
        );
        // Archive
        assert_eq!(
            extension_color("backup.zip", false),
            egui::Color32::from_rgb(211, 84, 0)
        );
        // Source code
        assert_eq!(
            extension_color("main.rs", false),
            egui::Color32::from_rgb(22, 160, 133)
        );
        // Document
        assert_eq!(
            extension_color("report.pdf", false),
            egui::Color32::from_rgb(41, 128, 185)
        );
        // Config
        assert_eq!(
            extension_color("config.json", false),
            egui::Color32::from_rgb(44, 62, 80)
        );
        // Build artifact
        assert_eq!(
            extension_color("module.o", false),
            egui::Color32::from_rgb(146, 43, 33)
        );
        // Unknown → default gray
        assert_eq!(
            extension_color("random.xyz", false),
            egui::Color32::from_rgb(93, 109, 126)
        );
    }

    // ── find_node / breadcrumbs with deeper trees ──

    #[test]
    fn find_node_deeply_nested() {
        let tree = dir(
            "root",
            vec![dir(
                "a",
                vec![dir("b", vec![dir("c", vec![leaf("deep.txt", 1)])])],
            )],
        );
        assert!(find_node(&tree, Path::new("root/a/b/c/deep.txt")).is_some());
        assert!(find_node(&tree, Path::new("root/a/b/c")).is_some());
        assert!(find_node(&tree, Path::new("root/a/b/c/nope")).is_none());
    }

    #[test]
    fn find_node_among_siblings() {
        let tree = dir(
            "root",
            vec![
                leaf("a.txt", 10),
                leaf("b.txt", 20),
                dir("sub", vec![leaf("c.txt", 5)]),
            ],
        );
        assert!(find_node(&tree, Path::new("root/b.txt")).is_some());
        assert!(find_node(&tree, Path::new("root/sub/c.txt")).is_some());
    }

    #[test]
    fn breadcrumbs_deep_path() {
        let tree = dir(
            "root",
            vec![dir(
                "a",
                vec![dir("b", vec![dir("c", vec![leaf("d.txt", 1)])])],
            )],
        );
        let bc = breadcrumbs(&tree, Path::new("root/a/b/c"));
        assert_eq!(bc.len(), 4);
        assert_eq!(bc[0].0, "root");
        assert_eq!(bc[1].0, "a");
        assert_eq!(bc[2].0, "b");
        assert_eq!(bc[3].0, "c");
    }

    #[test]
    fn breadcrumbs_to_file() {
        let tree = dir("root", vec![dir("sub", vec![leaf("file.txt", 10)])]);
        let bc = breadcrumbs(&tree, Path::new("root/sub/file.txt"));
        assert_eq!(bc.len(), 3);
        assert_eq!(bc[2].0, "file.txt");
    }
}
