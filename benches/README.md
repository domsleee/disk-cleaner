# Benchmarks

Benchmarks are organised into three categories matching the app's main subsystems,
plus two special-purpose suites.

## Categories

### Scanning (`scan_bench`)

Disk I/O and tree construction. Synthetic fixtures (deep nesting + 20k-file),
real directory scans (sample_size=10), directory-heavy layouts, and memory-per-node tracking.

```sh
cargo bench --bench scan_bench
```

### Tree view (`tree_bench`)

Per-frame hot path for the tree view. `collect_cached_rows`, `node_matches`,
tree walks (find/toggle/expand/remove), selection ops, filter caches,
category matching, `auto_expand`, and `compute_stats`.

```sh
cargo bench --bench tree_bench
```

### Treemap (`treemap_bench`)

Treemap rendering. `build_treemap_cache`, `squarify` layout algorithm,
`find_node`/`breadcrumbs` navigation, label formatting, and `FontId` allocation.

```sh
cargo bench --bench treemap_bench
```

## Special-purpose suites

### Regression gate (`regression_bench`)

Fixed 50K-file synthetic fixture for CI. Reports bytes/node and scan time
against hard thresholds. Not for iteration — use `scan_bench` instead.

```sh
cargo bench --bench regression_bench
```

### Statistical scan (`stat_bench`)

Runs N full scans of a real directory (default: `$HOME`, 10 runs) and reports
mean/stddev/CI for scan time and memory. Custom `main()`, not criterion.

```sh
cargo bench --bench stat_bench                              # default: ~/
BENCH_DIR=/path/to/scan BENCH_RUNS=5 cargo bench --bench stat_bench
```

### Cold-cache scan (`coldscan.sh`, macOS)

Cold metadata cache without sudo: fixture lives on an APFS sparse image,
detached/reattached between runs to evict the volume's vnode/metadata
cache. Note the image's backing file may stay in the boot volume's page
cache, so this measures a cold mount, not necessarily cold storage — it
is a consistent A/B baseline, not a disk-seek benchmark. All other
benches are warm-cache.

```sh
./benches/coldscan.sh                  # 5 cold runs, mean ± stddev
RUNS=10 SCAN_THREADS=16 ./benches/coldscan.sh
./benches/coldscan.sh --clean          # remove cached fixture image
```

## Comparing branches

```sh
# Save a baseline on main
./benches/baseline.sh save

# Switch to your branch, compare
./benches/baseline.sh compare

# Or A/B two refs directly
./benches/ab.sh main my-feature-branch
```

## Competitive benchmarks

```sh
./benches/vs_dust.sh [PATH]   # disk-cleaner vs dust
./benches/vs_all.sh [PATH]    # disk-cleaner vs du, dust, ncdu (requires hyperfine)
./benches/fullscan.sh [PATH]  # wall-clock + peak RSS for a single scan
```
