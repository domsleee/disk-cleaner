use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use disk_cleaner::tree::{self, build_test_tree, dir, leaf, FileTree, NodeId, TestNode};
use disk_cleaner::ui;
use std::collections::HashSet;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Synthetic tree builders (no disk I/O)
// ---------------------------------------------------------------------------

/// Wide tree: one root with `n_dirs` directories, each holding `files_per_dir` files.
fn build_wide_tree(n_dirs: usize, files_per_dir: usize) -> FileTree {
    let dirs: Vec<TestNode> = (0..n_dirs)
        .map(|i| {
            let files: Vec<TestNode> = (0..files_per_dir)
                .map(|j| leaf(&format!("file_{j}.dat"), (j as u64 + 1) * 1024))
                .collect();
            dir(&format!("dir_{i:05}"), files)
        })
        .collect();
    build_test_tree(dir("root", dirs))
}

/// Deep tree: single chain of `depth` directories, with `files_per_level` files at each level.
fn build_deep_tree(depth: usize, files_per_level: usize) -> FileTree {
    fn build_level(depth: usize, files_per_level: usize) -> TestNode {
        if depth == 0 {
            return dir("leaf_dir", vec![leaf("bottom.dat", 1024)]);
        }
        let mut children: Vec<TestNode> = (0..files_per_level)
            .map(|j| leaf(&format!("file_{j}.dat"), (j as u64 + 1) * 512))
            .collect();
        children.push(build_level(depth - 1, files_per_level));
        dir(&format!("level_{:04}", depth - 1), children)
    }
    build_test_tree(dir("root", vec![build_level(depth, files_per_level)]))
}

/// Mixed tree: 2-level hierarchy with varied fan-out to approximate real scans.
fn build_mixed_tree(n_top: usize, max_sub: usize, files_per_sub: usize) -> FileTree {
    let dirs: Vec<TestNode> = (0..n_top)
        .map(|i| {
            let n_sub = (i % max_sub) + 1;
            let subdirs: Vec<TestNode> = (0..n_sub)
                .map(|s| {
                    let files: Vec<TestNode> = (0..files_per_sub)
                        .map(|j| leaf(&format!("f_{j}.bin"), (j as u64 + 1) * 256))
                        .collect();
                    dir(&format!("sub_{s:03}"), files)
                })
                .collect();
            dir(&format!("top_{i:04}"), subdirs)
        })
        .collect();
    build_test_tree(dir("root", dirs))
}

fn count_nodes(tree: &FileTree, id: NodeId) -> usize {
    1 + tree.children(id).iter().map(|&c| count_nodes(tree, c)).sum::<usize>()
}

/// Walk tree and count how many node paths are in `selected`.
fn count_selected(tree: &FileTree, id: NodeId, prefix: &mut PathBuf, selected: &HashSet<PathBuf>) -> usize {
    prefix.push(tree.name(id));
    let mut total = if selected.contains(prefix.as_path()) { 1 } else { 0 };
    for &child in tree.children(id) {
        total += count_selected(tree, child, prefix, selected);
    }
    prefix.pop();
    total
}

/// Collect all paths in the tree (for building a selection set).
fn collect_paths(tree: &FileTree, id: NodeId, prefix: &mut PathBuf, out: &mut Vec<PathBuf>) {
    prefix.push(tree.name(id));
    out.push(prefix.clone());
    for &child in tree.children(id) {
        collect_paths(tree, child, prefix, out);
    }
    prefix.pop();
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

fn bench_node_matches(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_matches");
    group.sample_size(20);

    let cases: Vec<(&str, FileTree)> = vec![
        ("wide_10k", build_wide_tree(500, 20)),
        ("wide_100k", build_wide_tree(5_000, 20)),
        ("wide_1m", build_wide_tree(50_000, 20)),
        ("deep_10k", build_deep_tree(1_000, 10)),
        ("mixed_100k", build_mixed_tree(500, 20, 10)),
    ];

    for (label, tree) in &cases {
        let n = count_nodes(tree, tree.root());
        let root = tree.root();
        group.bench_with_input(
            BenchmarkId::new("hit", format!("{label}_{n}")),
            tree,
            |b, t| b.iter(|| ui::node_matches(t, root, "file_0")),
        );
        group.bench_with_input(
            BenchmarkId::new("miss", format!("{label}_{n}")),
            tree,
            |b, t| b.iter(|| ui::node_matches(t, root, "nonexistent_zzz")),
        );
    }

    group.finish();
}

fn bench_count_selected(c: &mut Criterion) {
    let mut group = c.benchmark_group("count_selected");
    group.sample_size(20);

    let cases: Vec<(&str, FileTree)> = vec![
        ("wide_10k", build_wide_tree(500, 20)),
        ("wide_100k", build_wide_tree(5_000, 20)),
        ("wide_1m", build_wide_tree(50_000, 20)),
        ("deep_10k", build_deep_tree(1_000, 10)),
    ];

    for (label, tree) in &cases {
        let n = count_nodes(tree, tree.root());
        let root = tree.root();

        let mut all_paths = Vec::new();
        collect_paths(tree, root, &mut PathBuf::new(), &mut all_paths);
        let selected: HashSet<PathBuf> = all_paths.iter().step_by(10).cloned().collect();

        let sel_count = selected.len();
        group.bench_with_input(
            BenchmarkId::new("10pct", format!("{label}_{n}_sel{sel_count}")),
            &(tree, selected),
            |b, (t, sel)| {
                b.iter(|| {
                    let mut prefix = PathBuf::new();
                    count_selected(t, root, &mut prefix, sel)
                })
            },
        );

        let empty: HashSet<PathBuf> = HashSet::new();
        group.bench_with_input(
            BenchmarkId::new("empty", format!("{label}_{n}")),
            &(tree, empty),
            |b, (t, sel)| {
                b.iter(|| {
                    let mut prefix = PathBuf::new();
                    count_selected(t, root, &mut prefix, sel)
                })
            },
        );
    }

    group.finish();
}

fn bench_sort_by_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("sort_by_size");
    group.sample_size(30);

    // Sort benchmark: build Vec of (name, size) pairs and sort by size descending.
    // This approximates what sort_children_recursive does internally.
    for &n in &[1_000usize, 10_000, 100_000] {
        group.bench_function(BenchmarkId::new("children", n), |b| {
            b.iter_batched(
                || {
                    (0..n)
                        .map(|i| {
                            let size = ((n - i) as u64) * 1024 + (i as u64 % 7);
                            (format!("f_{i}.dat"), size)
                        })
                        .collect::<Vec<_>>()
                },
                |mut v| {
                    v.sort_by_key(|a| std::cmp::Reverse(a.1));
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

    let cases: Vec<(&str, FileTree)> = vec![
        ("wide_10k", build_wide_tree(500, 20)),
        ("wide_100k", build_wide_tree(5_000, 20)),
    ];

    for (label, tree) in &cases {
        let n = count_nodes(tree, tree.root());

        let shallow_target = PathBuf::from("root/dir_00000");
        group.bench_with_input(
            BenchmarkId::new("find_node_info_shallow", format!("{label}_{n}")),
            tree,
            |b, t| b.iter(|| ui::find_node_info(t, &shallow_target)),
        );

        // Deep target: last dir, last file
        let n_dirs = tree.children_count(tree.root());
        let last_dir = format!("dir_{:05}", n_dirs.saturating_sub(1));
        let deep_target = PathBuf::from(format!("root/{last_dir}/file_19.dat"));
        group.bench_with_input(
            BenchmarkId::new("find_node_info_deep", format!("{label}_{n}")),
            tree,
            |b, t| b.iter(|| ui::find_node_info(t, &deep_target)),
        );

        let miss_target = PathBuf::from("root/nope/nada");
        group.bench_with_input(
            BenchmarkId::new("find_node_info_miss", format!("{label}_{n}")),
            tree,
            |b, t| b.iter(|| ui::find_node_info(t, &miss_target)),
        );

        group.bench_with_input(
            BenchmarkId::new("find_parent_path", format!("{label}_{n}")),
            tree,
            |b, t| b.iter(|| ui::find_parent_path(t, &deep_target)),
        );
    }

    // toggle_expand (mutating, use iter_batched)
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

    // set_expanded
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

    // remove_node
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

    // batch remove_node
    {
        let n_dirs = 500;
        let files_per_dir = 20;
        let n = n_dirs * (files_per_dir + 1) + 1;
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
        let n = count_nodes(&tree, tree.root());

        let mut all_paths = Vec::new();
        collect_paths(&tree, tree.root(), &mut PathBuf::new(), &mut all_paths);

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

        let large_sel: HashSet<PathBuf> = all_paths.iter().step_by(10).cloned().collect();
        let sel_size = large_sel.len();
        let lookup_target = all_paths[all_paths.len() / 2].clone();
        group.bench_function(
            BenchmarkId::new("selection_contains", format!("{n}_sel{sel_size}")),
            |b| b.iter(|| large_sel.contains(&lookup_target)),
        );

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
