# Treemap Performance Optimization — Design Spec

## Problem

The treemap view becomes janky when viewing directories with many children (e.g. `~/.rustup`). The root cause: **everything is recomputed every frame at 60fps** — filtering, sorting, squarify layout, PathBuf allocations, nested sub-rect layout, and 20K+ paint operations — even when nothing has changed since the last frame.

## Goal

Cache the treemap layout and only rebuild when state changes. Idle frames (hover, no interaction) should cost near-zero computation.

## Design

### New data structures

```rust
/// Cached treemap layout — rebuilt only when dirty.
pub struct TreemapCache {
    /// Top-level tiles (real children + optional "Other" bucket).
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
    /// Number of children (for directory tooltip). None for files.
    pub child_count: Option<usize>,
    /// Pre-computed nested child rects for directory tiles.
    /// Empty for file tiles or directories too small to show children.
    pub nested: Vec<NestedTile>,
}

/// A nested child inside a directory tile.
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

### Dirty flag mechanism

New fields on `App`:

```rust
treemap_cache: Option<TreemapCache>,
treemap_dirty: bool,
```

Set `treemap_dirty = true` at every mutation site that already sets `rows_dirty = true` (they share the same triggers):
- Zoom change (`treemap_zoom` updated)
- Category filter change
- Show hidden toggle
- Tree mutation (delete, scan complete)
- Search filter change (affects category sidebar which can affect treemap)

To avoid missing sites, introduce a helper: `fn mark_dirty(&mut self) { self.rows_dirty = true; self.treemap_dirty = true; }` and replace all `self.rows_dirty = true` calls with `self.mark_dirty()`.

Additionally, detect **resize** by comparing `treemap_cache.layout_size` against the current frame's available rect. If different, rebuild.

**Note:** `zoom_anim_start` changes do NOT dirty the cache — the alpha fade is applied at paint time and does not affect layout.

### Render split

`render_treemap()` is refactored into two phases:

**Phase 1 — Build (only when dirty or resized):**
Extract the current filtering, sorting, squarify, and nested layout code into a `build_treemap_cache()` function. This is called once and the result stored in `App::treemap_cache`.

**Phase 2 — Paint (every frame):**
`render_treemap()` reads from the cache:
- Paint breadcrumbs (from cached breadcrumbs)
- Iterate `tiles`, paint each rect + label
- For directory tiles, iterate `nested` and paint sub-rects
- Hit-test hover position against cached rects (simple `rect.contains(pos)`)
- Handle click by checking which cached tile was hit

Focus highlight (white border) is determined at paint time by comparing `tile.path` against current `focused_path` — no cache rebuild needed for focus changes.

### What moves out of the per-frame path

| Operation | Before | After |
|-----------|--------|-------|
| `find_node()` tree walk | Every frame | On rebuild only |
| `node_matches_category()` recursive walk | Every frame per child | On rebuild only |
| Child filtering + sorting | Every frame | On rebuild only |
| `squarify()` layout computation | Every frame | On rebuild only |
| Nested `squarify()` per directory | Every frame | On rebuild only |
| PathBuf allocation per child | Every frame | On rebuild only |
| `breadcrumbs()` tree walk | Every frame | On rebuild only |

### What remains per-frame

- Painting cached rects (just `painter.rect_filled` + `painter.text` calls with pre-computed values)
- Hover hit-testing (iterate cached rects, check `contains`)
- Click handling (same hit-test)
- Focus border check (`tile.path == focused_path`)
- Tooltip rendering for hovered tile

### File changes

- **`src/treemap.rs`**: Add cache structs, `build_treemap_cache()` function, refactor `render_treemap()` to paint from cache. Existing `squarify()`, `find_node()`, `breadcrumbs()` functions unchanged.
- **`src/main.rs`**: Add `treemap_cache` and `treemap_dirty` fields to `App`. Set `treemap_dirty = true` alongside existing `rows_dirty = true` sites. Pass cache to `render_treemap()`.

### Constants (unchanged)

- `MAX_VISIBLE_ENTRIES = 200`
- `MAX_NESTED_CHILDREN = 100`
- `MIN_PAINT_AREA = 4.0`

These caps are already effective. The optimization is about not recomputing them every frame, not reducing them.

## Non-goals

- Changing the treemap visual appearance
- Adding new treemap features
- Optimizing the squarify algorithm itself (it's already O(N))
- Virtualizing the treemap (it's spatially bounded by the viewport by definition)

## Testing

- Existing treemap unit tests (squarify, find_node, breadcrumbs) remain unchanged
- E2E tests that exercise the treemap view should still pass
- Manual verification: load `~/.rustup`, zoom in, confirm smooth hover/interaction
