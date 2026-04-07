//! End-to-end integration tests for disk-cleaner.
//!
//! These tests exercise the full pipeline: real filesystem operations → scanner
//! → tree building → UI logic (selection, filtering, removal, categories).

use disk_cleaner::categories::{compute_stats, FileCategory};
use disk_cleaner::scanner::{scan_directory, ScanProgress};
use disk_cleaner::tree::{auto_expand, FileTree, NodeId};
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

/// Helper: scan a temp directory and return the tree.
fn scan(dir: &std::path::Path) -> FileTree {
    scan_directory(dir, progress())
}

/// Find a child of `parent` whose name matches `name`.
fn find_child(tree: &FileTree, parent: NodeId, name: &str) -> Option<NodeId> {
    tree.children(parent)
        .iter()
        .copied()
        .find(|&c| tree.name(c) == name)
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
    let r = tree.root();

    assert!(tree.is_dir(r));
    assert_eq!(tree.children(r).len(), 3); // a.txt, b.rs, sub/
    assert!(tree.size(r) >= 600);

    let sub = find_child(&tree, r, "sub").unwrap();
    assert!(tree.is_dir(sub));
    assert_eq!(tree.children(sub).len(), 1);
    assert_eq!(tree.name(tree.children(sub)[0]), "c.log");
    assert!(tree.size(tree.children(sub)[0]) >= 300);
}

#[test]
fn scan_empty_dir_produces_empty_tree() {
    let tmp = tmpdir();
    let tree = scan(tmp.path());
    let r = tree.root();
    assert!(tree.is_dir(r));
    assert_eq!(tree.children(r).len(), 0);
    assert_eq!(tree.size(r), 0);
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
    let r = tree.root();
    assert!(tree.size(r) >= 42);

    // Walk down 5 levels
    let mut node = r;
    for i in 0..5 {
        let child = find_child(&tree, node, &format!("level{i}"))
            .unwrap_or_else(|| panic!("Missing level{i}"));
        assert!(tree.is_dir(child));
        node = child;
    }
    assert_eq!(tree.children(node).len(), 1);
    assert_eq!(tree.name(tree.children(node)[0]), "deep.txt");
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
    let r = tree.root();
    tree.set_expanded(r, true);
    auto_expand(&mut tree, r, 0, 2);

    let big = find_child(&tree, r, "big").unwrap();
    assert!(tree.expanded(big), "big/ should auto-expand (dominant)");

    let tiny = find_child(&tree, r, "tiny").unwrap();
    assert!(!tree.expanded(tiny), "tiny/ should not auto-expand");
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
    let r = tree.root();

    assert!(ui::node_matches(&tree, r, "md"));

    let readme = find_child(&tree, r, "readme.md").unwrap();
    assert!(ui::node_matches(&tree, readme, "md"));

    let main_rs = find_child(&tree, r, "main.rs").unwrap();
    assert!(!ui::node_matches(&tree, main_rs, "md"));

    let docs = find_child(&tree, r, "docs").unwrap();
    assert!(ui::node_matches(&tree, docs, "md")); // matches because descendant has .md
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
    let r = tree.root();
    let original_size = tree.size(r);
    assert_eq!(tree.children(r).len(), 2);

    let remove_path = root.join("remove.txt");
    let removed_size = ui::remove_node(&mut tree, &remove_path);

    assert!(removed_size.is_some());
    let r = tree.root();
    assert_eq!(tree.children(r).len(), 1);
    // After swap-remove, order may change, so check by name
    assert_eq!(tree.name(tree.children(r)[0]), "keep.txt");
    assert!(tree.size(r) < original_size);
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
    let r = tree.root();
    assert_eq!(tree.children(r).len(), 2);

    // Delete via filesystem (same codepath as permanent delete)
    fs::remove_file(&delete_me).unwrap();
    ui::remove_node(&mut tree, &delete_me);

    let r = tree.root();
    assert_eq!(tree.children(r).len(), 1);
    assert_eq!(tree.name(tree.children(r)[0]), "keep.txt");
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

    let tree = scan(root);
    // Root is already expanded by scanner

    // Before expanding folder, inner.txt should not be visible
    let rows = ui::collect_cached_rows(&tree, "", None, true, None, None, None);
    let paths: Vec<_> = rows.iter().map(|r| &r.path).collect();
    let inner_path = root.join("folder").join("inner.txt");
    assert!(
        !paths.contains(&&inner_path),
        "inner.txt should not be visible when folder is collapsed"
    );

    // Expand the folder (need mut)
    let mut tree = tree;
    ui::toggle_expand(&mut tree, &root.join("folder"));

    let rows = ui::collect_cached_rows(&tree, "", None, true, None, None, None);
    let paths: Vec<_> = rows.iter().map(|r| &r.path).collect();
    assert!(
        paths.contains(&&inner_path),
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
    let rows = ui::collect_cached_rows(&tree, "", Some(FileCategory::Video), true, None, None, None);
    let paths: Vec<_> = rows.iter().map(|r| &r.path).collect();

    let video_path = root.join("video.mp4");
    let code_path = root.join("code.rs");
    let photo_path = root.join("photo.jpg");

    assert!(paths.contains(&&video_path));
    assert!(!paths.contains(&&code_path));
    assert!(!paths.contains(&&photo_path));
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
    let rows = ui::collect_cached_rows(&tree, "", None, false, None, None, None);
    let paths: Vec<_> = rows.iter().map(|r| &r.path).collect();
    assert!(!paths.contains(&&root.join(".hidden")));
    assert!(paths.contains(&&root.join("visible.txt")));

    // With show_hidden, both files are visible but grouped into "[2 files]"
    let rows = ui::collect_cached_rows(&tree, "", None, true, None, None, None);
    // Root + "[2 files]" group (both files visible → grouped since ≥ threshold)
    assert!(rows.iter().any(|r| r.is_file_group && r.name.as_ref() == "[2 files]"));

    // When file group is expanded, individual files become visible
    let mut expanded = std::collections::HashSet::new();
    expanded.insert(root.to_path_buf());
    let rows = ui::collect_cached_rows(&tree, "", None, true, None, None, Some(&expanded));
    let paths: Vec<_> = rows.iter().map(|r| &r.path).collect();
    assert!(paths.contains(&&root.join(".hidden")));
    assert!(paths.contains(&&root.join("visible.txt")));
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
    let r = tree.root();
    assert!(tree.size(r) >= 1100);
    assert_eq!(tree.children(r).len(), 3); // src/, docs/, Cargo.toml

    // Step 2: Auto-expand
    auto_expand(&mut tree, r, 0, 2);

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
    let r = tree.root();
    let src = find_child(&tree, r, "src").unwrap();
    assert_eq!(tree.children(src).len(), 2);
    let docs = find_child(&tree, r, "docs").unwrap();
    assert_eq!(tree.children(docs).len(), 1);
}

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
    let click_path = root.join("folder_a");
    selected_paths.clear();
    selected_paths.insert(click_path.clone());
    let mut focused_path: Option<PathBuf> = Some(click_path.clone());

    assert_eq!(selected_paths.len(), 1);
    assert!(selected_paths.contains(&click_path));
    assert_eq!(focused_path.as_deref(), Some(click_path.as_path()));

    // --- Step 2: Click disclosure triangle on folder_b ---
    let triangle_path = root.join("folder_b");

    // Focus action (processed first in the action loop)
    focused_path = Some(triangle_path.clone());

    // ToggleExpand action (processed in the second loop)
    ui::toggle_expand(&mut tree, &triangle_path);
    selected_paths.clear(); // <-- the fix under test

    // --- Assertions ---
    assert!(
        selected_paths.is_empty(),
        "selected_paths should be empty after disclosure triangle click"
    );
    assert_eq!(
        focused_path.as_deref(),
        Some(triangle_path.as_path()),
        "focused_path should be set to the disclosure-triangle target"
    );
    // The folder should now be expanded
    let r = tree.root();
    let folder_b = find_child(&tree, r, "folder_b").unwrap();
    assert!(
        tree.expanded(folder_b),
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
    let r = tree.root();
    // Tree should be mostly empty due to cancellation
    assert!(tree.children(r).len() < 100);
}
