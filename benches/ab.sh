#!/usr/bin/env bash
# A/B Performance Benchmark — compare two git refs with criterion baselines.
#
# Usage:
#   ./benches/ab.sh                    # compare HEAD~1 (before) vs HEAD (after)
#   ./benches/ab.sh main feature-xyz   # compare main vs feature-xyz
#   ./benches/ab.sh abc123 def456      # compare two commits
#
# Requires: cargo, criterion benchmarks configured in Cargo.toml
#
# How it works:
#   1. Stashes any uncommitted changes
#   2. Checks out REF_A, builds + benchmarks → saves as "before" baseline
#   3. Checks out REF_B, builds + benchmarks → compares against "before"
#   4. Restores original branch + stash
#
# Criterion prints comparison results automatically when a baseline is set.
# Look for lines like:  "Performance has regressed." / "Performance has improved."

set -euo pipefail

REF_A="${1:-HEAD~1}"
REF_B="${2:-HEAD}"
BENCH_DIR="$(cd "$(dirname "$0")/.." && pwd)"

cd "$BENCH_DIR"

ORIGINAL_BRANCH=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || git rev-parse HEAD)
STASH_NEEDED=false

echo "=== A/B Performance Benchmark ==="
echo "  Before: $REF_A"
echo "  After:  $REF_B"
echo ""

# Stash uncommitted changes if any
if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "Stashing uncommitted changes..."
    git stash push -m "ab-bench-stash-$(date +%s)"
    STASH_NEEDED=true
fi

cleanup() {
    echo ""
    echo "=== Restoring original state ==="
    git checkout "$ORIGINAL_BRANCH" 2>/dev/null || git checkout -
    if $STASH_NEEDED; then
        echo "Restoring stashed changes..."
        git stash pop
    fi
}
trap cleanup EXIT

# --- Phase 1: Baseline (REF_A) ---
echo "=== Phase 1: Building baseline ($REF_A) ==="
git checkout "$REF_A" --quiet 2>/dev/null || git checkout "$REF_A"
echo ""

echo "Running benchmarks (saving as 'before')..."
cargo bench -- --save-baseline before 2>&1 | tee /tmp/ab-bench-before.log
echo ""

# --- Phase 2: Comparison (REF_B) ---
echo "=== Phase 2: Building comparison ($REF_B) ==="
git checkout "$REF_B" --quiet 2>/dev/null || git checkout "$REF_B"
echo ""

echo "Running benchmarks (comparing against 'before')..."
cargo bench -- --baseline before 2>&1 | tee /tmp/ab-bench-after.log
echo ""

echo "=== A/B Benchmark Complete ==="
echo ""
echo "Full logs:"
echo "  Before: /tmp/ab-bench-before.log"
echo "  After:  /tmp/ab-bench-after.log"
echo ""
echo "HTML reports: target/criterion/*/report/index.html"
