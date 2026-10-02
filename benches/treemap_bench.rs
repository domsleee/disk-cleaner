use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use disk_cleaner::tree::{DirNode, FileLeaf, FileNode};
use disk_cleaner::treemap;
use eframe::egui;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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

/// Build a tree mimicking /Applications: many top-level dirs with deep children.
fn build_applications_like(n_apps: usize, files_per_app: usize) -> FileNode {
    let exts = [
        "rs", "dylib", "plist", "png", "strings", "nib", "json", "dat", "xml", "js",
    ];
    let apps: Vec<FileNode> = (0..n_apps)
        .map(|i| {
            let files: Vec<FileNode> = (0..files_per_app)
                .map(|j| {
                    let ext = exts[j % exts.len()];
                    make_leaf(&format!("file_{j}.{ext}"), (j as u64 + 1) * 4096)
                })
                .collect();
            make_dir(&format!("App_{i:03}.app"), files)
        })
        .collect();
    make_dir("/Applications", apps)
}

fn count_nodes(node: &FileNode) -> usize {
    1 + node.children().iter().map(count_nodes).sum::<usize>()
}

// ---------------------------------------------------------------------------
// build_treemap_cache benchmarks
// ---------------------------------------------------------------------------

fn bench_build_cache(c: &mut Criterion) {
    let mut group = c.benchmark_group("treemap_build_cache");
    group.sample_size(20);

    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1200.0, 700.0));

    // Small: 50 apps × 20 files = 1050 nodes
    {
        let tree = build_applications_like(50, 20);
        let n = count_nodes(&tree);
        group.bench_with_input(BenchmarkId::new("apps", n), &tree, |b, t| {
            b.iter(|| treemap::build_treemap_cache(t, &None, None, true, rect))
        });
    }

    // Medium: 100 apps × 100 files = 10100 nodes
    {
        let tree = build_applications_like(100, 100);
        let n = count_nodes(&tree);
        group.bench_with_input(BenchmarkId::new("apps", n), &tree, |b, t| {
            b.iter(|| treemap::build_treemap_cache(t, &None, None, true, rect))
        });
    }

    // Large: 200 apps × 200 files = 40200 nodes
    {
        let tree = build_applications_like(200, 200);
        let n = count_nodes(&tree);
        group.bench_with_input(BenchmarkId::new("apps", n), &tree, |b, t| {
            b.iter(|| treemap::build_treemap_cache(t, &None, None, true, rect))
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// find_node + breadcrumbs at scale (tree_memory.rs only tests 1.1K nodes)
// ---------------------------------------------------------------------------

fn bench_navigation_at_scale(c: &mut Criterion) {
    let mut group = c.benchmark_group("treemap_navigation_scale");
    group.sample_size(20);

    for &(n_apps, files_per_app) in &[(200, 100), (500, 200)] {
        let tree = build_applications_like(n_apps, files_per_app);
        let n = count_nodes(&tree);

        // Shallow zoom (top-level dir)
        let shallow = std::path::PathBuf::from("/Applications/App_000.app");
        group.bench_with_input(BenchmarkId::new("find_node_shallow", n), &tree, |b, t| {
            b.iter(|| treemap::find_node(t, &shallow))
        });

        // Deep zoom (near end — worst case traversal)
        let deep = std::path::PathBuf::from(format!("/Applications/App_{:03}.app", n_apps - 1));
        group.bench_with_input(BenchmarkId::new("find_node_deep", n), &tree, |b, t| {
            b.iter(|| treemap::find_node(t, &deep))
        });

        // Miss (nonexistent path)
        let miss = std::path::PathBuf::from("/Applications/NotAnApp.app");
        group.bench_with_input(BenchmarkId::new("find_node_miss", n), &tree, |b, t| {
            b.iter(|| treemap::find_node(t, &miss))
        });

        // Breadcrumbs at scale
        group.bench_with_input(BenchmarkId::new("breadcrumbs", n), &tree, |b, t| {
            b.iter(|| treemap::breadcrumbs(t, &deep))
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// squarify layout algorithm (pure computation, no tree)
// ---------------------------------------------------------------------------

fn bench_squarify(c: &mut Criterion) {
    let sizes_100: Vec<f64> = (1..=100).rev().map(|i| i as f64).collect();
    c.bench_function("squarify_100_items", |b| {
        b.iter(|| treemap::squarify(&sizes_100, 0.0, 0.0, 1200.0, 800.0))
    });

    let sizes_1000: Vec<f64> = (1..=1000).rev().map(|i| i as f64).collect();
    c.bench_function("squarify_1000_items", |b| {
        b.iter(|| treemap::squarify(&sizes_1000, 0.0, 0.0, 1200.0, 800.0))
    });

    let sizes_10k: Vec<f64> = (1..=10_000).rev().map(|i| i as f64).collect();
    c.bench_function("squarify_10000_items", |b| {
        b.iter(|| treemap::squarify(&sizes_10k, 0.0, 0.0, 1200.0, 800.0))
    });
}

criterion_group!(
    benches,
    bench_build_cache,
    bench_navigation_at_scale,
    bench_squarify,
);
criterion_main!(benches);
