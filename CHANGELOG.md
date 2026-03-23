# Changelog

All notable changes to this project will be documented in this file.

## [0.1.2.0] - 2026-03-23

### Added
- Cross-platform support: macOS, Linux, and Windows from a single codebase
- Platform module (`src/platform/`) with `macos.rs`, `linux.rs`, `windows.rs` implementations
- Linux volume detection via `/proc/mounts` parsing with virtual filesystem denylist
- Windows volume enumeration via `GetLogicalDrives` and `GetVolumeInformationW`
- Windows disk space via `GetDiskFreeSpaceExW`
- NTFS reparse point (junction/symlink) skipping to prevent double-counting on Windows
- Linux skip set for `/proc`, `/sys`, `/dev`, `/run`, `/snap` when scanning from root
- Windows skip set for `$Recycle.Bin` and `System Volume Information` on drive roots
- Fallback stub for unsupported platforms (FreeBSD, Android, etc.) — graceful compile
- 3-platform CI workflow (macOS, Linux with apt deps, Windows)
- 11 new platform-specific unit tests

### Changed
- Extracted platform code from `scanner.rs` and `icons.rs` into `src/platform/` modules
- `libc` dependency moved to `cfg(unix)` (no longer pulled on Windows)
- Added `windows-sys` conditional dependency for Windows builds
- `scan_is_volume` now uses `canonicalize()` for correct path comparison across symlinks/firmlinks

### Fixed
- Operator precedence bug in Windows `is_drive_root` check (would incorrectly treat paths ending in `:` as drive roots)

## [0.1.1.0] - 2026-03-23

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
- CLAUDE.md project guide and TODOS.md for deferred cross-platform work

### Fixed
- APFS double-counting when scanning root volume on macOS
- Treemap rendering for directories with many children (batching into "Other" bucket)
- Selection state cleared on rescan and after deletion
- Disk usage stats refresh after file deletion
- Consistent toolbar widget height and proportional fonts
- Unicode rendering issues in keyboard hints and close buttons
- Multi-select row gap elimination
- UTF-8 path truncation panic in suggestions view on non-ASCII directory names
- Concurrent background deletion guard preventing orphaned threads on double-click
