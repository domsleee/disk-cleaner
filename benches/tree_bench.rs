//! Tree view benchmarks — per-frame hot path, tree walks, selection, filtering.
//!
//! Covers: collect_cached_rows, node_matches, tree walks (find/toggle/expand/remove),
//! selection ops, filter caches, category matching, auto_expand, compute_stats.
//!
//! ```sh
//! cargo bench --bench tree_bench
//! ```

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use disk_cleaner::categories;
use disk_cleaner::tree::{self, DirNode, FileLeaf, FileNode};
use disk_cleaner::ui;
use std::collections::HashSet;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Synthetic tree builders
// ---------------------------------------------------------------------------

fn make_leaf(name: &str, size: u64) -> FileNode {
    FileNode::File(FileLeaf::new(name.into(), size, false))
}

fn make_dir(name: &str, children: Vec<FileNode>) -> FileNode {
    let size = children.iter().map(|c| c.size()).sum();
    FileNode::Dir(Box::new(DirNode {
        name: name.into(),
        size,
        children,
        expanded: false,
        hidden: false,
    }))
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

/// Wide tree with realistic file extensions for category benchmarks.
fn build_categorized_tree(n_dirs: usize, files_per_dir: usize) -> FileNode {
    let exts = [
        "rs", "mp4", "jpg", "mp3", "pdf", "zip", "dat", "log", "toml", "py",
    ];
    let dirs: Vec<FileNode> = (0..n_dirs)
        .map(|i| {
            let files: Vec<FileNode> = (0..files_per_dir)
                .map(|j| {
                    let ext = exts[j % exts.len()];
                    make_leaf(&format!("file_{j}.{ext}"), (j as u64 + 1) * 1024)
                })
                .collect();
            make_dir(&format!("dir_{i:05}"), files)
        })
        .collect();
    make_dir("/root", dirs)
}

/// Build a tree where all directories are expanded.
fn build_expanded_tree(n_dirs: usize, files_per_dir: usize) -> FileNode {
    let mut tree = build_categorized_tree(n_dirs, files_per_dir);
    expand_all(&mut tree);
    tree
}

fn expand_all(node: &mut FileNode) {
    if node.is_dir() {
        node.set_expanded(true);
        if let FileNode::Dir(d) = node {
            for child in &mut d.children {
                expand_all(child);
            }
        }
    }
}

fn count_nodes(node: &FileNode) -> usize {
    1 + node.children().iter().map(count_nodes).sum::<usize>()
}

/// Walk tree and count how many node paths are in `selected`.
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

// ---------------------------------------------------------------------------
// node_matches — recursive text search
// ---------------------------------------------------------------------------

fn bench_node_matches(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_matches");
    group.sample_size(20);

    let cases: Vec<(&str, FileNode)> = vec![
        ("wide_10k", build_wide_tree(500, 20)),
        ("wide_100k", build_wide_tree(5_000, 20)),
        ("wide_1m", build_wide_tree(50_000, 20)),
        ("deep_10k", build_deep_tree(1_000, 10)),
        ("mixed_100k", build_mixed_tree(500, 20, 10)),
    ];

    for (label, tree) in &cases {
        let n = count_nodes(tree);
        group.bench_with_input(
            BenchmarkId::new("hit", format!("{label}_{n}")),
            tree,
            |b, t| b.iter(|| ui::node_matches(t, "file_0")),
        );
        group.bench_with_input(
            BenchmarkId::new("miss", format!("{label}_{n}")),
            tree,
            |b, t| b.iter(|| ui::node_matches(t, "nonexistent_zzz")),
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// count_selected — selection counting during tree walk
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// sort_by_size — child sorting
// ---------------------------------------------------------------------------

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
// tree_walks — find_node_info, toggle_expand, set_expanded, remove_node
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

        let shallow_target = PathBuf::from("root/dir_00000");
        group.bench_with_input(
            BenchmarkId::new("find_node_info_shallow", format!("{label}_{n}")),
            tree,
            |b, t| b.iter(|| ui::find_node_info(t, &shallow_target)),
        );

        let last_dir = format!("dir_{:05}", tree.children().len().saturating_sub(1));
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
// selection_ops — shift-click range, contains, clear
// ---------------------------------------------------------------------------

fn bench_selection_ops(c: &mut Criterion) {
    let mut group = c.benchmark_group("selection_ops");
    group.sample_size(20);

    for &(n_dirs, files_per_dir) in &[(500, 20), (5_000, 20)] {
        let tree = build_wide_tree(n_dirs, files_per_dir);
        let n = count_nodes(&tree);

        let mut all_paths = Vec::new();
        collect_paths(&tree, &mut PathBuf::new(), &mut all_paths);

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

// ---------------------------------------------------------------------------
// collect_cached_rows — frame hot path proxy
// ---------------------------------------------------------------------------

fn bench_collect_visible_paths(c: &mut Criterion) {
    let mut group = c.benchmark_group("collect_cached_rows");
    group.sample_size(20);

    // All expanded — worst case for tree view rendering
    for &(n_dirs, files_per_dir) in &[(100, 10), (500, 20), (2000, 20)] {
        let tree = build_expanded_tree(n_dirs, files_per_dir);
        let n = count_nodes(&tree);

        group.bench_with_input(BenchmarkId::new("all_expanded", n), &tree, |b, t| {
            b.iter(|| ui::collect_cached_rows(t, "", None, true, None, None, None))
        });
    }

    // Root only expanded (default after scan) — best case
    for &(n_dirs, files_per_dir) in &[(500, 20), (2000, 20)] {
        let mut tree = build_categorized_tree(n_dirs, files_per_dir);
        tree.set_expanded(true);
        let n = count_nodes(&tree);

        group.bench_with_input(BenchmarkId::new("root_only", n), &tree, |b, t| {
            b.iter(|| ui::collect_cached_rows(t, "", None, true, None, None, None))
        });
    }

    // With text filter — uncached vs cached
    {
        let tree = build_expanded_tree(500, 20);
        let n = count_nodes(&tree);

        group.bench_with_input(BenchmarkId::new("filter_hit_uncached", n), &tree, |b, t| {
            b.iter(|| ui::collect_cached_rows(t, "file_5", None, true, None, None, None))
        });

        let text_cache = ui::build_text_match_cache(&tree, "file_5");
        group.bench_with_input(BenchmarkId::new("filter_hit_cached", n), &tree, |b, t| {
            b.iter(|| {
                ui::collect_cached_rows(t, "file_5", None, true, Some(&text_cache), None, None)
            })
        });

        group.bench_with_input(
            BenchmarkId::new("filter_miss_uncached", n),
            &tree,
            |b, t| {
                b.iter(|| {
                    ui::collect_cached_rows(t, "nonexistent_zzz", None, true, None, None, None)
                })
            },
        );

        let miss_cache = ui::build_text_match_cache(&tree, "nonexistent_zzz");
        group.bench_with_input(BenchmarkId::new("filter_miss_cached", n), &tree, |b, t| {
            b.iter(|| {
                ui::collect_cached_rows(
                    t,
                    "nonexistent_zzz",
                    None,
                    true,
                    Some(&miss_cache),
                    None,
                    None,
                )
            })
        });
    }

    // With category filter — uncached vs cached
    {
        let tree = build_expanded_tree(500, 20);
        let n = count_nodes(&tree);

        group.bench_with_input(
            BenchmarkId::new("category_video_uncached", n),
            &tree,
            |b, t| {
                b.iter(|| {
                    ui::collect_cached_rows(
                        t,
                        "",
                        Some(categories::FileCategory::Video),
                        true,
                        None,
                        None,
                        None,
                    )
                })
            },
        );

        let cat_cache = ui::build_category_match_cache(&tree, categories::FileCategory::Video);
        group.bench_with_input(
            BenchmarkId::new("category_video_cached", n),
            &tree,
            |b, t| {
                b.iter(|| {
                    ui::collect_cached_rows(
                        t,
                        "",
                        Some(categories::FileCategory::Video),
                        true,
                        None,
                        Some(&cat_cache),
                        None,
                    )
                })
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// auto_expand — runs once after scan
// ---------------------------------------------------------------------------

fn bench_auto_expand(c: &mut Criterion) {
    let mut group = c.benchmark_group("auto_expand");
    group.sample_size(20);

    fn build_skewed(depth: usize, breadth: usize) -> FileNode {
        if depth == 0 {
            return make_leaf("leaf.dat", 1024);
        }
        let mut children: Vec<FileNode> = (0..breadth - 1)
            .map(|i| make_leaf(&format!("small_{i}.dat"), 100))
            .collect();
        children.push(build_skewed(depth - 1, breadth));
        make_dir(&format!("level_{depth}"), children)
    }

    for &(depth, breadth) in &[(5, 100), (10, 50), (20, 20)] {
        let label = format!("d{depth}_b{breadth}");
        group.bench_function(BenchmarkId::new("skewed", &label), |b| {
            b.iter_batched(
                || {
                    let mut t = build_skewed(depth, breadth);
                    t.set_expanded(true);
                    t
                },
                |mut t| {
                    tree::auto_expand(&mut t, 0, 5);
                    t
                },
                criterion::BatchSize::SmallInput,
            )
        });
    }

    for &n_dirs in &[500, 2000, 5000] {
        group.bench_function(BenchmarkId::new("wide", n_dirs), |b| {
            b.iter_batched(
                || {
                    let mut t = build_categorized_tree(n_dirs, 20);
                    t.set_expanded(true);
                    t
                },
                |mut t| {
                    tree::auto_expand(&mut t, 0, 5);
                    t
                },
                criterion::BatchSize::LargeInput,
            )
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// compute_stats — category sidebar
// ---------------------------------------------------------------------------

fn bench_compute_stats(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_stats");
    group.sample_size(20);

    for &(n_dirs, files_per_dir) in &[(100, 10), (500, 20), (2000, 20)] {
        let tree = build_categorized_tree(n_dirs, files_per_dir);
        let n = count_nodes(&tree);

        group.bench_with_input(BenchmarkId::new("nodes", n), &tree, |b, t| {
            b.iter(|| categories::compute_stats(t))
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// node_matches_category — recursive category filter
// ---------------------------------------------------------------------------

fn bench_node_matches_category(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_matches_category");
    group.sample_size(20);

    for &(n_dirs, files_per_dir) in &[(500, 20), (2000, 20)] {
        let tree = build_categorized_tree(n_dirs, files_per_dir);
        let n = count_nodes(&tree);

        group.bench_with_input(BenchmarkId::new("hit_video", n), &tree, |b, t| {
            b.iter(|| categories::node_matches_category(t, categories::FileCategory::Video))
        });

        group.bench_with_input(BenchmarkId::new("hit_code", n), &tree, |b, t| {
            b.iter(|| categories::node_matches_category(t, categories::FileCategory::Code))
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// build_filter_caches — runs on every search keystroke / category change
// ---------------------------------------------------------------------------

fn bench_build_filter_caches(c: &mut Criterion) {
    let mut group = c.benchmark_group("build_filter_caches");
    group.sample_size(20);

    for &(n_dirs, files_per_dir) in &[(500, 20), (2000, 20), (5000, 20)] {
        let tree = build_categorized_tree(n_dirs, files_per_dir);
        let n = count_nodes(&tree);

        group.bench_with_input(BenchmarkId::new("text_cache_hit", n), &tree, |b, t| {
            b.iter(|| ui::build_text_match_cache(t, "file_5"))
        });

        group.bench_with_input(BenchmarkId::new("text_cache_miss", n), &tree, |b, t| {
            b.iter(|| ui::build_text_match_cache(t, "nonexistent_zzz"))
        });

        group.bench_with_input(
            BenchmarkId::new("category_cache_video", n),
            &tree,
            |b, t| b.iter(|| ui::build_category_match_cache(t, categories::FileCategory::Video)),
        );

        group.bench_with_input(BenchmarkId::new("category_cache_code", n), &tree, |b, t| {
            b.iter(|| ui::build_category_match_cache(t, categories::FileCategory::Code))
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// full_filter_pipeline — cache build + collect_cached_rows end-to-end
// ---------------------------------------------------------------------------

fn bench_full_filter_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_filter_pipeline");
    group.sample_size(20);

    for &(n_dirs, files_per_dir) in &[(500, 20), (2000, 20)] {
        let tree = build_expanded_tree(n_dirs, files_per_dir);
        let n = count_nodes(&tree);

        group.bench_with_input(BenchmarkId::new("text_search_e2e", n), &tree, |b, t| {
            b.iter(|| {
                let cache = ui::build_text_match_cache(t, "file_5");
                ui::collect_cached_rows(t, "file_5", None, true, Some(&cache), None, None)
            })
        });

        group.bench_with_input(BenchmarkId::new("category_filter_e2e", n), &tree, |b, t| {
            b.iter(|| {
                let cache = ui::build_category_match_cache(t, categories::FileCategory::Video);
                ui::collect_cached_rows(
                    t,
                    "",
                    Some(categories::FileCategory::Video),
                    true,
                    None,
                    Some(&cache),
                    None,
                )
            })
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// collect_cached_rows at larger scale (100K+ nodes)
// ---------------------------------------------------------------------------

fn bench_collect_rows_large(c: &mut Criterion) {
    let mut group = c.benchmark_group("collect_cached_rows_large");
    group.sample_size(10);

    {
        let tree = build_expanded_tree(5000, 20);
        let n = count_nodes(&tree);

        group.bench_with_input(BenchmarkId::new("all_expanded", n), &tree, |b, t| {
            b.iter(|| ui::collect_cached_rows(t, "", None, true, None, None, None))
        });
    }

    {
        let mut tree = build_categorized_tree(5000, 20);
        tree.set_expanded(true);
        let n = count_nodes(&tree);

        group.bench_with_input(BenchmarkId::new("root_only", n), &tree, |b, t| {
            b.iter(|| ui::collect_cached_rows(t, "", None, true, None, None, None))
        });
    }

    {
        let tree = build_expanded_tree(5000, 20);
        let n = count_nodes(&tree);
        let text_cache = ui::build_text_match_cache(&tree, "file_5");

        group.bench_with_input(BenchmarkId::new("filter_cached", n), &tree, |b, t| {
            b.iter(|| {
                ui::collect_cached_rows(t, "file_5", None, true, Some(&text_cache), None, None)
            })
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Criterion groups
// ---------------------------------------------------------------------------

criterion_group!(
    benches,
    // Node matching / search
    bench_node_matches,
    bench_count_selected,
    // Sorting
    bench_sort_by_size,
    // Tree walks
    bench_tree_walks,
    // Selection
    bench_selection_ops,
    // Frame hot path
    bench_collect_visible_paths,
    bench_collect_rows_large,
    // Filtering
    bench_build_filter_caches,
    bench_full_filter_pipeline,
    // Category
    bench_node_matches_category,
    bench_compute_stats,
    // Auto expand
    bench_auto_expand,
);
criterion_main!(benches);
