use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use disk_cleaner::tree::{DirNode, FileLeaf, FileNode};
use disk_cleaner::ui;
use std::collections::HashSet;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Synthetic tree builders (no disk I/O)
// ---------------------------------------------------------------------------

fn make_leaf(name: &str, size: u64) -> FileNode {
    FileNode::File(FileLeaf {
        name: name.into(),
        size,
        hidden: false,
    })
}

fn make_dir(name: &str, children: Vec<FileNode>) -> FileNode {
    let size = children.iter().map(|c| c.size()).sum();
    FileNode::Dir(DirNode {
        name: name.into(),
        size,
        children,
        expanded: false,
        hidden: false,
    })
}

/// Wide tree: one root with `n_dirs` directories, each holding `files_per_dir` files.
fn build_wide_tree(n_dirs: usize, files_per_dir: usize) -> FileNode {
    let dirs: Vec<FileNode> = (0..n_dirs)
        .map(|i| {
            let files: Vec<FileNode> = (0..files_per_dir)
                .map(|j| make_leaf(&format!("file_{j}.dat"), (j as u64 + 1) * 1024))
                .collect();
            make_dir(&format!("dir_{i:05}"), files)
        })
        .collect();
    make_dir("root", dirs)
}

/// Deep tree: single chain of `depth` directories, with `files_per_level` files at each level.
fn build_deep_tree(depth: usize, files_per_level: usize) -> FileNode {
    let mut node = make_dir("leaf_dir", vec![make_leaf("bottom.dat", 1024)]);
    for d in (0..depth).rev() {
        let mut children: Vec<FileNode> = (0..files_per_level)
            .map(|j| make_leaf(&format!("file_{j}.dat"), (j as u64 + 1) * 512))
            .collect();
        children.push(node);
        node = make_dir(&format!("level_{d:04}"), children);
    }
    make_dir("root", vec![node])
}

/// Mixed tree: 2-level hierarchy with varied fan-out to approximate real scans.
/// Top level has `n_top` dirs. Each top dir has between 1 and `max_sub` subdirs,
/// each subdirectory has `files_per_sub` files.
fn build_mixed_tree(n_top: usize, max_sub: usize, files_per_sub: usize) -> FileNode {
    let dirs: Vec<FileNode> = (0..n_top)
        .map(|i| {
            let n_sub = (i % max_sub) + 1;
            let subdirs: Vec<FileNode> = (0..n_sub)
                .map(|s| {
                    let files: Vec<FileNode> = (0..files_per_sub)
                        .map(|j| make_leaf(&format!("f_{j}.bin"), (j as u64 + 1) * 256))
                        .collect();
                    make_dir(&format!("sub_{s:03}"), files)
                })
                .collect();
            make_dir(&format!("top_{i:04}"), subdirs)
        })
        .collect();
    make_dir("root", dirs)
}

fn count_nodes(node: &FileNode) -> usize {
    1 + node.children().iter().map(count_nodes).sum::<usize>()
}

/// Walk tree and count how many node paths are in `selected`.
/// Simulates the selection-counting path the UI takes.
fn count_selected(node: &FileNode, prefix: &mut PathBuf, selected: &HashSet<PathBuf>) -> usize {
    prefix.push(node.name());
    let mut total = if selected.contains(prefix.as_path()) {
        1
    } else {
        0
    };
    for child in node.children() {
        total += count_selected(child, prefix, selected);
    }
    prefix.pop();
    total
}

/// Collect all paths in the tree (for building a selection set).
fn collect_paths(node: &FileNode, prefix: &mut PathBuf, out: &mut Vec<PathBuf>) {
    prefix.push(node.name());
    out.push(prefix.clone());
    for child in node.children() {
        collect_paths(child, prefix, out);
    }
    prefix.pop();
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

fn bench_node_matches(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_matches");
    group.sample_size(20);

    // (label, tree, node_count)
    let cases: Vec<(&str, FileNode)> = vec![
        // ~10K nodes: 500 dirs × 20 files = 10_500 + 501
        ("wide_10k", build_wide_tree(500, 20)),
        // ~100K nodes: 5000 dirs × 20 files = 100_000 + 5001
        ("wide_100k", build_wide_tree(5_000, 20)),
        // ~1M nodes: 50000 dirs × 20 files = 1_000_000 + 50001
        ("wide_1m", build_wide_tree(50_000, 20)),
        // Deep ~10K: 1000 deep × 10 files/level
        ("deep_10k", build_deep_tree(1_000, 10)),
        // Mixed ~100K
        ("mixed_100k", build_mixed_tree(500, 20, 10)),
    ];

    for (label, tree) in &cases {
        let n = count_nodes(tree);
        // Hit case: search for a name that exists everywhere
        group.bench_with_input(
            BenchmarkId::new("hit", format!("{label}_{n}")),
            tree,
            |b, t| b.iter(|| ui::node_matches(t, "file_0")),
        );
        // Miss case: search for a name that exists nowhere
        group.bench_with_input(
            BenchmarkId::new("miss", format!("{label}_{n}")),
            tree,
            |b, t| b.iter(|| ui::node_matches(t, "nonexistent_zzz")),
        );
    }

    group.finish();
}

fn bench_count_selected(c: &mut Criterion) {
    let mut group = c.benchmark_group("count_selected");
    group.sample_size(20);

    let cases: Vec<(&str, FileNode)> = vec![
        ("wide_10k", build_wide_tree(500, 20)),
        ("wide_100k", build_wide_tree(5_000, 20)),
        ("wide_1m", build_wide_tree(50_000, 20)),
        ("deep_10k", build_deep_tree(1_000, 10)),
    ];

    for (label, tree) in &cases {
        let n = count_nodes(tree);

        // Collect all paths, then select ~10% of them
        let mut all_paths = Vec::new();
        collect_paths(tree, &mut PathBuf::new(), &mut all_paths);
        let selected: HashSet<PathBuf> = all_paths.iter().step_by(10).cloned().collect();

        let sel_count = selected.len();
        group.bench_with_input(
            BenchmarkId::new("10pct", format!("{label}_{n}_sel{sel_count}")),
            &(tree, selected),
            |b, (t, sel)| {
                b.iter(|| {
                    let mut prefix = PathBuf::new();
                    count_selected(t, &mut prefix, sel)
                })
            },
        );

        // Empty selection (cheapest path)
        let empty: HashSet<PathBuf> = HashSet::new();
        group.bench_with_input(
            BenchmarkId::new("empty", format!("{label}_{n}")),
            &(tree, empty),
            |b, (t, sel)| {
                b.iter(|| {
                    let mut prefix = PathBuf::new();
                    count_selected(t, &mut prefix, sel)
                })
            },
        );
    }

    group.finish();
}

fn build_unsorted_children(n: usize) -> Vec<FileNode> {
    (0..n)
        .map(|i| {
            make_leaf(
                &format!("f_{i}.dat"),
                ((n - i) as u64) * 1024 + (i as u64 % 7),
            )
        })
        .collect()
}

fn bench_sort_by_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("sort_by_size");
    group.sample_size(30);

    for &n in &[1_000usize, 10_000, 100_000] {
        group.bench_function(BenchmarkId::new("children", n), |b| {
            b.iter_batched(
                || build_unsorted_children(n),
                |mut v| {
                    v.sort_by_key(|a| std::cmp::Reverse(a.size()));
                    v
                },
                criterion::BatchSize::LargeInput,
            )
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Tree walk operations at scale (find_node_info, toggle_expand, remove_node,
// set_expanded, find_parent_path) — these run on every keystroke/click
// ---------------------------------------------------------------------------

fn bench_tree_walks(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_walks");
    group.sample_size(20);

    let cases: Vec<(&str, FileNode)> = vec![
        ("wide_10k", build_wide_tree(500, 20)),
        ("wide_100k", build_wide_tree(5_000, 20)),
    ];

    for (label, tree) in &cases {
        let n = count_nodes(tree);

        // --- find_node_info: shallow target (first dir) ---
        let shallow_target = PathBuf::from("root/dir_00000");
        group.bench_with_input(
            BenchmarkId::new("find_node_info_shallow", format!("{label}_{n}")),
            tree,
            |b, t| b.iter(|| ui::find_node_info(t, &shallow_target)),
        );

        // --- find_node_info: deep target (last dir, last file) ---
        let last_dir = format!("dir_{:05}", tree.children().len().saturating_sub(1));
        let deep_target = PathBuf::from(format!("root/{last_dir}/file_19.dat"));
        group.bench_with_input(
            BenchmarkId::new("find_node_info_deep", format!("{label}_{n}")),
            tree,
            |b, t| b.iter(|| ui::find_node_info(t, &deep_target)),
        );

        // --- find_node_info: miss (nonexistent path) ---
        let miss_target = PathBuf::from("root/nope/nada");
        group.bench_with_input(
            BenchmarkId::new("find_node_info_miss", format!("{label}_{n}")),
            tree,
            |b, t| b.iter(|| ui::find_node_info(t, &miss_target)),
        );

        // --- find_parent_path ---
        group.bench_with_input(
            BenchmarkId::new("find_parent_path", format!("{label}_{n}")),
            tree,
            |b, t| b.iter(|| ui::find_parent_path(t, &deep_target)),
        );
    }

    // --- toggle_expand (mutating, use iter_batched) ---
    for &(n_dirs, files_per_dir) in &[(500, 20), (5_000, 20)] {
        let n = n_dirs * (files_per_dir + 1) + 1;
        let mid_dir = format!("root/dir_{:05}", n_dirs / 2);
        let target = PathBuf::from(&mid_dir);

        group.bench_function(BenchmarkId::new("toggle_expand", format!("{n}")), |b| {
            b.iter_batched(
                || build_wide_tree(n_dirs, files_per_dir),
                |mut t| {
                    ui::toggle_expand(&mut t, &target);
                    t
                },
                criterion::BatchSize::LargeInput,
            )
        });
    }

    // --- set_expanded ---
    for &(n_dirs, files_per_dir) in &[(500, 20), (5_000, 20)] {
        let n = n_dirs * (files_per_dir + 1) + 1;
        let mid_dir = format!("root/dir_{:05}", n_dirs / 2);
        let target = PathBuf::from(&mid_dir);

        group.bench_function(BenchmarkId::new("set_expanded", format!("{n}")), |b| {
            b.iter_batched(
                || build_wide_tree(n_dirs, files_per_dir),
                |mut t| {
                    ui::set_expanded(&mut t, &target, true);
                    t
                },
                criterion::BatchSize::LargeInput,
            )
        });
    }

    // --- remove_node (mutating) ---
    for &(n_dirs, files_per_dir) in &[(500, 20), (5_000, 20)] {
        let n = n_dirs * (files_per_dir + 1) + 1;
        let target_file = format!("root/dir_{:05}/file_10.dat", n_dirs / 2);
        let target = PathBuf::from(&target_file);

        group.bench_function(BenchmarkId::new("remove_node", format!("{n}")), |b| {
            b.iter_batched(
                || build_wide_tree(n_dirs, files_per_dir),
                |mut t| {
                    ui::remove_node(&mut t, &target);
                    t
                },
                criterion::BatchSize::LargeInput,
            )
        });
    }

    // --- batch remove_node (simulating multi-delete) ---
    {
        let n_dirs = 500;
        let files_per_dir = 20;
        let n = n_dirs * (files_per_dir + 1) + 1;
        // Remove 100 files from different directories
        let targets: Vec<PathBuf> = (0..100)
            .map(|i| PathBuf::from(format!("root/dir_{:05}/file_5.dat", i * 5)))
            .collect();

        group.bench_function(
            BenchmarkId::new("remove_node_batch_100", format!("{n}")),
            |b| {
                b.iter_batched(
                    || build_wide_tree(n_dirs, files_per_dir),
                    |mut t| {
                        for target in &targets {
                            ui::remove_node(&mut t, target);
                        }
                        t
                    },
                    criterion::BatchSize::LargeInput,
                )
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Selection building benchmarks (shift-click range, clear, HashSet operations)
// ---------------------------------------------------------------------------

fn bench_selection_ops(c: &mut Criterion) {
    let mut group = c.benchmark_group("selection_ops");
    group.sample_size(20);

    for &(n_dirs, files_per_dir) in &[(500, 20), (5_000, 20)] {
        let tree = build_wide_tree(n_dirs, files_per_dir);
        let n = count_nodes(&tree);

        // Collect all paths
        let mut all_paths = Vec::new();
        collect_paths(&tree, &mut PathBuf::new(), &mut all_paths);

        // Build a large selection (simulating shift-click range)
        let range_size = all_paths.len().min(1000);
        group.bench_function(
            BenchmarkId::new("build_range_selection", format!("{n}_range{range_size}")),
            |b| {
                b.iter(|| {
                    let sel: HashSet<PathBuf> = all_paths[..range_size].iter().cloned().collect();
                    sel
                })
            },
        );

        // Lookup in large selection (hot path during rendering)
        let large_sel: HashSet<PathBuf> = all_paths.iter().step_by(10).cloned().collect();
        let sel_size = large_sel.len();
        let lookup_target = all_paths[all_paths.len() / 2].clone();
        group.bench_function(
            BenchmarkId::new("selection_contains", format!("{n}_sel{sel_size}")),
            |b| b.iter(|| large_sel.contains(&lookup_target)),
        );

        // Clear large selection
        group.bench_function(
            BenchmarkId::new("selection_clear", format!("{n}_sel{sel_size}")),
            |b| {
                b.iter_batched(
                    || large_sel.clone(),
                    |mut sel| {
                        sel.clear();
                        sel
                    },
                    criterion::BatchSize::SmallInput,
                )
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_node_matches,
    bench_count_selected,
    bench_sort_by_size,
    bench_tree_walks,
    bench_selection_ops,
);
criterion_main!(benches);
