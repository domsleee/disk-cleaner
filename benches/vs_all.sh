#!/usr/bin/env bash
# Competitive scan speed benchmark: disk-cleaner vs du vs dust vs ncdu
# Requires: hyperfine, dust (cargo install du-dust), ncdu (brew install ncdu)
#
# Usage: ./benches/vs_all.sh [PATH] [RUNS]
#   PATH defaults to "/" (root filesystem).
#   RUNS defaults to 5.

set -euo pipefail

cd "$(dirname "$0")/.."

TARGET="${1:-/}"
RUNS="${2:-5}"
WARMUP=3
SCAN_BIN="./target/release/scan_only"
RESULTS_JSON=$(mktemp /tmp/bench-results-XXXXXX.json)
RESULTS_MD=$(mktemp /tmp/bench-results-XXXXXX.md)

echo "=== Competitive Scan Benchmark ==="
echo "Target:  $TARGET"
echo "Warmup:  $WARMUP runs"
echo "Timed:   $RUNS runs"
echo ""

# Build scan_only
echo "Building scan_only (release)..."
cargo build --release --bin scan_only 2>&1
echo ""

# Check dependencies
for cmd in hyperfine dust ncdu; do
  if ! command -v "$cmd" &>/dev/null; then
    echo "Error: $cmd not found."
    exit 1
  fi
done

echo "Tool versions:"
echo "  disk-cleaner: $($SCAN_BIN --version 2>&1 || echo 'unknown')"
echo "  du:           $(du --version 2>&1 | head -1 || echo 'BSD du')"
echo "  dust:         $(dust --version 2>&1)"
echo "  ncdu:         $(ncdu --version 2>&1)"
echo "  hyperfine:    $(hyperfine --version 2>&1)"
echo ""

echo "Running benchmarks..."
echo ""

hyperfine \
  --warmup "$WARMUP" \
  --runs "$RUNS" \
  --ignore-failure \
  --command-name "disk-cleaner (scan_only)" "$SCAN_BIN $TARGET" \
  --command-name "du -sh"                   "du -sh $TARGET 2>/dev/null" \
  --command-name "dust"                     "dust -d0 $TARGET 2>/dev/null" \
  --command-name "ncdu -0"                  "ncdu -0 -o /dev/null $TARGET 2>/dev/null" \
  --export-json "$RESULTS_JSON" \
  --export-markdown "$RESULTS_MD"

echo ""
echo "=== Results (Markdown) ==="
cat "$RESULTS_MD"
echo ""
echo "JSON results saved to: $RESULTS_JSON"
echo "Markdown results saved to: $RESULTS_MD"
