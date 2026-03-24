use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use disk_cleaner::categories;
use disk_cleaner::tree::{self, DirNode, FileLeaf, FileNode};
use disk_cleaner::ui;

// ---------------------------------------------------------------------------
// Synthetic tree builders
// ---------------------------------------------------------------------------

fn make_leaf(name: &str, size: u64) -> FileNode {
    FileNode::File(FileLeaf {
        name: name.into(),
        size,
    })
}

fn make_dir(name: &str, children: Vec<FileNode>) -> FileNode {
    let size = children.iter().map(|c| c.size()).sum();
    FileNode::Dir(DirNode {
        name: name.into(),
        size,
        children,
        expanded: false,
    })
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

// ---------------------------------------------------------------------------
// Tree view benchmarks: collect_visible_paths (frame hot path proxy)
// ---------------------------------------------------------------------------

fn bench_collect_visible_paths(c: &mut Criterion) {
    let mut group = c.benchmark_group("collect_visible_paths");
    group.sample_size(20);

    // All expanded — worst case for tree view rendering
    for &(n_dirs, files_per_dir) in &[(100, 10), (500, 20), (2000, 20)] {
        let tree = build_expanded_tree(n_dirs, files_per_dir);
        let n = count_nodes(&tree);

        group.bench_with_input(BenchmarkId::new("all_expanded", n), &tree, |b, t| {
            b.iter(|| {
                let mut result = Vec::new();
                ui::collect_visible_paths(t, "", None, true, &mut result);
                result
            })
        });
    }

    // Root only expanded (default after scan) — best case
    for &(n_dirs, files_per_dir) in &[(500, 20), (2000, 20)] {
        let mut tree = build_categorized_tree(n_dirs, files_per_dir);
        tree.set_expanded(true);
        let n = count_nodes(&tree);

        group.bench_with_input(BenchmarkId::new("root_only", n), &tree, |b, t| {
            b.iter(|| {
                let mut result = Vec::new();
                ui::collect_visible_paths(t, "", None, true, &mut result);
                result
            })
        });
    }

    // With text filter active — forces node_matches on every subtree
    {
        let tree = build_expanded_tree(500, 20);
        let n = count_nodes(&tree);

        group.bench_with_input(BenchmarkId::new("filter_hit", n), &tree, |b, t| {
            b.iter(|| {
                let mut result = Vec::new();
                ui::collect_visible_paths(t, "file_5", None, true, &mut result);
                result
            })
        });

        group.bench_with_input(BenchmarkId::new("filter_miss", n), &tree, |b, t| {
            b.iter(|| {
                let mut result = Vec::new();
                ui::collect_visible_paths(t, "nonexistent_zzz", None, true, &mut result);
                result
            })
        });
    }

    // With category filter
    {
        let tree = build_expanded_tree(500, 20);
        let n = count_nodes(&tree);

        group.bench_with_input(BenchmarkId::new("category_video", n), &tree, |b, t| {
            b.iter(|| {
                let mut result = Vec::new();
                ui::collect_visible_paths(
                    t,
                    "",
                    Some(categories::FileCategory::Video),
                    true,
                    &mut result,
                );
                result
            })
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// auto_expand — runs once after scan, reported as slow
// ---------------------------------------------------------------------------

fn bench_auto_expand(c: &mut Criterion) {
    let mut group = c.benchmark_group("auto_expand");
    group.sample_size(20);

    // Skewed tree where one child dominates at each level (worst case for auto_expand
    // since it expands deeply through the dominant branch).
    fn build_skewed(depth: usize, breadth: usize) -> FileNode {
        if depth == 0 {
            return make_leaf("leaf.dat", 1024);
        }
        let mut children: Vec<FileNode> = (0..breadth - 1)
            .map(|i| make_leaf(&format!("small_{i}.dat"), 100))
            .collect();
        // One dominant child
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

    // Wide flat tree (many dirs at root)
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
// node_matches_category — recursive category filter used in both views
// ---------------------------------------------------------------------------

fn bench_node_matches_category(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_matches_category");
    group.sample_size(20);

    for &(n_dirs, files_per_dir) in &[(500, 20), (2000, 20)] {
        let tree = build_categorized_tree(n_dirs, files_per_dir);
        let n = count_nodes(&tree);

        // Hit case: Video files exist in every directory
        group.bench_with_input(BenchmarkId::new("hit_video", n), &tree, |b, t| {
            b.iter(|| categories::node_matches_category(t, categories::FileCategory::Video))
        });

        // Miss case: no Archive files if we filter them out
        // (Actually archives exist — use a category that doesn't exist)
        // All exts in our tree: rs, mp4, jpg, mp3, pdf, zip, dat, log, toml, py
        // Image category includes jpg so that's a hit. Let's just bench both common cases.
        group.bench_with_input(BenchmarkId::new("hit_code", n), &tree, |b, t| {
            b.iter(|| categories::node_matches_category(t, categories::FileCategory::Code))
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
);
criterion_main!(benches);
