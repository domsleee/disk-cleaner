#!/usr/bin/env bash
# Compare scan performance between two git refs using bench_perf --json.
#
# Usage:
#   scripts/bench_compare.sh <ref-a> <ref-b> [OPTIONS]
#
# Options:
#   --target PATH   Directory to scan (default: ~/git)
#   --runs N        Measured iterations per ref (default: 5)
#   --warmup N      Warmup iterations per ref (default: 1)
#
# Examples:
#   scripts/bench_compare.sh main feat/trivial-perf-wins
#   scripts/bench_compare.sh main HEAD --target ~
#   scripts/bench_compare.sh v0.2.0 v0.3.0 --runs 10

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
OUTDIR="$REPO_ROOT/target/bench-compare"
RUNS=5
WARMUP=1
TARGET=""

# Track worktrees for cleanup on exit
WORKTREES_TO_CLEAN=()

cleanup() {
    for wt in "${WORKTREES_TO_CLEAN[@]}"; do
        if [[ -d "$wt" ]]; then
            echo "Cleaning up worktree: $wt" >&2
            git worktree remove --force "$wt" 2>/dev/null || rm -rf "$wt"
        fi
    done
}
trap cleanup EXIT

# --- Argument parsing ---

if [[ $# -lt 2 ]]; then
    echo "Usage: bench_compare.sh <ref-a> <ref-b> [--target PATH] [--runs N] [--warmup N]" >&2
    exit 1
fi

REF_A="$1"; shift
REF_B="$1"; shift

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target)  TARGET="$2"; shift 2 ;;
        --runs)    RUNS="$2";   shift 2 ;;
        --warmup)  WARMUP="$2"; shift 2 ;;
        *)         echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

# Resolve refs to short SHAs for display/filenames
SHA_A="$(git rev-parse --short "$REF_A")"
SHA_B="$(git rev-parse --short "$REF_B")"

mkdir -p "$OUTDIR"

# --- Build and run bench_perf in a temporary worktree ---

run_bench() {
    local ref="$1"
    local label="$2"
    local json_out="$3"
    local worktree_dir
    worktree_dir="$(mktemp -d -t bench-compare-XXXXXX)"
    WORKTREES_TO_CLEAN+=("$worktree_dir")

    echo "--- [$label] Setting up worktree for $ref ($worktree_dir) ---" >&2

    # Create detached worktree so we don't conflict with existing branch checkouts
    git worktree add --detach "$worktree_dir" "$ref" 2>&1 | sed 's/^/  /' >&2

    echo "  Building bench_perf (release)..." >&2
    (cd "$worktree_dir" && cargo build --release --bin bench_perf 2>&1 | tail -3 | sed 's/^/  /' >&2)

    local bench_bin="$worktree_dir/target/release/bench_perf"
    if [[ ! -x "$bench_bin" ]]; then
        echo "ERROR: bench_perf binary not found at $bench_bin" >&2
        return 1
    fi

    local bench_args=(--json --no-startup --warmup "$WARMUP" --runs "$RUNS")
    if [[ -n "$TARGET" ]]; then
        bench_args+=("$TARGET")
    fi

    echo "  Running: bench_perf ${bench_args[*]}" >&2
    "$bench_bin" "${bench_args[@]}" > "$json_out"
    echo "  Saved JSON to $json_out" >&2

    # Clean up worktree immediately to free disk space
    git worktree remove --force "$worktree_dir" 2>/dev/null || true
}

# --- Run both refs ---

JSON_A="$OUTDIR/${SHA_A}.json"
JSON_B="$OUTDIR/${SHA_B}.json"

run_bench "$REF_A" "A" "$JSON_A"
echo >&2
run_bench "$REF_B" "B" "$JSON_B"
echo >&2

# --- Compare results with python3 ---

python3 - "$REF_A" "$SHA_A" "$JSON_A" "$REF_B" "$SHA_B" "$JSON_B" <<'PYEOF'
import json, sys

ref_a, sha_a, path_a = sys.argv[1], sys.argv[2], sys.argv[3]
ref_b, sha_b, path_b = sys.argv[4], sys.argv[5], sys.argv[6]

with open(path_a) as f:
    a = json.load(f)
with open(path_b) as f:
    b = json.load(f)

REGRESSION_THRESHOLD = 5.0  # percent

def pct_change(old, new):
    if old == 0:
        return 0.0
    return ((new - old) / old) * 100.0

def flag(pct):
    """Negative pct = faster (good), positive = slower (regression)."""
    if pct > REGRESSION_THRESHOLD:
        return " *** REGRESSION"
    elif pct < -REGRESSION_THRESHOLD:
        return " (improved)"
    return ""

def fmt_pct(pct):
    sign = "+" if pct >= 0 else ""
    return f"{sign}{pct:.1f}%"

print(f"{'='*64}")
print(f"  Benchmark Comparison")
print(f"  A: {ref_a} ({sha_a})")
print(f"  B: {ref_b} ({sha_b})")
print(f"  Target: {a['path']}")
print(f"  Runs: {a['runs']}  Warmup: {a['warmup']}")
print(f"  Files: {a['file_count']:,}  Size: {a['total_size']:,} bytes")
print(f"{'='*64}")
print()

# Scan duration comparison
print("  Scan Duration (seconds)")
print(f"  {'':20s} {'A':>10s} {'B':>10s} {'Change':>10s}")
print(f"  {'-'*52}")
for metric in ["median", "mean", "min", "max"]:
    va = a["scan_s"][metric]
    vb = b["scan_s"][metric]
    pct = pct_change(va, vb)
    marker = flag(pct)
    print(f"  {metric:20s} {va:10.3f} {vb:10.3f} {fmt_pct(pct):>10s}{marker}")

print(f"  {'stddev':20s} {a['scan_s']['stddev']:10.3f} {b['scan_s']['stddev']:10.3f}")
print(f"  {'cv%':20s} {a['scan_s']['cv_pct']:10.1f} {b['scan_s']['cv_pct']:10.1f}")
print()

# Post-scan comparison
print("  Post-Scan Duration (ms)")
print(f"  {'':20s} {'A':>10s} {'B':>10s} {'Change':>10s}")
print(f"  {'-'*52}")
for metric in ["median", "mean", "min", "max"]:
    va = a["post_scan_ms"][metric]
    vb = b["post_scan_ms"][metric]
    pct = pct_change(va, vb)
    marker = flag(pct)
    print(f"  {metric:20s} {va:10.1f} {vb:10.1f} {fmt_pct(pct):>10s}{marker}")

print(f"  {'stddev':20s} {a['post_scan_ms']['stddev']:10.1f} {b['post_scan_ms']['stddev']:10.1f}")
print(f"  {'cv%':20s} {a['post_scan_ms']['cv_pct']:10.1f} {b['post_scan_ms']['cv_pct']:10.1f}")
print()

# Summary verdict
scan_pct = pct_change(a["scan_s"]["median"], b["scan_s"]["median"])
post_pct = pct_change(a["post_scan_ms"]["median"], b["post_scan_ms"]["median"])

regressions = []
if scan_pct > REGRESSION_THRESHOLD:
    regressions.append(f"scan median {fmt_pct(scan_pct)}")
if post_pct > REGRESSION_THRESHOLD:
    regressions.append(f"post-scan median {fmt_pct(post_pct)}")

if regressions:
    print(f"  *** REGRESSIONS DETECTED: {', '.join(regressions)}")
else:
    scan_label = fmt_pct(scan_pct)
    print(f"  No regressions (>5%). Scan median: {scan_label}, post-scan median: {fmt_pct(post_pct)}")

print(f"{'='*64}")
print(f"  Raw JSON: {path_a}")
print(f"            {path_b}")
PYEOF
