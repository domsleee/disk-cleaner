# Treemap Performance Optimization — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cache the treemap layout so idle frames cost near-zero computation, eliminating jank when viewing large directories.

**Architecture:** Extract current per-frame layout computation into a cached `TreemapCache` struct that is rebuilt only on dirty flag (zoom, filter, resize, tree mutation). The render function reads from cache for painting + hit-testing. A `mark_dirty()` helper on `App` replaces all 17 individual `rows_dirty = true` sites to co-set `treemap_dirty`.

**Tech Stack:** Rust, egui, existing squarify/treemap module

**Spec:** `docs/superpowers/specs/2026-03-25-treemap-performance-design.md`

---

### File Structure

| File | Responsibility | Changes |
|------|---------------|---------|
| `src/treemap.rs` | Treemap data structures, layout, rendering | Add cache structs + `build_treemap_cache()` + refactor `render_treemap()` to paint from cache |
| `src/main.rs` | App state, frame loop | Add `treemap_cache`/`treemap_dirty` fields, `mark_dirty()` helper, replace all dirty sites, update render call |

---

### Task 1: Add cache data structures and `build_treemap_cache()`

**Files:**
- Modify: `src/treemap.rs`

This task adds the cache structs and the build function that extracts the current layout logic into a cacheable form. The existing `render_treemap` is NOT changed yet — that happens in Task 3.

- [ ] **Step 1: Write tests for `build_treemap_cache()`**

Add these tests after the existing test module's closing brace (inside `mod tests`), at the end of `src/treemap.rs`:

```rust
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
    let cache = build_treemap_cache(
        &tree,
        &None,       // no zoom
        None,        // no category filter
        true,        // show_hidden
        rect,
    );
    // Two top-level children (big dir + c.txt), no "other" bucket
    assert_eq!(cache.tiles.len(), 2);
    assert!(cache.other.is_none());
    assert_eq!(cache.view_size, 1000);
    assert_eq!(cache.layout_size, (800.0, 600.0));
    // Breadcrumbs should be just root
    assert_eq!(cache.breadcrumbs.len(), 1);
    assert_eq!(cache.breadcrumbs[0].0, "root");
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
    // Viewing "sub" which has 2 children
    assert_eq!(cache.tiles.len(), 2);
    assert_eq!(cache.view_size, 300);
    // Breadcrumbs: root > sub
    assert_eq!(cache.breadcrumbs.len(), 2);
    assert_eq!(cache.breadcrumbs[1].0, "sub");
}

#[test]
fn build_cache_other_bucket() {
    // Create tree where many small files should collapse into "Other"
    let mut children: Vec<FileNode> = vec![leaf("big.mp4", 10000)];
    for i in 0..20 {
        children.push(leaf(&format!("tiny_{i}.txt"), 1)); // 0.01% each, well below 0.5%
    }
    let tree = dir("root", children);
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
    let cache = build_treemap_cache(&tree, &None, None, true, rect);
    // big.mp4 should be a tile, tiny files should be "Other"
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
    // Nested children should be populated (rect is large enough)
    assert_eq!(tile.nested.len(), 3);
}

#[test]
fn build_cache_hidden_filtered() {
    let tree = dir(
        "root",
        vec![
            leaf(".hidden", 500),
            leaf("visible.txt", 500),
        ],
    );
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
    // show_hidden = false
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
    assert_eq!(tile.color, extension_color("movie.mp4", false));
    assert!(!tile.is_dir);
    assert_eq!(tile.child_count, None);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib treemap::tests -- build_cache 2>&1`
Expected: FAIL — `build_treemap_cache` not found, `TreemapCache`/`TreemapTile` etc. not defined

- [ ] **Step 3: Add the cache structs**

Add these structs just above the `// ─── Treemap actions ──` section (before line 268) in `src/treemap.rs`:

```rust
// ─── Cached treemap layout ─────────────────────────────────────

/// Cached treemap layout — rebuilt only when dirty.
pub struct TreemapCache {
    /// Top-level tiles (real children only; "Other" bucket is separate).
    pub tiles: Vec<TreemapTile>,
    /// Collapsed small-file bucket, if any.
    pub other: Option<OtherBucket>,
    /// Breadcrumb trail for the current zoom level.
    pub breadcrumbs: Vec<(String, PathBuf)>,
    /// Total size of the viewed node (for display).
    pub view_size: u64,
    /// Layout dimensions used to compute rects — for resize detection.
    pub layout_size: (f32, f32),
}

/// A single top-level tile in the treemap.
pub struct TreemapTile {
    pub rect: egui::Rect,
    pub path: PathBuf,
    pub name: Box<str>,
    pub size: u64,
    pub is_dir: bool,
    pub color: egui::Color32,
    /// Number of direct children (for directory tooltip). None for files.
    pub child_count: Option<usize>,
    /// Pre-computed nested child rects for directory tiles.
    /// Empty for file tiles or directories too small to show children.
    pub nested: Vec<NestedTile>,
}

/// A nested child rect inside a directory tile.
pub struct NestedTile {
    pub rect: egui::Rect,
    pub path: PathBuf,
    pub name: Box<str>,
    pub is_dir: bool,
    pub color: egui::Color32,
}

/// The "Other (N files)" collapsed bucket.
pub struct OtherBucket {
    pub rect: egui::Rect,
    pub count: usize,
    pub size: u64,
}
```

- [ ] **Step 4: Implement `build_treemap_cache()`**

Add this function after the cache struct definitions, before the `// ─── Treemap actions ──` section. This extracts the layout logic currently in `render_treemap()` lines 296–441 and `paint_directory()` lines 667–690 into a pure function that returns `TreemapCache`.

```rust
/// Build the cached treemap layout. Called once on dirty, not every frame.
pub fn build_treemap_cache(
    root: &FileNode,
    zoom_path: &Option<PathBuf>,
    category_filter: Option<crate::categories::FileCategory>,
    show_hidden: bool,
    full_rect: egui::Rect,
) -> TreemapCache {
    let root_path = PathBuf::from(root.name());

    // Resolve view node
    let (view_node, view_path) = if let Some(ref zp) = zoom_path {
        match find_node(root, zp) {
            Some(n) => (n, zp.clone()),
            None => (root, root_path.clone()),
        }
    } else {
        (root, root_path.clone())
    };

    // Breadcrumbs
    let breadcrumbs = zoom_path
        .as_ref()
        .map(|p| breadcrumbs(root, p))
        .unwrap_or_else(|| vec![(root.name().to_string(), root_path.clone())]);

    let view_size = view_node.size();

    if view_node.children().is_empty() {
        return TreemapCache {
            tiles: Vec::new(),
            other: None,
            breadcrumbs,
            view_size,
            layout_size: (full_rect.width(), full_rect.height()),
        };
    }

    // Filter children
    let all_children: Vec<&FileNode> = view_node
        .children()
        .iter()
        .filter(|c| c.size() > 0)
        .filter(|c| show_hidden || !c.name().starts_with('.'))
        .filter(|c| {
            category_filter
                .is_none_or(|cat| crate::categories::node_matches_category(c, cat))
        })
        .collect();

    if all_children.is_empty() {
        return TreemapCache {
            tiles: Vec::new(),
            other: None,
            breadcrumbs,
            view_size,
            layout_size: (full_rect.width(), full_rect.height()),
        };
    }

    // Collapse tiny files into "Other" bucket (< 0.5% of total)
    let total_size: u64 = all_children.iter().map(|c| c.size()).sum();
    let threshold = (total_size as f64 * 0.005) as u64;
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

    // Hard cap
    if children.len() > MAX_VISIBLE_ENTRIES {
        children.sort_by_key(|c| std::cmp::Reverse(c.size()));
        for c in children.drain(MAX_VISIBLE_ENTRIES..) {
            other_size += c.size();
            other_count += 1;
        }
    }

    let has_other = other_count > 0 && other_size > 0;
    let entry_count = children.len() + if has_other { 1 } else { 0 };

    // Squarify top-level
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

    // Build tiles
    let mut tiles = Vec::with_capacity(children.len());
    for (i, child) in children.iter().enumerate() {
        let r = rects[i].shrink(GAP);
        let child_path = view_path.join(child.name());
        let color = extension_color(child.name(), child.is_dir());
        let child_count = if child.is_dir() {
            Some(child.children().len())
        } else {
            None
        };

        // Build nested children for directory tiles
        let nested = if child.is_dir()
            && r.width() > 24.0
            && r.height() > DIR_HEADER_H + 12.0
        {
            build_nested_tiles(child, &child_path, r)
        } else {
            Vec::new()
        };

        tiles.push(TreemapTile {
            rect: r,
            path: child_path,
            name: child.name().into(),
            size: child.size(),
            is_dir: child.is_dir(),
            color,
            child_count,
            nested,
        });
    }

    // "Other" bucket
    let other = if has_other && entry_count > 0 {
        let r = rects[children.len()].shrink(GAP);
        Some(OtherBucket {
            rect: r,
            count: other_count,
            size: other_size,
        })
    } else {
        None
    };

    TreemapCache {
        tiles,
        other,
        breadcrumbs,
        view_size,
        layout_size: (full_rect.width(), full_rect.height()),
    }
}

/// Build nested sub-tiles for a directory tile.
fn build_nested_tiles(
    node: &FileNode,
    node_path: &Path,
    parent_rect: egui::Rect,
) -> Vec<NestedTile> {
    let content_rect = egui::Rect::from_min_max(
        egui::pos2(parent_rect.min.x + 1.0, parent_rect.min.y + DIR_HEADER_H),
        egui::pos2(parent_rect.max.x - 1.0, parent_rect.max.y - 1.0),
    );

    if content_rect.width() <= 4.0 || content_rect.height() <= 4.0 || node.children().is_empty() {
        return Vec::new();
    }

    let mut nested: Vec<&FileNode> = node.children().iter().filter(|c| c.size() > 0).collect();
    if nested.is_empty() {
        return Vec::new();
    }
    if nested.len() > MAX_NESTED_CHILDREN {
        nested.sort_by_key(|c| std::cmp::Reverse(c.size()));
        nested.truncate(MAX_NESTED_CHILDREN);
    }

    let child_sizes: Vec<f64> = nested.iter().map(|c| c.size() as f64).collect();
    let child_rects = squarify(
        &child_sizes,
        content_rect.min.x,
        content_rect.min.y,
        content_rect.width(),
        content_rect.height(),
    );

    nested
        .iter()
        .enumerate()
        .filter_map(|(j, child)| {
            let cr = child_rects[j].shrink(0.5);
            if cr.width() <= 0.0 || cr.height() <= 0.0 || cr.area() < MIN_PAINT_AREA {
                return None;
            }
            Some(NestedTile {
                rect: cr,
                path: node_path.join(child.name()),
                name: child.name().into(),
                is_dir: child.is_dir(),
                color: extension_color(child.name(), child.is_dir()),
            })
        })
        .collect()
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib treemap::tests -- build_cache 2>&1`
Expected: All 6 new tests PASS

- [ ] **Step 6: Run full test suite**

Run: `cargo test 2>&1`
Expected: All existing tests still pass (we only added code, changed nothing)

- [ ] **Step 7: Commit**

```bash
git add src/treemap.rs
git commit -m "perf: add TreemapCache structs and build_treemap_cache()"
```

---

### Task 2: Add `mark_dirty()` helper and `treemap_cache`/`treemap_dirty` fields to App

**Files:**
- Modify: `src/main.rs`

This task introduces the `mark_dirty()` helper and replaces all 17 `self.rows_dirty = true` call sites. It also adds the treemap cache fields. The treemap render call is NOT changed yet — that happens in Task 3.

- [ ] **Step 1: Add fields to `App` struct**

In `src/main.rs`, find the `rows_dirty: bool,` field (around line 202) and add after it:

```rust
    /// Cached treemap layout; rebuilt when treemap_dirty.
    treemap_cache: Option<treemap::TreemapCache>,
    treemap_dirty: bool,
```

- [ ] **Step 2: Initialize new fields in `Default` impl**

Find `rows_dirty: true,` (around line 265) and add after it:

```rust
            treemap_cache: None,
            treemap_dirty: true,
```

- [ ] **Step 3: Add `mark_dirty()` helper method**

Add this method on `App`, right after the `rebuild_rows_if_dirty()` method:

```rust
    /// Mark both tree-view and treemap caches as needing rebuild.
    fn mark_dirty(&mut self) {
        self.rows_dirty = true;
        self.treemap_dirty = true;
    }
```

- [ ] **Step 4: Replace all `self.rows_dirty = true` with `self.mark_dirty()`**

Use find-and-replace across `src/main.rs`:
- Replace all occurrences of `self.rows_dirty = true` with `self.mark_dirty()`

There are 17 sites. The one exception is inside `rebuild_rows_if_dirty()` which sets `self.rows_dirty = false` — that should NOT be replaced (it's setting to `false`, not `true`).

- [ ] **Step 5: Also set `treemap_dirty` on zoom change**

Find the `TreemapAction::ZoomTo` handler (around line 1473–1476) and add `self.treemap_dirty = true;` after `self.treemap_zoom = new_zoom;`:

```rust
TreemapAction::ZoomTo(path) => {
    let is_root =
        std::path::Path::new(tree.name()) == path.as_path();
    let new_zoom = if is_root { None } else { Some(path) };
    if new_zoom != self.treemap_zoom {
        self.treemap_zoom_anim = Some(ctx.input(|i| i.time));
        self.treemap_zoom = new_zoom;
        self.treemap_dirty = true;
    }
}
```

- [ ] **Step 6: Verify it compiles and tests pass**

Run: `cargo test 2>&1`
Expected: All tests pass. No behavior change yet — treemap_cache is created but not used.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs
git commit -m "refactor: add mark_dirty() helper, treemap cache fields"
```

---

### Task 3: Refactor `render_treemap()` to paint from cache

**Files:**
- Modify: `src/treemap.rs` (refactor `render_treemap` signature and body)
- Modify: `src/main.rs` (update the call site)

This is the core task. `render_treemap()` stops computing layout and instead reads from a `TreemapCache` for painting and interaction.

- [ ] **Step 1: Change `render_treemap()` signature**

Replace the existing `render_treemap` function signature and body in `src/treemap.rs`. The new function takes a `&TreemapCache` instead of the tree and filter params:

```rust
/// Render the treemap view from a pre-computed cache. Returns user-triggered actions.
pub fn render_treemap(
    ui: &mut egui::Ui,
    cache: &TreemapCache,
    focused_path: &Option<PathBuf>,
    zoom_anim_start: Option<f64>,
) -> Vec<TreemapAction> {
    let mut actions = Vec::new();

    // ── Breadcrumb bar ──
    ui.horizontal(|ui| {
        if cache.breadcrumbs.len() > 1 {
            let parent_path = cache.breadcrumbs[cache.breadcrumbs.len() - 2].1.clone();
            if ui.button("< Back").clicked() {
                actions.push(TreemapAction::ZoomTo(parent_path));
            }
            ui.separator();
        }

        for (i, (name, path)) in cache.breadcrumbs.iter().enumerate() {
            if i > 0 {
                ui.label(">");
            }
            let label = if i == cache.breadcrumbs.len() - 1 {
                egui::RichText::new(name).strong()
            } else {
                egui::RichText::new(name)
            };
            if ui.link(label).clicked() {
                actions.push(TreemapAction::ZoomTo(path.clone()));
            }
        }
        ui.label(format!("  ({})", ByteSize::b(cache.view_size)));
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
    let mut hovered_is_other = false;

    // ── Paint tiles ──
    for (i, tile) in cache.tiles.iter().enumerate() {
        if tile.rect.width() <= 0.0 || tile.rect.height() <= 0.0 {
            continue;
        }
        if tile.rect.area() < MIN_PAINT_AREA {
            continue;
        }

        let is_focused = focused_path
            .as_ref()
            .is_some_and(|fp| *fp == tile.path);

        if tile.is_dir && !tile.nested.is_empty() {
            paint_cached_directory(&painter, tile, is_focused, focused_path, alpha);
        } else {
            paint_cached_leaf(&painter, tile, is_focused, alpha);
        }

        if let Some(pos) = hover_pos {
            if tile.rect.contains(pos) {
                hovered_tile = Some(i);
            }
        }
    }

    // ── Paint "Other" bucket ──
    if let Some(ref other) = cache.other {
        if other.rect.width() > 0.0 && other.rect.height() > 0.0 && other.rect.area() >= MIN_PAINT_AREA {
            paint_other_bucket(&painter, other.count, other.size, other.rect, alpha);
            if let Some(pos) = hover_pos {
                if other.rect.contains(pos) {
                    hovered_is_other = true;
                }
            }
        }
    }

    // ── Hover tooltip ──
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
            ui.label(egui::RichText::new(&*tile.name).strong());
            ui.label(ByteSize::b(tile.size).to_string());
            if let Some(count) = tile.child_count {
                ui.label(format!("{} items", count));
            }
            ui.label(tile.path.display().to_string());
        });
    } else if hovered_is_other {
        if let Some(ref other) = cache.other {
            egui::Tooltip::always_open(
                ui.ctx().clone(),
                ui.layer_id(),
                ui.id().with("treemap_tip"),
                egui::PopupAnchor::Pointer,
            )
            .gap(12.0)
            .show(|ui| {
                ui.label(
                    egui::RichText::new(format!("Other ({} files)", other.count)).strong(),
                );
                ui.label(ByteSize::b(other.size).to_string());
                ui.label("Small files collapsed into one block");
            });
        }
    }

    // ── Handle click ──
    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            for tile in &cache.tiles {
                if tile.rect.contains(pos) {
                    if tile.is_dir {
                        actions.push(TreemapAction::ZoomTo(tile.path.clone()));
                    }
                    actions.push(TreemapAction::Focus(tile.path.clone()));
                    break;
                }
            }
        }
    }

    actions
}
```

- [ ] **Step 2: Add `paint_cached_directory` and `paint_cached_leaf` helpers**

Add these after `render_treemap`, replacing (or alongside) the old `paint_directory` and `paint_leaf`. The old functions can be removed since nothing calls them after this refactor.

```rust
fn paint_cached_leaf(
    painter: &egui::Painter,
    tile: &TreemapTile,
    is_focused: bool,
    alpha: f32,
) {
    let color = apply_alpha(tile.color, alpha);
    painter.rect_filled(tile.rect, 2.0, color);

    if is_focused {
        painter.rect_stroke(
            tile.rect,
            2.0,
            egui::Stroke::new(2.0, egui::Color32::WHITE),
            egui::StrokeKind::Inside,
        );
    }

    if tile.rect.width() > MIN_LABEL_W && tile.rect.height() > 14.0 {
        let tc = apply_alpha(text_color_for_bg(tile.color), alpha);
        let font = egui::FontId::proportional(11.0);
        let text = if tile.rect.height() > 30.0 {
            format!("{}\n{}", tile.name, ByteSize::b(tile.size))
        } else {
            tile.name.to_string()
        };
        painter.text(tile.rect.center(), egui::Align2::CENTER_CENTER, text, font, tc);
    }
}

fn paint_cached_directory(
    painter: &egui::Painter,
    tile: &TreemapTile,
    is_focused: bool,
    focused_path: &Option<PathBuf>,
    alpha: f32,
) {
    let bg = apply_alpha(tile.color, alpha);
    let header_bg = apply_alpha(darken(tile.color, 15), alpha);

    painter.rect_filled(tile.rect, 2.0, bg);

    if is_focused {
        painter.rect_stroke(
            tile.rect,
            2.0,
            egui::Stroke::new(2.0, egui::Color32::WHITE),
            egui::StrokeKind::Inside,
        );
    }

    // Header
    let header_rect =
        egui::Rect::from_min_size(tile.rect.min, egui::vec2(tile.rect.width(), DIR_HEADER_H));
    painter.rect_filled(header_rect, 2.0, header_bg);

    let tc = apply_alpha(text_color_for_bg(darken(tile.color, 15)), alpha);
    if tile.rect.width() > MIN_LABEL_W {
        let label = format!("{} ({})", tile.name, ByteSize::b(tile.size));
        painter.text(
            header_rect.center(),
            egui::Align2::CENTER_CENTER,
            &label,
            egui::FontId::proportional(13.0),
            tc,
        );
    }

    // Nested children from cache
    for nested in &tile.nested {
        let color = apply_alpha(nested.color, alpha);
        painter.rect_filled(nested.rect, 1.0, color);

        let child_focused = focused_path
            .as_ref()
            .is_some_and(|fp| *fp == nested.path);
        if child_focused {
            painter.rect_stroke(
                nested.rect,
                1.0,
                egui::Stroke::new(2.0, egui::Color32::WHITE),
                egui::StrokeKind::Inside,
            );
        }

        if nested.rect.width() > MIN_LABEL_W && nested.rect.height() > 12.0 {
            let tc = apply_alpha(text_color_for_bg(nested.color), alpha);
            painter.text(
                nested.rect.center(),
                egui::Align2::CENTER_CENTER,
                &*nested.name,
                egui::FontId::proportional(10.0),
                tc,
            );
        }
    }
}
```

- [ ] **Step 3: Remove old `paint_leaf` and `paint_directory` functions**

Delete the old `paint_leaf` (lines 555–588) and `paint_directory` (lines 623–727) functions since they are replaced by the cached versions.

- [ ] **Step 4: Update the call site in `src/main.rs`**

Find the `ViewMode::Treemap` match arm (around line 1456) and replace it with:

```rust
ViewMode::Treemap => {
    if let Some(ref tree) = self.tree {
        // Rebuild cache if dirty or on first use
        let available = ui.available_size();
        let needs_rebuild = self.treemap_dirty
            || self.treemap_cache.is_none()
            || self.treemap_cache.as_ref().is_some_and(|c| {
                (c.layout_size.0 - available.x).abs() > 1.0
                    || (c.layout_size.1 - available.y).abs() > 1.0
            });

        if needs_rebuild {
            let rect = egui::Rect::from_min_size(
                ui.cursor().min,
                available,
            );
            self.treemap_cache = Some(treemap::build_treemap_cache(
                tree,
                &self.treemap_zoom,
                self.category_filter,
                self.show_hidden,
                rect,
            ));
            self.treemap_dirty = false;
        }

        let tm_actions = if let Some(ref cache) = self.treemap_cache {
            treemap::render_treemap(
                ui,
                cache,
                &self.focused_path,
                self.treemap_zoom_anim,
            )
        } else {
            Vec::new()
        };

        for action in tm_actions {
            match action {
                TreemapAction::ZoomTo(path) => {
                    let is_root =
                        std::path::Path::new(tree.name()) == path.as_path();
                    let new_zoom = if is_root { None } else { Some(path) };
                    if new_zoom != self.treemap_zoom {
                        self.treemap_zoom_anim = Some(ctx.input(|i| i.time));
                        self.treemap_zoom = new_zoom;
                        self.treemap_dirty = true;
                    }
                }
                TreemapAction::Focus(path) => {
                    self.focused_path = Some(path);
                }
            }
        }
    }
}
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo build 2>&1`
Expected: Compiles successfully

- [ ] **Step 6: Run full test suite**

Run: `cargo test 2>&1`
Expected: All tests pass (unit + e2e)

- [ ] **Step 7: Commit**

```bash
git add src/treemap.rs src/main.rs
git commit -m "perf: render treemap from cached layout, eliminate per-frame recomputation"
```

---

### Task 4: Clean up dead code and verify

**Files:**
- Modify: `src/treemap.rs` (remove any remaining dead code)

- [ ] **Step 1: Check for unused imports or dead code warnings**

Run: `cargo build 2>&1 | grep warning`
Expected: Fix any warnings about unused functions, imports, or variables

- [ ] **Step 2: Remove any dead code**

If old `paint_leaf`/`paint_directory` weren't removed in Task 3 Step 3, or if there are unused helper functions, remove them now.

- [ ] **Step 3: Run full test suite one final time**

Run: `cargo test 2>&1`
Expected: All tests pass

- [ ] **Step 4: Commit cleanup**

```bash
git add -A
git commit -m "refactor: remove dead treemap code after cache refactor"
```

(Skip this commit if no dead code was found.)
