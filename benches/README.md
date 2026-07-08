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

### Cold-cache scan (`coldcache.ps1`, Windows)

Scans a VHDX-backed NTFS volume that is dismounted/remounted before every
run, so each scan starts with a cold NTFS metadata cache — no reboot needed.
Hot-cache benches understate I/O-bound improvements; use this for experiments
that target first-scan latency (traversal order, handle pipelining, I/O depth).
Requires an elevated PowerShell. The fixture VHDX is created once under
`target/coldcache/` and reused across builds, so A/B comparisons see the
identical on-disk layout.

```powershell
# 5 cold runs against a generated 50k-file fixture (creates it on first use)
.\benches\coldcache.ps1

# Fixture copied from a real tree, 10 runs, plus a hot re-scan for contrast
.\benches\coldcache.ps1 -SourcePath C:\Users\me\projects -Runs 10 -AlsoHot

# Evict the .vhdx backing file from the host page cache too (block-level cold)
.\benches\coldcache.ps1 -PurgeStandby

# A/B: benchmark a saved binary from another ref against the same fixture
.\benches\coldcache.ps1 -Exe C:\temp\scan_only_main.exe

.\benches\coldcache.ps1 -Rebuild   # regenerate the fixture
.\benches\coldcache.ps1 -Cleanup   # delete the fixture
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
