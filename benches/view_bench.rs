use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use disk_cleaner::categories;
use disk_cleaner::tree::{self, build_test_tree, dir, leaf, FileTree, NodeId, TestNode};
use disk_cleaner::ui;

// ---------------------------------------------------------------------------
// Synthetic tree builders
// ---------------------------------------------------------------------------

/// Wide tree with realistic file extensions for category benchmarks.
fn build_categorized_tree(n_dirs: usize, files_per_dir: usize) -> FileTree {
    let exts = [
        "rs", "mp4", "jpg", "mp3", "pdf", "zip", "dat", "log", "toml", "py",
    ];
    let dirs: Vec<TestNode> = (0..n_dirs)
        .map(|i| {
            let files: Vec<TestNode> = (0..files_per_dir)
                .map(|j| {
                    let ext = exts[j % exts.len()];
                    leaf(&format!("file_{j}.{ext}"), (j as u64 + 1) * 1024)
                })
                .collect();
            dir(&format!("dir_{i:05}"), files)
        })
        .collect();
    build_test_tree(dir("/root", dirs))
}

/// Build a tree where all directories are expanded.
fn build_expanded_tree(n_dirs: usize, files_per_dir: usize) -> FileTree {
    let mut tree = build_categorized_tree(n_dirs, files_per_dir);
    expand_all(&mut tree, tree.root());
    tree
}

fn expand_all(tree: &mut FileTree, id: NodeId) {
    if tree.is_dir(id) {
        tree.set_expanded(id, true);
        let children: Vec<NodeId> = tree.children(id).to_vec();
        for child in children {
            expand_all(tree, child);
        }
    }
}

fn count_nodes(tree: &FileTree, id: NodeId) -> usize {
    1 + tree.children(id).iter().map(|&c| count_nodes(tree, c)).sum::<usize>()
}

// ---------------------------------------------------------------------------
// Tree view benchmarks: collect_cached_rows (frame hot path proxy)
// ---------------------------------------------------------------------------

fn bench_collect_visible_paths(c: &mut Criterion) {
    let mut group = c.benchmark_group("collect_cached_rows");
    group.sample_size(20);

    // All expanded — worst case for tree view rendering
    for &(n_dirs, files_per_dir) in &[(100, 10), (500, 20), (2000, 20)] {
        let tree = build_expanded_tree(n_dirs, files_per_dir);
        let n = count_nodes(&tree, tree.root());

        group.bench_with_input(BenchmarkId::new("all_expanded", n), &tree, |b, t| {
            b.iter(|| ui::collect_cached_rows(t, "", None, true, None, None, None))
        });
    }

    // Root only expanded (default after scan) — best case
    for &(n_dirs, files_per_dir) in &[(500, 20), (2000, 20)] {
        let mut tree = build_categorized_tree(n_dirs, files_per_dir);
        tree.set_expanded(tree.root(), true);
        let n = count_nodes(&tree, tree.root());

        group.bench_with_input(BenchmarkId::new("root_only", n), &tree, |b, t| {
            b.iter(|| ui::collect_cached_rows(t, "", None, true, None, None, None))
        });
    }

    // With text filter — uncached (O(N^2)) vs cached (O(N))
    {
        let tree = build_expanded_tree(500, 20);
        let n = count_nodes(&tree, tree.root());

        group.bench_with_input(BenchmarkId::new("filter_hit_uncached", n), &tree, |b, t| {
            b.iter(|| ui::collect_cached_rows(t, "file_5", None, true, None, None, None))
        });

        let text_cache = ui::build_text_match_cache(&tree, "file_5");
        group.bench_with_input(BenchmarkId::new("filter_hit_cached", n), &tree, |b, t| {
            b.iter(|| ui::collect_cached_rows(t, "file_5", None, true, Some(&text_cache), None, None))
        });

        group.bench_with_input(
            BenchmarkId::new("filter_miss_uncached", n),
            &tree,
            |b, t| b.iter(|| ui::collect_cached_rows(t, "nonexistent_zzz", None, true, None, None, None)),
        );

        let miss_cache = ui::build_text_match_cache(&tree, "nonexistent_zzz");
        group.bench_with_input(BenchmarkId::new("filter_miss_cached", n), &tree, |b, t| {
            b.iter(|| {
                ui::collect_cached_rows(t, "nonexistent_zzz", None, true, Some(&miss_cache), None, None)
            })
        });
    }

    // With category filter — uncached vs cached
    {
        let tree = build_expanded_tree(500, 20);
        let n = count_nodes(&tree, tree.root());

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
// auto_expand — runs once after scan, reported as slow
// ---------------------------------------------------------------------------

fn bench_auto_expand(c: &mut Criterion) {
    let mut group = c.benchmark_group("auto_expand");
    group.sample_size(20);

    // Skewed tree where one child dominates at each level
    fn build_skewed(depth: usize, breadth: usize) -> TestNode {
        if depth == 0 {
            return leaf("leaf.dat", 1024);
        }
        let mut children: Vec<TestNode> = (0..breadth - 1)
            .map(|i| leaf(&format!("small_{i}.dat"), 100))
            .collect();
        children.push(build_skewed(depth - 1, breadth));
        dir(&format!("level_{depth}"), children)
    }

    for &(depth, breadth) in &[(5, 100), (10, 50), (20, 20)] {
        let label = format!("d{depth}_b{breadth}");
        group.bench_function(BenchmarkId::new("skewed", &label), |b| {
            b.iter_batched(
                || {
                    let mut t = build_test_tree(build_skewed(depth, breadth));
                    let root = t.root();
                    t.set_expanded(root, true);
                    t
                },
                |mut t| {
                    let root = t.root();
                    tree::auto_expand(&mut t, root, 0, 5);
                    t
                },
                criterion::BatchSize::SmallInput,
            )
        });
    }

    // Wide flat tree (many dirs at root)
    for &n_dirs in &[500, 2000, 5000] {
        group.bench_function(BenchmarkId::new("wide", n_dirs), |b| {
            b.iter_batched(
                || {
                    let mut t = build_categorized_tree(n_dirs, 20);
                    let root = t.root();
                    t.set_expanded(root, true);
                    t
                },
                |mut t| {
                    let root = t.root();
                    tree::auto_expand(&mut t, root, 0, 5);
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
        let n = count_nodes(&tree, tree.root());

        group.bench_with_input(BenchmarkId::new("nodes", n), &tree, |b, t| {
            b.iter(|| categories::compute_stats(t))
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// node_matches_category — recursive category filter used in both views
// ---------------------------------------------------------------------------

fn bench_node_matches_category(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_matches_category");
    group.sample_size(20);

    for &(n_dirs, files_per_dir) in &[(500, 20), (2000, 20)] {
        let tree = build_categorized_tree(n_dirs, files_per_dir);
        let n = count_nodes(&tree, tree.root());
        let root = tree.root();

        group.bench_with_input(BenchmarkId::new("hit_video", n), &tree, |b, t| {
            b.iter(|| categories::node_matches_category(t, root, categories::FileCategory::Video))
        });

        group.bench_with_input(BenchmarkId::new("hit_code", n), &tree, |b, t| {
            b.iter(|| categories::node_matches_category(t, root, categories::FileCategory::Code))
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// build_text_match_cache / build_category_match_cache — filter cache building
// ---------------------------------------------------------------------------

fn bench_build_filter_caches(c: &mut Criterion) {
    let mut group = c.benchmark_group("build_filter_caches");
    group.sample_size(20);

    for &(n_dirs, files_per_dir) in &[(500, 20), (2000, 20), (5000, 20)] {
        let tree = build_categorized_tree(n_dirs, files_per_dir);
        let n = count_nodes(&tree, tree.root());

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
// Full filter pipeline — cache build + collect_cached_rows end-to-end
// ---------------------------------------------------------------------------

fn bench_full_filter_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_filter_pipeline");
    group.sample_size(20);

    for &(n_dirs, files_per_dir) in &[(500, 20), (2000, 20)] {
        let tree = build_expanded_tree(n_dirs, files_per_dir);
        let n = count_nodes(&tree, tree.root());

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

    // 100K nodes — all expanded (worst case)
    {
        let tree = build_expanded_tree(5000, 20);
        let n = count_nodes(&tree, tree.root());

        group.bench_with_input(BenchmarkId::new("all_expanded", n), &tree, |b, t| {
            b.iter(|| ui::collect_cached_rows(t, "", None, true, None, None, None))
        });
    }

    // 100K nodes — root only expanded (best case, most realistic)
    {
        let mut tree = build_categorized_tree(5000, 20);
        let root = tree.root();
        tree.set_expanded(root, true);
        let n = count_nodes(&tree, root);

        group.bench_with_input(BenchmarkId::new("root_only", n), &tree, |b, t| {
            b.iter(|| ui::collect_cached_rows(t, "", None, true, None, None, None))
        });
    }

    // 100K nodes — with text filter + cache
    {
        let tree = build_expanded_tree(5000, 20);
        let n = count_nodes(&tree, tree.root());
        let text_cache = ui::build_text_match_cache(&tree, "file_5");

        group.bench_with_input(BenchmarkId::new("filter_cached", n), &tree, |b, t| {
            b.iter(|| ui::collect_cached_rows(t, "file_5", None, true, Some(&text_cache), None, None))
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_collect_visible_paths,
    bench_auto_expand,
    bench_compute_stats,
    bench_node_matches_category,
    bench_build_filter_caches,
    bench_full_filter_pipeline,
    bench_collect_rows_large,
);
criterion_main!(benches);
