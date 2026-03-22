# UI Audit — Disk Cleaner

Date: 2026-03-17

## Screenshots

Existing screenshots in `docs/`:

| Screenshot | View | Notes |
|---|---|---|
| `screenshot.png` | Home / Volume Selection | Shows volume card, "Open Directory", "Resume last scan" |
| `screenshot_scan.png` | Tree View (post-scan) | Shows tree with categories sidebar, toolbar, file icons |

**Missing screenshots:** Scanning (progress) view, Treemap view, Context menu, Delete confirmation dialogs. Window-level screencapture is blocked by macOS wgpu/OpenGL rendering (DIS-9). Full-screen capture is possible but includes surrounding desktop.

## Current UI Pages

1. **Home screen** — Volume cards with capacity bars, Open Directory button, Resume last scan
2. **Scanning screen** — Centered spinner, path label, file count + size, Cancel button
3. **Tree view** — Indented file tree with icons, size bars, toolbar, optional File Types sidebar
4. **Treemap view** — Squarified treemap with breadcrumb navigation, color-coded by file type
5. **Context menu** — Right-click: Open in Finder, Copy Path, Move to Trash, Delete Permanently
6. **Delete confirmation dialogs** — Single-item and batch permanent delete modals

## Improvement Recommendations

### Priority 1 — High Impact

**1.1 — Home screen looks empty and unpolished**
The home screen has a lot of dead space. The volume card is functional but plain — a single white-border rectangle with a red bar and a tiny "Scan" button. Compare to DaisyDisk which makes the home screen inviting with large drive icons and a clear visual hierarchy.

Suggestions:
- Add a disk/drive icon next to volume name
- Make the entire volume card clickable (not just the "Scan" button)
- Use a more prominent CTA for scanning — the "Scan" button is small and easy to miss
- Add visual indication of disk health (color gradient already exists for >70%/>90% thresholds — good)
- Remove duplicate "Open Directory..." button (appears both in toolbar AND center content)
- Consider adding a brief tagline or empty-state illustration

**1.2 — Toolbar is overloaded**
The toolbar crams everything into a single horizontal bar: Open Directory, Re-scan, Tree/Treemap tabs, Filter input, Show hidden toggle, File Types toggle, batch operations, AND disk usage stats. On narrower windows, elements will clip or wrap.

Suggestions:
- Move disk usage stats to the status bar (bottom) which already shows scan info
- Consider a dedicated filter bar below the toolbar, or make the filter a popup/dropdown
- Group related controls: navigation (Tree/Treemap), actions (Re-scan, Open), filters (search, hidden, categories)
- The "Trash Selected" and "Delete Selected" buttons appearing inline is jarring — consider a floating action bar or a dedicated selection toolbar that slides in

**1.3 — Tree view row interaction is ambiguous**
The entire row is clickable (good) but the interaction model is confusing: clicking the disclosure triangle toggles expand, clicking elsewhere selects. There's no visual feedback differentiating these zones. Users won't know that shift-click multi-selects, or that the disclosure triangle area has special behavior.

Suggestions:
- Add hover state on disclosure triangle (slight highlight/scale)
- Show a tooltip or visual hint for multi-select (Shift+Click)
- Consider making single-click expand directories (like Finder) and double-click to drill in, rather than the current disclosure-triangle-only expand model
- The cursor is `PointingHand` on all rows — this suggests "link" behavior. Consider using default cursor for selection and `PointingHand` only on the disclosure triangle

**1.4 — No undo for destructive operations**
Move to Trash is good (reversible), but "Delete Permanently" is one click away in the context menu. The confirmation dialog exists but is minimal — just text and two buttons.

Suggestions:
- Add the file size to the delete confirmation ("Permanently delete 1.2 GiB?")
- For batch operations, list the items being deleted (or at least a preview of the first few)
- Consider adding an "Undo" capability for Trash operations (could show a toast: "Moved to Trash. Undo?")

### Priority 2 — Medium Impact

**2.1 — Category sidebar layout is inefficient**
The File Types sidebar shows categories with swatches, bars, and stats — but each category takes up ~4 lines of vertical space (label + bar + stats + spacing). With 7 categories, that fills the panel. The percentage bars are thin (4px) and hard to read.

Suggestions:
- Consolidate: put the size + file count on the same line as the category name
- Make bars thicker (8-10px) for better visibility
- Consider a donut/pie chart summary at the top, with the list below
- The "All files" button should be more prominent — it's currently an underlined selectable label

**2.2 — Treemap could use more interactivity**
The treemap is functional (squarified layout, breadcrumbs, color-coded, zoom on click) but lacks some polish:

Suggestions:
- Add a "zoom out" / "back" button (not just breadcrumb clicks)
- Show a minimap or overview when zoomed into a subdirectory
- Add keyboard navigation in treemap (arrow keys to move between blocks)
- The nested directory headers (16px) are quite small — consider increasing or adding folder icons
- Consider showing file size labels on hover for small blocks that can't fit text

**2.3 — Scanning screen has no progress indicator**
The scanning view shows a spinner, path, and running file count/size — but no estimate of completion. For root scans, this can take a while.

Suggestions:
- If disk total size is known, show a progress bar (scanned size / total disk size)
- Show elapsed time
- Show scan rate (files/sec) for power users
- The "Cancel" button is small and centered — make it more prominent

**2.4 — Status bar is underutilized**
The bottom status bar only shows scan path + file count + total size on the left, and version on the right. It could carry more useful information.

Suggestions:
- Show currently focused/selected item path
- Show selection count when items are selected
- Move disk usage stats here (from toolbar)
- Show keyboard shortcut hints contextually

### Priority 3 — Polish

**3.1 — Color consistency between tree and treemap**
The tree view uses `bar_color()` (blue gradient based on size) for directories and category colors for files. The treemap uses `extension_color()` which has a separate (more granular) color mapping. These don't fully align — e.g., Code category is teal in both, but the treemap has separate colors for source code vs. config vs. web vs. build artifacts.

Suggestion: Unify or at least harmonize the color systems. The category sidebar colors should match what the user sees in the treemap.

**3.2 — Monospace font for all text is utilitarian**
File sizes use monospace (good for alignment). File names also use monospace, which makes the tree look like a terminal. Consider proportional font for names and monospace only for sizes.

**3.3 — No keyboard shortcut discoverability**
The app supports arrow keys (navigate), Space (toggle expand), Delete (trash), Shift+Delete (permanent delete), Shift/Cmd+Click (multi-select). None of these are documented in the UI.

Suggestion: Add a help overlay (? key) or a menu bar with keyboard shortcuts listed.

**3.4 — Window title is generic**
The window title bar says "Disk Cleaner". After scanning, it could show the scan path: "Disk Cleaner — /Users/dom".

**3.5 — No dark/light mode toggle**
The app uses egui's dark theme by default with no way to switch. Some users prefer light mode.

**3.6 — Right-click context menu could show more info**
The context menu shows 4 actions but could also show: file size, file type, last modified date, or provide quick access to "Select all in category" or "Expand all children".

## Summary of Top 5 Improvements

1. **Make the home screen inviting** — clickable volume cards, remove duplicate buttons, add visual polish
2. **Declutter the toolbar** — move stats to status bar, group controls logically, conditional show of batch actions
3. **Add progress estimation to scanning** — progress bar using known disk size, elapsed time
4. **Improve keyboard shortcut discoverability** — help overlay or menu hints
5. **Harmonize color systems** — align tree bar colors, treemap colors, and category sidebar colors
