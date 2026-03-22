# Disk Cleaner — Design (dust integration)

## Overview

Replace disk-cleaner's `walkdir`-based scanner with dust's parallel directory walker via `du_dust` library crate. This gives us rayon-parallelized scanning, accurate block-level sizes, inode deduplication, and configurable thread counts (e.g. `-T8`).

## Dependency

```toml
[dependencies]
du-dust = { git = "https://github.com/domsleee/dust", branch = "public-disk-cleaner" }
```

The `public-disk-cleaner` branch adds `src/lib.rs` which exposes all modules as `pub mod`. All 70 existing tests pass; `main.rs` is unchanged in behavior.

## Key dust types for disk-cleaner

| Type | Module | Purpose |
|------|--------|---------|
| `Node` | `du_dust::node` | Tree node: `name: PathBuf`, `size: u64`, `children: Vec<Node>`, `inode_device`, `depth` |
| `WalkData` | `du_dust::dir_walker` | Scan config: ignored dirs, filters, apparent size, thread progress, error tracking |
| `walk_it()` | `du_dust::dir_walker` | Entry point: takes `HashSet<PathBuf>` dirs + `&WalkData`, returns `Vec<Node>` |
| `PIndicator` | `du_dust::progress` | Progress reporter with `Arc<PAtomicInfo>` (file count + total size atomics) |
| `PAtomicInfo` | `du_dust::progress` | Atomic counters: `num_files`, `total_file_size`, `state`, `current_path` |
| `RuntimeErrors` | `du_dust::progress` | Collected errors: permission denied, not found, unknown |
| `get_metadata()` | `du_dust::platform` | Platform-specific file size + inode + timestamps (unix/windows) |

## Architecture: scanning with dust

```
disk-cleaner                              du_dust (library)
┌──────────────┐                         ┌──────────────────┐
│ App          │  build WalkData         │ dir_walker       │
│  start_scan()├────────────────────────►│  walk_it()       │
│              │                         │   └─ walk()      │
│              │  Arc<PAtomicInfo>       │      par_bridge() │
│  progress ◄──┼────────────────────────┤      rayon        │
│  display     │                         │                  │
│              │  Vec<Node>             │                  │
│  convert ◄───┼────────────────────────┤                  │
│  Node→FileNode                        └──────────────────┘
└──────────────┘
```

### Scan flow

1. Build a `rayon::ThreadPool` with desired thread count (e.g. 8 threads).
2. Construct `WalkData` with minimal config:
   - `ignore_directories: HashSet::new()` (or user-selected ignores)
   - `filter_regex: &[]`, `invert_filter_regex: &[]`
   - `allowed_filesystems: HashSet::new()`
   - `use_apparent_size: true` (show file sizes, not block allocation)
   - `by_filecount: false`
   - `by_filetime: &None`
   - `ignore_hidden: false`
   - `follow_links: false`
   - `progress_data: Arc<PAtomicInfo>` (share with UI thread for live updates)
   - `errors: Arc<Mutex<RuntimeErrors>>`
3. Call `walk_it(dirs, &walk_data)` inside `thread_pool.install(|| ...)`.
4. Convert resulting `Vec<Node>` to `FileNode` tree for egui.

### Node to FileNode conversion

```rust
fn node_to_file_node(node: &du_dust::node::Node) -> FileNode {
    let name = node.name.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| node.name.to_string_lossy().to_string());

    let mut children: Vec<FileNode> = node.children.iter()
        .map(node_to_file_node)
        .collect();
    children.sort_by(|a, b| b.size.cmp(&a.size));

    FileNode {
        name,
        path: node.name.clone(),
        size: node.size,
        is_dir: !node.children.is_empty(),
        children,
        expanded: false,
        selected: false,
    }
}
```

### Progress reporting

dust's `PAtomicInfo` has `num_files: AtomicUsize` and `total_file_size: AtomicU64`. The egui UI thread reads these atomics each frame during scanning — same pattern as current `ScanProgress` but with dust doing the incrementing internally during `walk()`.

### Thread configuration

dust uses rayon's thread pool. Configure via:
```rust
rayon::ThreadPoolBuilder::new()
    .num_threads(8)      // -T8 equivalent
    .stack_size(1 << 30) // 1GB stack for deep trees
    .build()
    .unwrap()
    .install(|| {
        let nodes = walk_it(dirs, &walk_data);
        // ...
    });
```

## Tree layout: Disk Inventory X style

Disk Inventory X uses a **treemap** (cushion treemap / squarified treemap) where:
- Each rectangle's area is proportional to its size on disk
- Directories are nested rectangles containing their children
- Color indicates depth level or file type

### Implementation plan

The current disk-cleaner UI uses an indented tree list. To add Disk Inventory X-style visualization:

1. **Keep the tree list** as the primary navigation (left panel)
2. **Add a treemap panel** (right/bottom) showing the currently expanded directory as a squarified treemap
3. **Squarified treemap algorithm**: given a list of sizes and a rectangle, recursively subdivide into proportional sub-rectangles with aspect ratios close to 1:1

Treemap rendering with egui:
```rust
fn render_treemap(ui: &mut egui::Ui, node: &FileNode, rect: egui::Rect, depth: usize) {
    if node.children.is_empty() || depth > MAX_DEPTH {
        // leaf: draw colored rect with label
        let color = depth_color(depth);
        ui.painter().rect_filled(rect, 0.0, color);
        // label if rect is big enough
        if rect.width() > 40.0 && rect.height() > 14.0 {
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, &node.name, ...);
        }
        return;
    }

    // squarified layout: sort children by size desc, then lay them out
    let rects = squarify(&node.children, rect);
    for (child, child_rect) in node.children.iter().zip(rects) {
        render_treemap(ui, child, child_rect, depth + 1);
    }
}
```

### Memory considerations

- dust's `Node` stores `name: PathBuf`, `size: u64`, `children: Vec<Node>`, `inode_device: Option<(u64, u64)>`, `depth: usize`
- For `~/git` (570k files, 73GB): the tree structure itself is modest (~50-100 MB)
- dust deduplicates inodes (hardlinks), preventing double-counting
- rayon's par_bridge parallelizes at each directory level, keeping memory proportional to tree breadth at any given depth

### Compared to current walkdir scanner

| Aspect | Current (walkdir) | With dust |
|--------|-------------------|-----------|
| Parallelism | Single thread | rayon par_bridge (configurable -T8) |
| Size accuracy | `metadata.len()` (apparent) | Block-allocated size or apparent (configurable) |
| Hardlink handling | None (double counts) | Inode dedup via `clean_inodes()` |
| Hidden files | Always included | Configurable `ignore_hidden` |
| Progress | Manual atomic increment | Built-in `PAtomicInfo` atomics |
| Regex filtering | None | Built-in filter/invert-filter |
| Cross-filesystem | Not handled | `limit_filesystem` option |

## Start page design

The start page is the first screen users see. Current layout:

```
┌─ Toolbar ──────────────────────────────────────┐
│ [Open Directory...]                            │
├────────────────────────────────────────────────┤
│              Disk Cleaner                      │
│              Volumes                           │
│  ┌──────────────────────────────────────────┐  │
│  │ Macintosh HD               460.4 GiB     │  │
│  │ ████████████████████░░░░░░               │  │
│  │ 752.7 MiB free                    [Scan] │  │
│  └──────────────────────────────────────────┘  │
│          ─────────────────                     │
│         [Open Directory...]                    │
│         [Resume last scan: /]                  │
└────────────────────────────────────────────────┘
```

### Problems

1. "Open Directory..." appears twice (toolbar + content area)
2. Volume cards have a small "Scan" button but aren't themselves clickable
3. "Resume last scan" is a low-visibility button below a separator
4. Toolbar is nearly empty on this screen

### Target design

- **Remove the duplicate "Open Directory..." from the content area.** Keep it in the toolbar only.
- **Make volume cards fully clickable.** Hover highlight, click to scan. Remove the small "Scan" button.
- **Integrate "Resume last scan"** as a subtle secondary card or link in the volumes area.
- **On start page:** toolbar shows only "Open Directory..." (disk stats live on the cards).
- **On results pages:** toolbar adds "Re-scan" + view mode tabs; disk stats move to status bar.

### Implementation

- Phase 1: Clickable cards + remove duplicate button (DIS-110)
- Phase 2: Toolbar declutter + status bar migration (DIS-111)
- Phase 3: Polish — recent scans list, drag-and-drop, keyboard hints

## Scope for initial integration

Start with `~/git` as the default scan target for fast iteration:
1. Replace `scanner::scan_directory` internals with dust's `walk_it`
2. Map `PAtomicInfo` to the existing `ScanProgress` UI
3. Keep the tree list UI unchanged
4. Add thread count as a setting (default: 8)
5. Treemap visualization as a follow-up phase
