# Changelog

All notable changes to this project will be documented in this file.

## [0.1.0.0] - 2026-03-23

### Added
- Smart Cleanup Suggestions view with 6 categories: build artifacts, package caches, system caches, IDE artifacts, temp/log files, and old installers
- Safety ratings (Safe / Caution) with color-coded badges and per-category descriptions
- Treemap visualization with squarified layout, breadcrumb navigation, and extension-based coloring
- Multi-select with shift+click range selection in tree view
- Batch delete and batch trash with background threading and progress bar
- File category filtering (Video, Image, Audio, Document, Archive, Code)
- Search/filter in tree view
- Volume picker home screen with clickable volume cards
- Scanning progress screen with file count, elapsed time, and cancel button
- Screenshot mode (`--screenshot <prefix>`) for programmatic UI captures
- Disk usage stats in status bar with selection count and keyboard hints
- Context menu with right-click support (multi-select aware)
- Auto-expand of large directories after scan
- Hidden files toggle (persisted in config)
- Performance benchmarks (tree operations, startup time, frame-time profiling)

### Fixed
- APFS double-counting when scanning root volume on macOS
- Treemap rendering for directories with many children (batching into "Other" bucket)
- Selection state cleared on rescan and after deletion
- Disk usage stats refresh after file deletion
- Consistent toolbar widget height and proportional fonts
- Unicode rendering issues in keyboard hints and close buttons
- Multi-select row gap elimination
