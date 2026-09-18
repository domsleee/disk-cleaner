# Benchmarks

Benchmarks cover scanning, tree view, and treemap rendering, with additional
suites for regression checks and scan comparisons.

## Categories

### Scanning (`scan_bench`)

Measures disk I/O and tree construction using deep nesting, a 20k-file fixture,
real directory scans (`sample_size=10`), and directory-heavy layouts.
Also tracks memory per node.

```sh
cargo bench --bench scan_bench
```

### Tree view (`tree_bench`)

Measures the per-frame hot path: `collect_cached_rows`, `node_matches`,
find/toggle/expand/remove tree walks, selection operations, filter caches,
category matching, `auto_expand`, and `compute_stats`.

```sh
cargo bench --bench tree_bench
```

### Treemap (`treemap_bench`)

Measures `build_treemap_cache`, `squarify` layout, `find_node`/`breadcrumbs`
navigation, label formatting, and `FontId` allocation.

```sh
cargo bench --bench treemap_bench
```

## Special-purpose suites

### Regression gate (`regression_bench`)

Checks bytes/node and scan time against hard thresholds using a fixed
50K-file CI fixture. Use `scan_bench` for iteration.

```sh
cargo bench --bench regression_bench
```

### Statistical scan (`stat_bench`)

Runs N full scans of a real directory (default: `$HOME`, 10 runs), reporting
mean, standard deviation, and confidence intervals for scan time and memory.
Uses a custom `main()`, not Criterion.

```sh
cargo bench --bench stat_bench                              # default: ~/
BENCH_DIR=/path/to/scan BENCH_RUNS=5 cargo bench --bench stat_bench
```

### Cold-cache scan (`coldscan.sh`, macOS)

Detaches and reattaches an APFS sparse-image fixture between runs to evict
the volume's vnode/metadata cache without sudo. The backing file may remain
in the boot volume's page cache, so this measures a cold mount, not necessarily
cold storage. It provides a consistent A/B baseline, not a disk-seek benchmark.
Suites other than the cold-cache scans use warm caches.

```sh
./benches/coldscan.sh                  # 5 cold runs, mean ± stddev
RUNS=10 SCAN_THREADS=16 ./benches/coldscan.sh
./benches/coldscan.sh --clean          # remove cached fixture image
```

### Cold-cache scan (`coldcache.ps1`, Windows)

Dismounts and remounts a VHDX-backed NTFS volume before each scan, giving it
a cold NTFS metadata cache without rebooting. Use this for first-scan latency
experiments, such as traversal order, handle pipelining, and I/O depth:
warm-cache benchmarks understate I/O-bound improvements.

Requires elevated PowerShell. The fixture is created once in
`target/coldcache/` and reused across builds to preserve the on-disk layout.

```powershell
# 5 cold runs against a generated 50k-file fixture (created on first use)
.\benches\coldcache.ps1

# Copy a real tree, run 10 times, and include hot re-scans
.\benches\coldcache.ps1 -SourcePath C:\Users\me\projects -Runs 10 -AlsoHot

# Also evict the backing file from the host page cache (block-level cold)
.\benches\coldcache.ps1 -PurgeStandby

# Compare a saved binary against the same fixture
.\benches\coldcache.ps1 -Exe C:\temp\scan_only_main.exe

.\benches\coldcache.ps1 -Rebuild   # regenerate the fixture
.\benches\coldcache.ps1 -Cleanup   # delete the fixture
```

### Paired A/B scan speed (`ab_pairs.ps1`, Windows)

Compares two builds with randomized warm-cache AB/BA pairs, reporting the
geometric mean ratio and a bootstrap confidence interval. Each measured pair
must have matching file counts and byte totals.

`-Pairs` must be even and at least 20 to balance order and limit small-sample
bootstrap problems. This minimum does not guarantee 95% coverage: the
percentile interval is approximate and assumes independent, representative
pairs. Use a stable target and inspect interval width; an improvement interval
including zero is inconclusive.

Do not use this harness to compare raw MFT scanning with directory walking.
They evict each other's caches, biasing alternating pairs, and their legitimate
file-count difference aborts the harness. See
[`src/scanner/README.md`](../src/scanner/README.md).

`scan_only` prints `BENCH scan_ms=... drop_ms=...`. `scan_ms` covers the full
`scan_directory()` call, including thread-pool initialization, dedup-set cleanup,
and sorting. `drop_ms` covers returned-tree teardown (~200 ms for a 1.5M-file
tree), not all process cleanup. Allocator changes can affect both. Compare
`scan_ms` for scan completion latency; process wall-clock also includes tree
teardown and process startup/exit.

Relative executable, scan, and CSV paths resolve from PowerShell's current
location. `-CsvPath` clears the CSV at startup and saves each completed pair
immediately, preserving earlier measurements if a later pair fails.
Failed or mismatched pairs are excluded. A partial CSV contains raw data only;
incomplete runs produce no overall verdict.

```powershell
cargo build --profile release-dist --features internal-tools --bin scan_only
Copy-Item target\release-dist\scan_only.exe $env:TEMP\scan_a.exe
# Switch branch and rebuild, then:
.\benches\ab_pairs.ps1 -ExeA $env:TEMP\scan_a.exe -ExeB target\release-dist\scan_only.exe -ScanPath C:\Users\me\projects
.\benches\ab_pairs.ps1 ... -Pairs 40 -CsvPath runs.csv   # more samples
```

Harness regression checks compile a small native fixture with `rustc`:

```powershell
powershell -NoProfile -File benches/ab_pairs.Tests.ps1
pwsh -NoProfile -File benches/ab_pairs.Tests.ps1
```

## Comparing branches

```sh
# Save a baseline on main
./benches/baseline.sh save

# Switch to your branch and compare
./benches/baseline.sh compare

# Or compare two refs directly
./benches/ab.sh main my-feature-branch
```

## Competitive benchmarks

```sh
./benches/vs_dust.sh [PATH]   # disk-cleaner vs dust
./benches/vs_all.sh [PATH]    # disk-cleaner vs du, dust, ncdu (requires hyperfine)
./benches/fullscan.sh [PATH]  # wall-clock + peak RSS for a single scan
```