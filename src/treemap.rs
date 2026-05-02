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

#[cfg(test)]
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

// ─── Cached treemap layout ─────────────────────────────────────

pub struct TreemapCache {
    pub tiles: Vec<TreemapTile>,
    pub other: Option<OtherBucket>,
    pub breadcrumbs: Vec<(String, PathBuf)>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub view_size: u64,
    /// Pre-formatted view size string for breadcrumb display.
    pub view_size_label: Box<str>,
    pub layout_size: (f32, f32),
}

pub struct TreemapTile {
    pub rect: egui::Rect,
    pub path: PathBuf,
    pub name: Box<str>,
    pub size: u64,
    pub is_dir: bool,
    pub color: egui::Color32,
    pub child_count: Option<usize>,
    pub nested: Vec<NestedTile>,
}

pub struct NestedTile {
    pub rect: egui::Rect,
    pub path: PathBuf,
    #[allow(dead_code)]
    pub is_dir: bool,
    pub color: egui::Color32,
    /// Recursive size of this child — needed so we can render the
    /// "X GB" label inside the nested tile (matches the during-scan
    /// view's nested tile layout).
    pub size: u64,
}

pub struct OtherBucket {
    pub rect: egui::Rect,
    #[cfg_attr(not(test), allow(dead_code))]
    pub count: usize,
    pub size: u64,
    /// Pre-formatted short label, e.g. "Other (123 files)".
    pub label_short: Box<str>,
}

/// Build a cached treemap layout from the given tree and parameters.
///
/// This extracts all filtering + squarifying logic from `render_treemap` into a
/// pure function whose result can be stored and reused across frames.
pub fn build_treemap_cache(
    root: &FileNode,
    zoom_path: &Option<PathBuf>,
    category_filter: Option<crate::categories::FileCategory>,
    show_hidden: bool,
    full_rect: egui::Rect,
) -> TreemapCache {
    let root_path = PathBuf::from(root.name());

    // Resolve the node we're viewing and its full path
    let (view_node, view_path) = if let Some(zp) = zoom_path {
        match find_node(root, zp) {
            Some(n) => (n, zp.clone()),
            None => (root, root_path.clone()),
        }
    } else {
        (root, root_path.clone())
    };

    let view_size = view_node.size();
    let view_size_label: Box<str> =
        format!("  ({})", fmt_size_compact(view_size)).into();

    // Cache breadcrumbs (avoids O(N) tree walk every frame)
    let cached_breadcrumbs = zoom_path
        .as_ref()
        .map(|p| breadcrumbs(root, p))
        .unwrap_or_else(|| vec![(root.name().to_string(), root_path.clone())]);

    // Empty directory — return empty cache
    if view_node.children().is_empty() {
        return TreemapCache {
            tiles: vec![],
            other: None,
            breadcrumbs: cached_breadcrumbs,
            view_size,
            view_size_label: view_size_label.clone(),
            layout_size: (full_rect.width(), full_rect.height()),
        };
    }

    // Filter children by size, hidden status, and optional category
    let all_children: Vec<&FileNode> = view_node
        .children()
        .iter()
        .filter(|c| c.size() > 0)
        .filter(|c| show_hidden || !c.is_hidden())
        .filter(|c| {
            category_filter.is_none_or(|cat| crate::categories::node_matches_category(c, cat))
        })
        .collect();

    if all_children.is_empty() {
        return TreemapCache {
            tiles: vec![],
            other: None,
            breadcrumbs: cached_breadcrumbs,
            view_size,
            view_size_label: view_size_label.clone(),
            layout_size: (full_rect.width(), full_rect.height()),
        };
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

    // Hard cap: if we still have too many entries, keep only the largest
    // and fold the rest into "Other".
    if children.len() > MAX_VISIBLE_ENTRIES {
        children.sort_by_key(|c| std::cmp::Reverse(c.size()));
        for c in children.drain(MAX_VISIBLE_ENTRIES..) {
            other_size += c.size();
            other_count += 1;
        }
    }

    let has_other = other_count > 0 && other_size > 0;
    let entry_count = children.len() + if has_other { 1 } else { 0 };

    if entry_count == 0 {
        return TreemapCache {
            tiles: vec![],
            other: None,
            breadcrumbs: cached_breadcrumbs,
            view_size,
            view_size_label: view_size_label.clone(),
            layout_size: (full_rect.width(), full_rect.height()),
        };
    }

    // Compute squarified layout
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

    // Build tiles for real children, tracking global nested budget
    let mut tiles: Vec<TreemapTile> = Vec::with_capacity(children.len());
    let mut nested_budget = MAX_TOTAL_NESTED;
    // Every level uses the rank palette by rank-in-parent-by-size.
    // Originally I gated this on root_view-only and fell back to
    // extension_color for zoomed-in views — but extension_color
    // returns the same dark grey for every directory, which makes a
    // zoomed-in dir-of-dirs look like a flat slab of identical tiles.
    // Rank palette cycles through 8 hues so adjacent siblings always
    // read as distinct, regardless of depth.  Files inside zoomed
    // views still get a useful colour because they're sized within
    // the same rank order as their dir siblings.
    for (i, child) in children.iter().enumerate() {
        let r = rects[i].shrink(GAP);
        if r.width() <= 0.0 || r.height() <= 0.0 || r.area() < MIN_PAINT_AREA {
            continue;
        }
        let child_path = view_path.join(child.name());
        let is_dir = child.is_dir();
        let color = scan_rank_palette(i);
        let child_count = if is_dir {
            Some(child.children().len())
        } else {
            None
        };

        // Always propagate the parent rank colour to nested children.
        // Inside any directory tile the nested sub-tiles read as
        // dimmed variants of the parent so the whole subtree looks
        // like a coherent block rather than a rainbow.
        let nested = if is_dir
            && nested_budget > 0
            && r.width() > 40.0
            && r.height() > DIR_HEADER_H + 16.0
        {
            let tiles = build_nested_tiles(child, &child_path, r, nested_budget, Some(color));
            nested_budget = nested_budget.saturating_sub(tiles.len());
            tiles
        } else {
            vec![]
        };

        let name: Box<str> = child.name().into();
        let size = child.size();

        tiles.push(TreemapTile {
            rect: r,
            path: child_path,
            name,
            size,
            is_dir,
            color,
            child_count,
            nested,
        });
    }

    // Build Other bucket if needed
    let other = if has_other {
        let other_idx = children.len();
        let r = rects[other_idx].shrink(GAP);
        Some(OtherBucket {
            rect: r,
            count: other_count,
            size: other_size,
            label_short: format!("Other ({})", other_count).into(),
        })
    } else {
        None
    };

    TreemapCache {
        tiles,
        other,
        breadcrumbs: cached_breadcrumbs,
        view_size,
        view_size_label,
        layout_size: (full_rect.width(), full_rect.height()),
    }
}

/// Build nested sub-tiles for the children of a directory tile.
/// `budget` caps how many nested tiles to create (global limit).
/// Extracts the nested layout logic from `paint_directory`.
///
/// `parent_color` is the parent dir tile's colour.  When provided,
/// nested children are coloured with a darker variant of it so the
/// whole subtree reads as one block (matches the during-scan view).
/// When None, nested children fall back to extension/category
/// colours — useful in zoomed-in views where the user wants
/// content-type cues.
fn build_nested_tiles(
    node: &FileNode,
    node_path: &Path,
    rect: egui::Rect,
    budget: usize,
    parent_color: Option<egui::Color32>,
) -> Vec<NestedTile> {
    let content_rect = egui::Rect::from_min_max(
        egui::pos2(rect.min.x + 1.0, rect.min.y + DIR_HEADER_H),
        egui::pos2(rect.max.x - 1.0, rect.max.y - 1.0),
    );

    if content_rect.width() <= 4.0 || content_rect.height() <= 4.0 || node.children().is_empty() {
        return vec![];
    }

    let mut nested: Vec<&FileNode> = node.children().iter().filter(|c| c.size() > 0).collect();
    if nested.is_empty() {
        return vec![];
    }
    let limit = MAX_NESTED_CHILDREN.min(budget);
    if nested.len() > limit {
        nested.sort_by_key(|c| std::cmp::Reverse(c.size()));
        nested.truncate(limit);
    }

    let child_sizes: Vec<f64> = nested.iter().map(|c| c.size() as f64).collect();
    let child_rects = squarify(
        &child_sizes,
        content_rect.min.x,
        content_rect.min.y,
        content_rect.width(),
        content_rect.height(),
    );

    let mut result = Vec::with_capacity(nested.len());
    for (j, child) in nested.iter().enumerate() {
        let cr = child_rects[j].shrink(0.5);
        if cr.width() <= 0.0 || cr.height() <= 0.0 || cr.area() < MIN_PAINT_AREA {
            continue;
        }
        let child_path = node_path.join(child.name());
        let is_dir = child.is_dir();
        let color = match parent_color {
            // Slightly-dimmed variant of the parent.  Same logic as
            // the during-scan view's nested tiles.
            Some(p) => p.linear_multiply(0.55),
            None => extension_color(child.name(), is_dir),
        };
        result.push(NestedTile {
            rect: cr,
            path: child_path,
            is_dir,
            color,
            size: child.size(),
        });
    }
    result
}

// ─── Treemap actions ────────────────────────────────────────────

pub enum TreemapAction {
    ZoomTo(PathBuf),
    Focus(PathBuf),
}

// ─── Rendering ──────────────────────────────────────────────────

const GAP: f32 = 1.5;
const DIR_HEADER_H: f32 = 20.0;
/// Hard cap on visible top-level entries to prevent lag with huge directories.
const MAX_VISIBLE_ENTRIES: usize = 200;
/// Minimum rect area (px²) worth painting — below this we skip.
const MIN_PAINT_AREA: f32 = 4.0;
/// Maximum nested children to paint inside a directory tile.
const MAX_NESTED_CHILDREN: usize = 50;
/// Global budget for total nested tiles across all directory tiles.
/// Prevents 200 dirs × 50 nested = 10K paint ops scenario.
const MAX_TOTAL_NESTED: usize = 1000;

/// Render the full treemap view (breadcrumbs + map) from a cached layout.
/// Returns user-triggered actions.
#[allow(clippy::too_many_arguments)]
pub fn render_treemap(
    ui: &mut egui::Ui,
    cache: &mut Option<TreemapCache>,
    cache_dirty: &mut bool,
    root: &FileNode,
    zoom_path: &Option<PathBuf>,
    focused_path: &Option<PathBuf>,
    zoom_anim_start: Option<f64>,
    category_filter: Option<crate::categories::FileCategory>,
    show_hidden: bool,
) -> Vec<TreemapAction> {
    let mut actions = Vec::new();
    let root_path = PathBuf::from(root.name());

    // ── Breadcrumb bar ──
    // Use cached breadcrumbs when available to avoid O(N) tree walk every frame.
    // On first frame (cache not yet built), compute inline.
    let have_cached_crumbs = cache.is_some();
    let inline_crumbs;
    let crumbs: &[(String, PathBuf)] = if have_cached_crumbs {
        &cache.as_ref().unwrap().breadcrumbs
    } else {
        inline_crumbs = zoom_path
            .as_ref()
            .map(|p| breadcrumbs(root, p))
            .unwrap_or_else(|| vec![(root.name().to_string(), root_path.clone())]);
        &inline_crumbs
    };
    let view_size_label: Option<&str> = cache.as_ref().map(|c| c.view_size_label.as_ref());
    let inline_size_label;
    let size_label = if let Some(l) = view_size_label {
        l
    } else {
        let view_size = if let Some(zp) = zoom_path {
            find_node(root, zp).map_or(root.size(), |n| n.size())
        } else {
            root.size()
        };
        inline_size_label = format!("  ({})", fmt_size_compact(view_size));
        &inline_size_label
    };

    ui.horizontal(|ui| {
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
        ui.label(size_label);
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
    // Reserve breathing room around the canvas so tiles don't run
    // flush to the panel edge — matches the during-scan view.
    let available = ui.available_size();
    let (outer_rect, response) = ui.allocate_exact_size(available, egui::Sense::click());
    let outer_painter = ui.painter_at(outer_rect);
    outer_painter.rect_filled(outer_rect, 0.0, egui::Color32::from_rgb(8, 9, 11));
    let full_rect = outer_rect.shrink(8.0);
    let painter = ui.painter_at(full_rect);

    // ── Rebuild cache if needed (AFTER breadcrumbs so full_rect is correct) ──
    let needs_rebuild = *cache_dirty
        || cache.is_none()
        || cache.as_ref().is_some_and(|c| {
            (c.layout_size.0 - full_rect.width()).abs() > 1.0
                || (c.layout_size.1 - full_rect.height()).abs() > 1.0
        });
    if needs_rebuild {
        *cache = Some(build_treemap_cache(
            root,
            zoom_path,
            category_filter,
            show_hidden,
            full_rect,
        ));
        *cache_dirty = false;
    }
    let cache = cache.as_ref().unwrap();

    // Background
    painter.rect_filled(full_rect, 0.0, ui.visuals().extreme_bg_color);

    if cache.tiles.is_empty() && cache.other.is_none() {
        painter.text(
            full_rect.center(),
            egui::Align2::CENTER_CENTER,
            "Empty directory",
            egui::FontId::proportional(16.0),
            ui.visuals().text_color(),
        );
        return actions;
    }

    let hover_pos = response.hover_pos();
    let mut hovered_tile: Option<usize> = None;
    let mut hovered_other = false;

    // Pre-create font IDs once (FontId::proportional allocates a String each call)
    let font_leaf = egui::FontId::proportional(11.0);
    let font_dir_header = egui::FontId::proportional(13.0);
    let font_nested = egui::FontId::proportional(10.0);
    let has_focus = focused_path.is_some();

    // Paint tiles
    for (idx, tile) in cache.tiles.iter().enumerate() {
        let is_focused = has_focus && focused_path.as_ref().is_some_and(|fp| *fp == tile.path);

        if tile.is_dir {
            paint_cached_directory(
                &painter,
                tile,
                is_focused,
                if has_focus { focused_path } else { &None },
                alpha,
                &font_dir_header,
                &font_nested,
            );
        } else {
            paint_cached_leaf(&painter, tile, is_focused, alpha, &font_leaf);
        }

        if let Some(pos) = hover_pos
            && tile.rect.contains(pos)
        {
            hovered_tile = Some(idx);
        }
    }

    // Paint Other bucket
    if let Some(ref other) = cache.other {
        paint_other_bucket(&painter, other, alpha, &font_leaf);
        if let Some(pos) = hover_pos
            && other.rect.contains(pos)
        {
            hovered_other = true;
        }
    }

    // Hover stroke — visible 2-px white inner outline + soft outer
    // halo on the tile being pointed at.  Matches the during-scan
    // hover treatment.
    if let Some(idx) = hovered_tile {
        let r = cache.tiles[idx].rect;
        painter.rect_stroke(
            r.shrink(1.0),
            2.0,
            egui::Stroke::new(2.0, egui::Color32::WHITE),
            egui::epaint::StrokeKind::Inside,
        );
        painter.rect_stroke(
            r,
            2.0,
            egui::Stroke::new(
                1.0,
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 80),
            ),
            egui::epaint::StrokeKind::Outside,
        );
    }
    if hovered_other && let Some(ref other) = cache.other {
        let r = other.rect;
        painter.rect_stroke(
            r.shrink(1.0),
            2.0,
            egui::Stroke::new(2.0, egui::Color32::WHITE),
            egui::epaint::StrokeKind::Inside,
        );
    }

    // Hover tooltip
    if let Some(idx) = hovered_tile {
        let tile = &cache.tiles[idx];
        egui::Tooltip::always_open(
            ui.ctx().clone(),
            ui.layer_id(),
            ui.id().with("treemap_tip"),
            egui::PopupAnchor::Pointer,
        )
        .gap(12.0)
        .show(|ui| {
            ui.label(egui::RichText::new(tile.name.as_ref()).strong());
            ui.label(ByteSize::b(tile.size).to_string());
            if let Some(count) = tile.child_count {
                ui.label(format!("{} items", count));
            }
            ui.label(tile.path.display().to_string());
        });
    } else if hovered_other && let Some(ref other) = cache.other {
        egui::Tooltip::always_open(
            ui.ctx().clone(),
            ui.layer_id(),
            ui.id().with("treemap_tip"),
            egui::PopupAnchor::Pointer,
        )
        .gap(12.0)
        .show(|ui| {
            ui.label(egui::RichText::new(other.label_short.as_ref()).strong());
            ui.label(ByteSize::b(other.size).to_string());
            ui.label("Small files collapsed into one block");
        });
    }

    // Handle click — reuse hovered_tile instead of re-scanning
    if response.clicked()
        && let Some(idx) = hovered_tile
    {
        let tile = &cache.tiles[idx];
        if tile.is_dir {
            actions.push(TreemapAction::ZoomTo(tile.path.clone()));
        }
        actions.push(TreemapAction::Focus(tile.path.clone()));
    }

    actions
}

/// Lay out `text` only if its measured width fits `max_w` — returns
/// the laid-out galley or None.  Lets callers avoid drawing labels
/// that would visually overlap their tile bounds.
pub fn fit_text_exact(
    painter: &egui::Painter,
    text: &str,
    font_id: egui::FontId,
    max_w: f32,
) -> Option<std::sync::Arc<egui::Galley>> {
    let g = painter.layout_no_wrap(text.to_string(), font_id, egui::Color32::WHITE);
    if g.size().x <= max_w { Some(g) } else { None }
}

/// Lay out `text` so it fits `max_w`, head-truncating with an ellipsis
/// if needed via binary search on the measured width.  Returns None if
/// even one character + ellipsis doesn't fit.
pub fn fit_text(
    painter: &egui::Painter,
    text: &str,
    font_id: egui::FontId,
    max_w: f32,
) -> Option<std::sync::Arc<egui::Galley>> {
    if let Some(g) = fit_text_exact(painter, text, font_id.clone(), max_w) {
        return Some(g);
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let mut lo = 0usize;
    let mut hi = chars.len();
    let mut best: Option<std::sync::Arc<egui::Galley>> = None;
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate: String = chars.iter().take(mid).collect::<String>() + "…";
        if let Some(g) = fit_text_exact(painter, &candidate, font_id.clone(), max_w) {
            best = Some(g);
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    best
}

/// Lay out a path so the leaf component stays visible: drops leading
/// components in favour of "…/" until the result fits `max_w`.  Falls
/// back to head-truncating the leaf alone when even ".../leaf" is too
/// wide.
pub fn fit_path(
    painter: &egui::Painter,
    path: &str,
    font_id: egui::FontId,
    max_w: f32,
) -> Option<std::sync::Arc<egui::Galley>> {
    if let Some(g) = fit_text_exact(painter, path, font_id.clone(), max_w) {
        return Some(g);
    }
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    for skip in 1..parts.len() {
        let candidate = format!("…/{}", parts[skip..].join("/"));
        if let Some(g) = fit_text_exact(painter, &candidate, font_id.clone(), max_w) {
            return Some(g);
        }
    }
    fit_text(painter, parts.last().copied().unwrap_or(path), font_id, max_w)
}

/// Format `bytes` into a short label suitable for cramped treemap
/// tiles.  Always one digit + optional one-decimal + unit, e.g.
/// `"1.5 GB"`, `"562 MB"`, `"4.4 GB"`, `"512 KB"`.  Significantly
/// shorter than `bytesize::ByteSize::b().to_string()` ("1.5 GiB" with
/// space + 3-letter unit).  Length is bounded so callers don't need
/// to ellipsis-truncate it for typical tiles.
pub fn fmt_size_compact(bytes: u64) -> String {
    let b = bytes as f64;
    if b < 1024.0 {
        return format!("{} B", bytes);
    }
    if b < 1024.0 * 1024.0 {
        let kb = b / 1024.0;
        return if kb < 10.0 {
            format!("{:.1} KB", kb)
        } else {
            format!("{:.0} KB", kb)
        };
    }
    if b < 1024.0 * 1024.0 * 1024.0 {
        let mb = b / 1024.0 / 1024.0;
        return if mb < 10.0 {
            format!("{:.1} MB", mb)
        } else {
            format!("{:.0} MB", mb)
        };
    }
    let gb = b / 1024.0 / 1024.0 / 1024.0;
    if gb < 10.0 {
        format!("{:.1} GB", gb)
    } else {
        format!("{:.0} GB", gb)
    }
}

/// Per-rank colour for the root-view top-level tiles.  Mirrors the
/// palette used by the live-scan treemap.  Largest = index 0.
pub fn scan_rank_palette(idx: usize) -> egui::Color32 {
    const PALETTE: [egui::Color32; 8] = [
        egui::Color32::from_rgb(58, 93, 139),  // blue
        egui::Color32::from_rgb(42, 109, 77),  // green
        egui::Color32::from_rgb(122, 74, 48),  // brown
        egui::Color32::from_rgb(74, 58, 107),  // purple
        egui::Color32::from_rgb(90, 74, 58),   // tan
        egui::Color32::from_rgb(48, 90, 100),  // teal
        egui::Color32::from_rgb(110, 60, 90),  // mauve
        egui::Color32::from_rgb(86, 86, 56),   // olive
    ];
    PALETTE[idx % PALETTE.len()]
}

// ─── Painting helpers ───────────────────────────────────────────

fn apply_alpha(c: egui::Color32, alpha: f32) -> egui::Color32 {
    if alpha >= 1.0 {
        return c;
    }
    egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (c.a() as f32 * alpha) as u8)
}

/// Paint a vertical gradient (top lighter, bottom darker) using a
/// 2-triangle vertex-coloured Mesh.  Same look as the live-scan
/// treemap so the two views are visually consistent.
fn paint_gradient_rect(
    painter: &egui::Painter,
    rect: egui::Rect,
    base: egui::Color32,
    radius: f32,
) {
    let lift = |c: u8, by: f32| -> u8 {
        ((c as f32) + (255.0 - c as f32) * by).clamp(0.0, 255.0) as u8
    };
    let dim = |c: u8, by: f32| -> u8 {
        ((c as f32) * (1.0 - by)).clamp(0.0, 255.0) as u8
    };
    let top = egui::Color32::from_rgba_unmultiplied(
        lift(base.r(), 0.18),
        lift(base.g(), 0.18),
        lift(base.b(), 0.18),
        base.a(),
    );
    let bot = egui::Color32::from_rgba_unmultiplied(
        dim(base.r(), 0.12),
        dim(base.g(), 0.12),
        dim(base.b(), 0.12),
        base.a(),
    );
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bot);
    mesh.colored_vertex(rect.left_bottom(), bot);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
    if radius > 0.0 {
        painter.rect_stroke(
            rect,
            radius,
            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 30)),
            egui::epaint::StrokeKind::Inside,
        );
    }
}

fn paint_cached_leaf(
    painter: &egui::Painter,
    tile: &TreemapTile,
    is_focused: bool,
    alpha: f32,
    _font: &egui::FontId,
) {
    let color = apply_alpha(tile.color, alpha);
    paint_gradient_rect(painter, tile.rect, color, 2.0);

    if is_focused {
        painter.rect_stroke(
            tile.rect,
            2.0,
            egui::Stroke::new(2.0, egui::Color32::WHITE),
            egui::StrokeKind::Inside,
        );
    }

    // Skip labels on tiny leaves — same threshold as dir tiles so the
    // two views read consistently.
    if tile.rect.width() < 85.0 || tile.rect.height() < 24.0 {
        return;
    }

    // Header band: same darkened-base + name-left + size-right
    // treatment used by dir tiles.  Files don't have nested children
    // backdropping the band, but applying it anyway keeps every tile
    // in the treemap visually consistent — the eye reads a tile as
    // "header on top + body below" regardless of dir vs file.
    let header_h = if tile.rect.height() > 80.0 { 28.0 } else { 22.0 };
    let header_rect =
        egui::Rect::from_min_size(tile.rect.min, egui::vec2(tile.rect.width(), header_h));
    let body = apply_alpha(tile.color, alpha);
    let band = body.linear_multiply(0.55);
    let band = egui::Color32::from_rgba_unmultiplied(
        band.r(),
        band.g(),
        band.b(),
        (band.a() as f32 * 0.92).clamp(0.0, 255.0) as u8,
    );
    painter.rect_filled(
        header_rect,
        egui::CornerRadius { nw: 2, ne: 2, sw: 0, se: 0 },
        band,
    );

    let header_pad = 8.0_f32;
    let avail = (tile.rect.width() - header_pad * 2.0).max(0.0);
    let size_str = fmt_size_compact(tile.size);
    let size_g = fit_text(
        painter,
        &size_str,
        egui::FontId::proportional(12.0),
        avail,
    );
    let size_w = size_g.as_ref().map(|g| g.size().x + 12.0).unwrap_or(0.0);
    let name_avail = (avail - size_w).max(0.0);
    let name_g = fit_text(
        painter,
        tile.name.as_ref(),
        egui::FontId::proportional(13.5),
        name_avail,
    );
    let mid_y = header_rect.center().y;
    if let Some(ng) = name_g {
        let pos = egui::pos2(
            header_rect.min.x + header_pad,
            mid_y - ng.size().y * 0.5,
        );
        painter.galley(pos, ng, egui::Color32::WHITE);
    }
    if let Some(sg) = size_g {
        let pos = egui::pos2(
            header_rect.max.x - header_pad - sg.size().x,
            mid_y - sg.size().y * 0.5,
        );
        painter.galley(
            pos,
            sg,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 230),
        );
    }
}

fn paint_other_bucket(
    painter: &egui::Painter,
    other: &OtherBucket,
    alpha: f32,
    _font: &egui::FontId,
) {
    let rect = other.rect;
    let bg = apply_alpha(egui::Color32::from_rgb(80, 80, 80), alpha);
    paint_gradient_rect(painter, rect, bg, 2.0);
    painter.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(
            1.0,
            apply_alpha(egui::Color32::from_rgb(120, 120, 120), alpha),
        ),
        egui::StrokeKind::Inside,
    );

    if rect.width() <= 50.0 || rect.height() <= 18.0 {
        return;
    }
    let pad = 4.0;
    let avail_w = rect.width() - pad * 2.0;
    let inner = rect.shrink(pad);
    let text_color = apply_alpha(egui::Color32::from_rgb(220, 220, 220), alpha);
    if let Some(g) = fit_text(
        painter,
        &other.label_short,
        egui::FontId::proportional(11.0),
        avail_w,
    ) {
        painter.galley(inner.left_top(), g, text_color);
    }
    if rect.height() > 36.0 {
        let size_str = fmt_size_compact(other.size);
        if let Some(g) = fit_text(
            painter,
            &size_str,
            egui::FontId::monospace(11.0),
            avail_w,
        ) {
            painter.galley(
                inner.left_bottom() + egui::vec2(0.0, -14.0),
                g,
                text_color,
            );
        }
    }
}

fn paint_cached_directory(
    painter: &egui::Painter,
    tile: &TreemapTile,
    is_focused: bool,
    focused_path: &Option<PathBuf>,
    alpha: f32,
    _font_header: &egui::FontId,
    _font_nested: &egui::FontId,
) {
    let rect = tile.rect;
    let bg = apply_alpha(tile.color, alpha);

    // Gradient background.
    paint_gradient_rect(painter, rect, bg, 2.0);

    if is_focused {
        painter.rect_stroke(
            rect,
            2.0,
            egui::Stroke::new(2.0, egui::Color32::WHITE),
            egui::StrokeKind::Inside,
        );
    }

    // ── Nested children first, so the header band paints on top and
    //    stays readable.
    let tile_painter = painter.with_clip_rect(rect);
    let has_focus = focused_path.is_some();
    for nested in &tile.nested {
        let cr = nested.rect;
        let color = apply_alpha(nested.color, alpha);
        paint_gradient_rect(&tile_painter, cr, color, 1.0);
        // Hairline stroke around the nested rect so the eye can tell
        // sibling tiles apart even when their gradients are similar
        // (target inside disk-cleaner, MoneyPrinterTurbo inside
        // yt-revenue, etc.).  Use a very low-alpha black so it reads
        // as a soft separator, not a hard outline.
        tile_painter.rect_stroke(
            cr,
            1.0,
            egui::Stroke::new(
                1.0,
                egui::Color32::from_rgba_unmultiplied(0, 0, 0, 60),
            ),
            egui::epaint::StrokeKind::Inside,
        );

        if has_focus {
            let child_focused = focused_path.as_ref().is_some_and(|fp| *fp == nested.path);
            if child_focused {
                tile_painter.rect_stroke(
                    cr,
                    1.0,
                    egui::Stroke::new(2.0, egui::Color32::WHITE),
                    egui::StrokeKind::Inside,
                );
            }
        }

        // Path-aware label + size — exactly the same layout the
        // during-scan view paints for its nested tiles.  Tiles
        // smaller than ~70px wide just stay empty rather than
        // showing "1..." or "12..." gibberish.
        if cr.width() > 70.0 && cr.height() > 22.0 {
            let inner = cr.shrink(3.0);
            let avail_w = inner.width();
            let display = nested
                .path
                .strip_prefix(&tile.path)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| {
                    nested
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| nested.path.display().to_string())
                });
            let text_color =
                apply_alpha(text_color_for_bg(nested.color), alpha * 0.95);
            // Name top-left.
            if let Some(g) = fit_path(
                &tile_painter,
                &display,
                egui::FontId::proportional(11.0),
                avail_w,
            ) {
                tile_painter.galley(inner.left_top(), g, text_color);
            }
            // Size bottom-left.  Suppress when the nested tile is
            // ≥95 % of the parent's size — header already shows the
            // same number, so painting it again is visual noise.
            let near_full = nested.size as f64 >= tile.size as f64 * 0.95;
            if cr.height() > 36.0 && !near_full {
                let size_str = fmt_size_compact(nested.size);
                if let Some(g) = fit_text(
                    &tile_painter,
                    &size_str,
                    egui::FontId::monospace(11.0),
                    avail_w,
                ) {
                    tile_painter.galley(
                        inner.left_bottom() + egui::vec2(0.0, -14.0),
                        g,
                        text_color,
                    );
                }
            }
        }
    }

    // ── Header band: only draw when the tile is large enough for
    //    *something readable* to fit.  Without this gate, narrow
    //    repo tiles in the bottom-right grid render with truncated
    //    "1...", "2..." gibberish.  85 px is roughly enough room
    //    for a 5-char name + a 4-char size like "562 MB".
    if rect.width() < 85.0 || rect.height() < 24.0 {
        return;
    }
    // ── Header band: solid darkened backdrop + name (left) + size
    //    (right).  Use the same darkening as the during-scan view
    //    (multiply tile colour by 0.55) so the header reads as a
    //    distinct band, not a faint variant of the body.
    let header_h = if rect.height() > 80.0 { 28.0 } else { 22.0 };
    let header_rect = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), header_h));
    let body = apply_alpha(tile.color, alpha);
    let band_color = body.linear_multiply(0.55);
    let band_color = egui::Color32::from_rgba_unmultiplied(
        band_color.r(),
        band_color.g(),
        band_color.b(),
        (band_color.a() as f32 * 0.92).clamp(0.0, 255.0) as u8,
    );
    painter.rect_filled(
        header_rect,
        egui::CornerRadius { nw: 2, ne: 2, sw: 0, se: 0 },
        band_color,
    );

    let header_pad = 8.0;
    let avail = (rect.width() - header_pad * 2.0).max(0.0);
    let size_str = fmt_size_compact(tile.size);
    let size_g = fit_text(
        painter,
        &size_str,
        egui::FontId::proportional(12.0),
        avail,
    );
    let size_w = size_g.as_ref().map(|g| g.size().x + 12.0).unwrap_or(0.0);
    let name_avail = (avail - size_w).max(0.0);
    let name_g = fit_text(
        painter,
        tile.name.as_ref(),
        egui::FontId::proportional(13.5),
        name_avail,
    );
    let mid_y = header_rect.center().y;
    if let Some(ng) = name_g {
        let pos = egui::pos2(
            header_rect.min.x + header_pad,
            mid_y - ng.size().y * 0.5,
        );
        painter.galley(pos, ng, egui::Color32::WHITE);
    }
    if let Some(sg) = size_g {
        let pos = egui::pos2(
            header_rect.max.x - header_pad - sg.size().x,
            mid_y - sg.size().y * 0.5,
        );
        painter.galley(
            pos,
            sg,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 230),
        );
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

    // ── build_treemap_cache tests ──

    #[test]
    fn build_cache_basic() {
        let tree = dir(
            "root",
            vec![
                dir("big", vec![leaf("a.mp4", 500), leaf("b.rs", 200)]),
                leaf("c.txt", 300),
            ],
        );
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let cache = build_treemap_cache(&tree, &None, None, true, rect);
        assert_eq!(cache.tiles.len(), 2);
        assert!(cache.other.is_none());
        assert_eq!(cache.view_size, 1000);
        assert_eq!(cache.layout_size, (800.0, 600.0));
    }

    #[test]
    fn build_cache_with_zoom() {
        let tree = dir(
            "root",
            vec![
                dir("sub", vec![leaf("a.txt", 100), leaf("b.txt", 200)]),
                leaf("c.txt", 50),
            ],
        );
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
        let zoom = Some(std::path::PathBuf::from("root/sub"));
        let cache = build_treemap_cache(&tree, &zoom, None, true, rect);
        assert_eq!(cache.tiles.len(), 2);
        assert_eq!(cache.view_size, 300);
    }

    #[test]
    fn build_cache_other_bucket() {
        let mut children: Vec<FileNode> = vec![leaf("big.mp4", 10000)];
        for i in 0..20 {
            children.push(leaf(&format!("tiny_{i}.txt"), 1));
        }
        let tree = dir("root", children);
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let cache = build_treemap_cache(&tree, &None, None, true, rect);
        assert_eq!(cache.tiles.len(), 1);
        assert!(cache.other.is_some());
        let other = cache.other.as_ref().unwrap();
        assert_eq!(other.count, 20);
        assert_eq!(other.size, 20);
    }

    #[test]
    fn build_cache_dir_tile_has_nested() {
        let tree = dir(
            "root",
            vec![dir(
                "sub",
                vec![leaf("a.mp4", 500), leaf("b.rs", 300), leaf("c.txt", 200)],
            )],
        );
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let cache = build_treemap_cache(&tree, &None, None, true, rect);
        assert_eq!(cache.tiles.len(), 1);
        let tile = &cache.tiles[0];
        assert!(tile.is_dir);
        assert_eq!(tile.child_count, Some(3));
        assert_eq!(tile.nested.len(), 3);
    }

    #[test]
    fn build_cache_hidden_filtered() {
        let tree = dir("root", vec![leaf(".hidden", 500), leaf("visible.txt", 500)]);
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let cache = build_treemap_cache(&tree, &None, None, false, rect);
        assert_eq!(cache.tiles.len(), 1);
        assert_eq!(&*cache.tiles[0].name, "visible.txt");
    }

    #[test]
    fn build_cache_tile_colors_and_paths() {
        let tree = dir("root", vec![leaf("movie.mp4", 100)]);
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
        let cache = build_treemap_cache(&tree, &None, None, true, rect);
        let tile = &cache.tiles[0];
        assert_eq!(&*tile.name, "movie.mp4");
        assert_eq!(tile.path, std::path::PathBuf::from("root/movie.mp4"));
        // At the root view, top-level tiles use the rank palette rather
        // than extension colour so the post-scan view matches the
        // during-scan one.
        assert_eq!(tile.color, scan_rank_palette(0));
        assert!(!tile.is_dir);
        assert_eq!(tile.child_count, None);
    }

    #[test]
    fn root_view_uses_rank_palette_in_order() {
        // Three top-level entries, sorted by size desc — colours
        // should be palette[0], palette[1], palette[2].
        let tree = dir(
            "root",
            vec![
                leaf("a.bin", 300),
                leaf("b.bin", 200),
                leaf("c.bin", 100),
            ],
        );
        let rect =
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
        let cache = build_treemap_cache(&tree, &None, None, true, rect);
        // tiles preserve insertion order from the children iter, which
        // is by the original FileNode order.  The squarify input was
        // also in that order, so tile[i].color must == palette(i).
        for (i, tile) in cache.tiles.iter().enumerate() {
            assert_eq!(
                tile.color,
                scan_rank_palette(i),
                "tile {} ({}) should use rank palette",
                i,
                tile.name
            );
        }
    }

    #[test]
    fn zoomed_view_uses_rank_palette() {
        // Zoomed-in tiles also get the rank palette — `extension_color`
        // returns a single dark grey for every directory, so a
        // zoomed-in dir-of-dirs would otherwise read as identical
        // tiles.
        let tree = dir(
            "root",
            vec![dir("sub", vec![leaf("video.mp4", 300)])],
        );
        let rect =
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
        let zoomed = Some(std::path::PathBuf::from("root/sub"));
        let cache = build_treemap_cache(&tree, &zoomed, None, true, rect);
        assert_eq!(cache.tiles.len(), 1);
        assert_eq!(cache.tiles[0].color, scan_rank_palette(0));
    }
}
