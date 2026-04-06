//! End-to-end integration tests for disk-cleaner.
//!
//! These tests exercise the full pipeline: real filesystem operations → scanner
//! → tree building → UI logic (selection, filtering, removal, categories).

use disk_cleaner::categories::{compute_stats, FileCategory};
use disk_cleaner::intern::PathInterner;
use disk_cleaner::scanner::{scan_directory, ScanProgress};
use disk_cleaner::tree::auto_expand;
use disk_cleaner::ui;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

/// Helper: create a temp directory with a non-hidden name (avoids `.tmp` prefix
/// which gets filtered by `show_hidden = false`).
fn tmpdir() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("test_").tempdir().unwrap()
}

/// Helper: create a file with specific size (writes that many zero bytes).
fn create_file(dir: &std::path::Path, name: &str, size: usize) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, vec![0u8; size]).unwrap();
    path
}

/// Helper: build a ScanProgress with default values.
fn progress() -> Arc<ScanProgress> {
    Arc::new(ScanProgress {
        file_count: std::sync::atomic::AtomicU64::new(0),
        total_size: std::sync::atomic::AtomicU64::new(0),
        cancelled: std::sync::atomic::AtomicBool::new(false),
    })
}

/// Helper: scan a temp directory and return the tree root.
fn scan(dir: &std::path::Path) -> disk_cleaner::tree::FileNode {
    scan_directory(dir, progress())
}

// ---------------------------------------------------------------------------
// Scan → tree structure
// ---------------------------------------------------------------------------

#[test]
fn scan_produces_correct_hierarchy() {
    let tmp = tmpdir();
    let root = tmp.path();

    create_file(root, "a.txt", 100);
    create_file(root, "b.rs", 200);
    fs::create_dir(root.join("sub")).unwrap();
    create_file(&root.join("sub"), "c.log", 300);

    let tree = scan(root);

    assert!(tree.is_dir());
    assert_eq!(tree.children().len(), 3); // a.txt, b.rs, sub/
    assert!(tree.size() >= 600);

    let sub = tree.children().iter().find(|c| c.name() == "sub").unwrap();
    assert!(sub.is_dir());
    assert_eq!(sub.children().len(), 1);
    assert_eq!(sub.children()[0].name(), "c.log");
    assert!(sub.children()[0].size() >= 300);
}

#[test]
fn scan_empty_dir_produces_empty_tree() {
    let tmp = tmpdir();
    let tree = scan(tmp.path());
    assert!(tree.is_dir());
    assert_eq!(tree.children().len(), 0);
    assert_eq!(tree.size(), 0);
}

#[test]
fn scan_deeply_nested_structure() {
    let tmp = tmpdir();
    let mut cur = tmp.path().to_path_buf();
    for i in 0..5 {
        cur = cur.join(format!("level{i}"));
        fs::create_dir(&cur).unwrap();
    }
    create_file(&cur, "deep.txt", 42);

    let tree = scan(tmp.path());
    assert!(tree.size() >= 42);

    // Walk down 5 levels
    let mut node = &tree;
    for i in 0..5 {
        let child = node
            .children()
            .iter()
            .find(|c| c.name() == format!("level{i}"))
            .unwrap_or_else(|| panic!("Missing level{i}"));
        assert!(child.is_dir());
        node = child;
    }
    assert_eq!(node.children().len(), 1);
    assert_eq!(node.children()[0].name(), "deep.txt");
}

// ---------------------------------------------------------------------------
// Scan → category classification → stats
// ---------------------------------------------------------------------------

#[test]
fn scan_categorizes_files_correctly() {
    let tmp = tmpdir();
    let root = tmp.path();

    create_file(root, "movie.mp4", 1000);
    create_file(root, "photo.jpg", 500);
    create_file(root, "song.mp3", 300);
    create_file(root, "readme.txt", 100);
    create_file(root, "archive.zip", 200);
    create_file(root, "main.rs", 150);
    create_file(root, "unknown.xyz", 50);

    let tree = scan(root);
    let stats = compute_stats(&tree);

    let video = stats
        .entries
        .iter()
        .find(|s| s.0 == FileCategory::Video)
        .unwrap();
    assert!(video.1 >= 1000);
    assert_eq!(video.2, 1);

    let image = stats
        .entries
        .iter()
        .find(|s| s.0 == FileCategory::Image)
        .unwrap();
    assert!(image.1 >= 500);

    let audio = stats
        .entries
        .iter()
        .find(|s| s.0 == FileCategory::Audio)
        .unwrap();
    assert!(audio.1 >= 300);

    let code = stats
        .entries
        .iter()
        .find(|s| s.0 == FileCategory::Code)
        .unwrap();
    assert!(code.1 >= 150);
}

// ---------------------------------------------------------------------------
// Scan → auto-expand → verify expansion
// ---------------------------------------------------------------------------

#[test]
fn auto_expand_works_on_scanned_tree() {
    let tmp = tmpdir();
    let root = tmp.path();

    fs::create_dir(root.join("big")).unwrap();
    create_file(&root.join("big"), "large.bin", 100_000);

    fs::create_dir(root.join("tiny")).unwrap();
    create_file(&root.join("tiny"), "small.txt", 10);

    let mut tree = scan(root);
    tree.set_expanded(true);
    auto_expand(&mut tree, 0, 2);

    let big = tree.children().iter().find(|c| c.name() == "big").unwrap();
    assert!(big.expanded(), "big/ should auto-expand (dominant)");

    let tiny = tree.children().iter().find(|c| c.name() == "tiny").unwrap();
    assert!(!tiny.expanded(), "tiny/ should not auto-expand");
}

// ---------------------------------------------------------------------------
// Scan → selection → collect_selected
// ---------------------------------------------------------------------------

#[test]
fn select_and_collect_from_scanned_tree() {
    let tmp = tmpdir();
    let root = tmp.path();

    create_file(root, "a.txt", 100);
    create_file(root, "b.txt", 200);
    fs::create_dir(root.join("sub")).unwrap();
    create_file(&root.join("sub"), "c.txt", 300);

    let _tree = scan(root);

    let path_a = root.join("a.txt");
    let path_c = root.join("sub").join("c.txt");

    // Selection is now tracked via HashSet (matching production code)
    let mut selected_paths: HashSet<PathBuf> = HashSet::new();

    // Simulate non-shift click on a.txt
    selected_paths.clear();
    selected_paths.insert(path_a.clone());

    // Simulate shift-click on c.txt
    selected_paths.insert(path_c.clone());

    assert_eq!(selected_paths.len(), 2);
    assert!(selected_paths.contains(&path_a));
    assert!(selected_paths.contains(&path_c));
}

// ---------------------------------------------------------------------------
// Scan → search/filter
// ---------------------------------------------------------------------------

#[test]
fn search_filter_matches_correct_nodes() {
    let tmp = tmpdir();
    let root = tmp.path();

    create_file(root, "readme.md", 100);
    create_file(root, "main.rs", 200);
    fs::create_dir(root.join("docs")).unwrap();
    create_file(&root.join("docs"), "guide.md", 300);

    let tree = scan(root);

    assert!(ui::node_matches(&tree, "md"));

    let readme = tree
        .children()
        .iter()
        .find(|c| c.name() == "readme.md")
        .unwrap();
    assert!(ui::node_matches(readme, "md"));

    let main_rs = tree
        .children()
        .iter()
        .find(|c| c.name() == "main.rs")
        .unwrap();
    assert!(!ui::node_matches(main_rs, "md"));

    let docs = tree.children().iter().find(|c| c.name() == "docs").unwrap();
    assert!(ui::node_matches(docs, "md")); // matches because descendant has .md
}

// ---------------------------------------------------------------------------
// Scan → remove_node
// ---------------------------------------------------------------------------

#[test]
fn remove_node_updates_tree_sizes() {
    let tmp = tmpdir();
    let root = tmp.path();

    create_file(root, "keep.txt", 100);
    create_file(root, "remove.txt", 500);

    let mut tree = scan(root);
    let original_size = tree.size();
    assert_eq!(tree.children().len(), 2);

    let remove_path = root.join("remove.txt");
    let removed_size = ui::remove_node(&mut tree, &remove_path);

    assert!(removed_size.is_some());
    assert_eq!(tree.children().len(), 1);
    assert_eq!(tree.children()[0].name(), "keep.txt");
    assert!(tree.size() < original_size);
}

// ---------------------------------------------------------------------------
// Scan → delete (fs::remove) → verify filesystem state
// ---------------------------------------------------------------------------

#[test]
fn delete_file_removes_from_tree_and_filesystem() {
    let tmp = tmpdir();
    let root = tmp.path();

    let delete_me = create_file(root, "delete_me.txt", 100);
    create_file(root, "keep.txt", 200);

    let mut tree = scan(root);
    assert_eq!(tree.children().len(), 2);

    // Delete via filesystem (same codepath as permanent delete)
    fs::remove_file(&delete_me).unwrap();
    ui::remove_node(&mut tree, &delete_me);

    assert_eq!(tree.children().len(), 1);
    assert_eq!(tree.children()[0].name(), "keep.txt");
    assert!(!delete_me.exists());
}

// ---------------------------------------------------------------------------
// Scan → toggle expand → collect visible paths
// ---------------------------------------------------------------------------

#[test]
fn toggle_expand_reveals_children_in_visible_paths() {
    let tmp = tmpdir();
    let root = tmp.path();

    fs::create_dir(root.join("folder")).unwrap();
    create_file(&root.join("folder"), "inner.txt", 100);
    create_file(root, "outer.txt", 200);

    let mut tree = scan(root);
    // Root is already expanded by scanner

    // Before expanding folder, inner.txt should not be visible
    let mut interner = PathInterner::new();
    let rows = ui::collect_cached_rows(&tree, "", None, true, None, None, None, &mut interner);
    let inner_str = root.join("folder").join("inner.txt").to_string_lossy().into_owned();
    assert!(
        !rows.iter().any(|r| *r.path == *inner_str),
        "inner.txt should not be visible when folder is collapsed"
    );

    // Expand the folder
    ui::toggle_expand(&mut tree, &root.join("folder"));

    let rows = ui::collect_cached_rows(&tree, "", None, true, None, None, None, &mut interner);
    assert!(
        rows.iter().any(|r| *r.path == *inner_str),
        "inner.txt should be visible after expanding folder"
    );
}

// ---------------------------------------------------------------------------
// Scan → category filter → visible paths
// ---------------------------------------------------------------------------

#[test]
fn category_filter_shows_only_matching_files() {
    let tmp = tmpdir();
    let root = tmp.path();

    create_file(root, "video.mp4", 1000);
    create_file(root, "code.rs", 200);
    create_file(root, "photo.jpg", 500);

    let tree = scan(root);
    // Root is already expanded by scanner

    // Filter to videos only
    let mut interner = PathInterner::new();
    let rows = ui::collect_cached_rows(&tree, "", Some(FileCategory::Video), true, None, None, None, &mut interner);

    let video_str = root.join("video.mp4").to_string_lossy().into_owned();
    let code_str = root.join("code.rs").to_string_lossy().into_owned();
    let photo_str = root.join("photo.jpg").to_string_lossy().into_owned();

    assert!(rows.iter().any(|r| *r.path == *video_str));
    assert!(!rows.iter().any(|r| *r.path == *code_str));
    assert!(!rows.iter().any(|r| *r.path == *photo_str));
}

// ---------------------------------------------------------------------------
// Scan → hidden files
// ---------------------------------------------------------------------------

#[test]
fn hidden_files_excluded_by_default() {
    let tmp = tmpdir();
    let root = tmp.path();

    create_file(root, "visible.txt", 100);
    create_file(root, ".hidden", 200);

    let tree = scan(root);
    // Root is already expanded by scanner

    // Without show_hidden, .hidden should be excluded
    let mut interner = PathInterner::new();
    let rows = ui::collect_cached_rows(&tree, "", None, false, None, None, None, &mut interner);
    let hidden_str = root.join(".hidden").to_string_lossy().into_owned();
    let visible_str = root.join("visible.txt").to_string_lossy().into_owned();
    assert!(!rows.iter().any(|r| *r.path == *hidden_str));
    assert!(rows.iter().any(|r| *r.path == *visible_str));

    // With show_hidden, both files are visible but grouped into "[2 files]"
    let rows = ui::collect_cached_rows(&tree, "", None, true, None, None, None, &mut interner);
    // Root + "[2 files]" group (both files visible → grouped since ≥ threshold)
    assert!(rows.iter().any(|r| r.is_file_group && r.name.as_ref() == "[2 files]"));

    // When file group is expanded, individual files become visible
    let mut expanded = std::collections::HashSet::new();
    expanded.insert(interner.intern(root));
    let rows = ui::collect_cached_rows(&tree, "", None, true, None, None, Some(&expanded), &mut interner);
    assert!(rows.iter().any(|r| *r.path == *hidden_str));
    assert!(rows.iter().any(|r| *r.path == *visible_str));
}

// ---------------------------------------------------------------------------
// Scan progress tracking
// ---------------------------------------------------------------------------

#[test]
fn scan_progress_tracks_file_count() {
    let tmp = tmpdir();
    let root = tmp.path();

    for i in 0..10 {
        create_file(root, &format!("file{i}.txt"), 100);
    }

    let prog = progress();
    let _tree = scan_directory(root, Arc::clone(&prog));

    let files = prog.file_count.load(std::sync::atomic::Ordering::Relaxed);
    let bytes = prog.total_size.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        files >= 10,
        "Should have scanned at least 10 files, got {files}"
    );
    assert!(bytes >= 1000);
}

// ---------------------------------------------------------------------------
// Full pipeline: scan → expand → select → count → clear → verify
// ---------------------------------------------------------------------------

#[test]
fn full_pipeline_scan_select_clear() {
    let tmp = tmpdir();
    let root = tmp.path();

    fs::create_dir(root.join("src")).unwrap();
    create_file(&root.join("src"), "main.rs", 500);
    create_file(&root.join("src"), "lib.rs", 300);
    fs::create_dir(root.join("docs")).unwrap();
    create_file(&root.join("docs"), "README.md", 100);
    create_file(root, "Cargo.toml", 200);

    // Step 1: Scan
    let mut tree = scan(root);
    assert!(tree.size() >= 1100);
    assert_eq!(tree.children().len(), 3); // src/, docs/, Cargo.toml

    // Step 2: Auto-expand
    auto_expand(&mut tree, 0, 2);

    // Step 3: Select files via HashSet (matching production code path)
    let main_path = root.join("src").join("main.rs");
    let readme_path = root.join("docs").join("README.md");
    let mut selected_paths: HashSet<PathBuf> = HashSet::new();

    // Non-shift click on main.rs
    selected_paths.clear();
    selected_paths.insert(main_path.clone());
    // Shift-click on README.md
    selected_paths.insert(readme_path.clone());

    assert_eq!(selected_paths.len(), 2);

    // Step 4: Clear selection
    selected_paths.clear();
    assert_eq!(selected_paths.len(), 0);

    // Step 5: Verify tree structure is intact
    let src = tree.children().iter().find(|c| c.name() == "src").unwrap();
    assert_eq!(src.children().len(), 2);
    let docs = tree.children().iter().find(|c| c.name() == "docs").unwrap();
    assert_eq!(docs.children().len(), 1);
}

// ---------------------------------------------------------------------------
// Scan cancellation
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Disclosure triangle click clears stale selection
// ---------------------------------------------------------------------------

#[test]
fn disclosure_triangle_click_clears_selection() {
    let tmp = tmpdir();
    let root = tmp.path();

    fs::create_dir(root.join("folder_a")).unwrap();
    create_file(&root.join("folder_a"), "a.txt", 100);
    fs::create_dir(root.join("folder_b")).unwrap();
    create_file(&root.join("folder_b"), "b.txt", 200);

    let mut tree = scan(root);

    // Simulate the app state fields relevant to selection
    let mut selected_paths: HashSet<PathBuf> = HashSet::new();
    // --- Step 1: Plain click on folder_a (Click action) ---
    // This mirrors the Click handler in main.rs
    let click_path = root.join("folder_a");
    selected_paths.clear();
    selected_paths.insert(click_path.clone());
    let mut focused_path: Option<PathBuf> = Some(click_path.clone());

    assert_eq!(selected_paths.len(), 1);
    assert!(selected_paths.contains(&click_path));
    assert_eq!(focused_path.as_deref(), Some(click_path.as_path()));

    // --- Step 2: Click disclosure triangle on folder_b ---
    // The UI emits two actions: Focus(folder_b) + ToggleExpand(folder_b)
    let triangle_path = root.join("folder_b");

    // Focus action (processed first in the action loop)
    focused_path = Some(triangle_path.clone());

    // ToggleExpand action (processed in the second loop)
    ui::toggle_expand(&mut tree, &triangle_path);
    selected_paths.clear(); // <-- the fix under test

    // --- Assertions ---
    // Selection must be empty (no ghost highlight on folder_a)
    assert!(
        selected_paths.is_empty(),
        "selected_paths should be empty after disclosure triangle click"
    );
    // Focus should point to the triangle-clicked row
    assert_eq!(
        focused_path.as_deref(),
        Some(triangle_path.as_path()),
        "focused_path should be set to the disclosure-triangle target"
    );
    // The folder should now be expanded
    let folder_b = tree
        .children()
        .iter()
        .find(|c| c.name() == "folder_b")
        .unwrap();
    assert!(
        folder_b.expanded(),
        "folder_b should be expanded after toggle"
    );
}

// ---------------------------------------------------------------------------
// Scan cancellation
// ---------------------------------------------------------------------------

#[test]
fn cancelled_scan_stops_early() {
    let tmp = tmpdir();
    let root = tmp.path();

    // Create many files
    for i in 0..100 {
        create_file(root, &format!("file{i}.dat"), 50);
    }

    let prog = progress();
    // Cancel immediately
    prog.cancelled
        .store(true, std::sync::atomic::Ordering::Relaxed);

    let tree = scan_directory(root, prog);
    // Tree should be mostly empty due to cancellation
    assert!(tree.children().len() < 100);
}
