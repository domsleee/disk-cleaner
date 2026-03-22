# Disk Cleaner — Plan

A native desktop app to visualize disk usage and clean up large files/directories interactively.

## Tech Stack

- **UI**: `egui` / `eframe` — immediate mode GUI, pure Rust, no webview
- **Scanning**: `walkdir` for recursive traversal; optionally shell out to `dust` for a head start
- **Trash**: `trash` crate — cross-platform safe delete
- **File sizes**: `bytesize` — human-readable formatting (KB, MB, GB)
- **Parallelism**: `rayon` — parallel directory scanning for speed

---

## Architecture

```
disk-cleaner/
├── src/
│   ├── main.rs        # eframe entry point, top-level App struct
│   ├── scanner.rs     # walkdir-based tree builder, runs in background thread
│   ├── tree.rs        # FileNode tree data structure
│   └── ui.rs          # egui rendering: tree view, toolbar, actions
├── Cargo.toml
└── IDEA.md
```

---

## Data Model

```rust
struct FileNode {
    name: String,
    path: PathBuf,
    size: u64,           // bytes, aggregated for dirs
    is_dir: bool,
    children: Vec<FileNode>,
    expanded: bool,      // UI state: is this node open
    selected: bool,      // UI state: checked for batch delete
}
```

---

## Phases

### Phase 1 — Scaffold
- [ ] `cargo new disk-cleaner` with eframe + egui
- [ ] Empty window boots, shows "Select a directory" prompt
- [ ] Directory picker (native dialog via `rfd` crate)

### Phase 2 — Scanner
- [ ] `scanner.rs`: walk a directory recursively with `walkdir`
- [ ] Build `FileNode` tree, aggregating sizes bottom-up
- [ ] Run scan in a background thread (`std::thread` + `mpsc` channel)
- [ ] Show a spinner/progress indicator while scanning

### Phase 3 — Tree UI
- [ ] Render the tree with indentation per depth level
- [ ] Sort children by size descending (largest first)
- [ ] Collapsible dirs (click to expand/collapse)
- [ ] Size bar: a horizontal bar proportional to parent size
- [ ] Color code: red = huge, orange = large, white = small

### Phase 4 — Actions
- [ ] Per-row "Trash" button — moves to system trash via `trash` crate
- [ ] Per-row "Delete" button — `std::fs::remove_dir_all` / `remove_file`, with confirmation dialog
- [ ] After delete: remove node from tree and propagate size changes up
- [ ] Checkbox per row for multi-select batch operations
- [ ] "Delete selected" / "Trash selected" toolbar buttons

### Phase 5 — Polish
- [ ] Persist last-scanned path across sessions (`dirs` crate + JSON config)
- [ ] Keyboard shortcuts: `Space` to expand, `Del` to trash, `Shift+Del` to delete
- [ ] Search/filter bar to find files by name
- [ ] Re-scan button
- [ ] Dark mode (egui default)

---

## Key Crates

| Crate      | Purpose                          |
|------------|----------------------------------|
| `eframe`   | Desktop window + event loop      |
| `egui`     | Immediate mode UI widgets        |
| `walkdir`  | Recursive directory traversal    |
| `rayon`    | Parallel scan                    |
| `trash`    | Cross-platform move-to-trash     |
| `bytesize` | Human-readable byte formatting   |
| `rfd`      | Native file/folder picker dialog |
| `dirs`     | OS config/data directories       |

---

## UX Flow

1. App opens → "Choose a directory to scan" button
2. User picks a directory → scan starts in background
3. Spinner shown during scan
4. Tree renders sorted by size, top-level dirs expanded one level
5. User browses, expands dirs, identifies large items
6. User clicks Trash or Delete on a row → confirmation for Delete
7. Node removed, parent sizes updated in-place
8. User can re-scan at any time
