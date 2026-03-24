#!/usr/bin/env bash
# Full-disk scan benchmark — measures wall-clock time and peak memory usage.
#
# Usage: ./benches/fullscan.sh [PATH]
#   Defaults to scanning "/" (root filesystem).
#
# Reports:
#   - Files scanned and total size (from scan_only)
#   - Elapsed wall-clock time
#   - Peak resident set size (RSS)
#
# Requires: macOS (uses /usr/bin/time -l for RSS measurement)

set -euo pipefail

TARGET="${1:-/}"

cd "$(dirname "$0")/.."

echo "=== Full Scan Benchmark ==="
echo "Target: $TARGET"
echo ""

echo "Building scan_only (release)..."
cargo build --release --bin scan_only 2>&1
echo ""

echo "Scanning..."

# /usr/bin/time -l writes resource stats to stderr; scan_only writes results to stdout.
TIME_STATS=$(mktemp)
SCAN_OUT=$(/usr/bin/time -l ./target/release/scan_only "$TARGET" 2>"$TIME_STATS")

REAL=$(awk '/real/{print $1}' "$TIME_STATS")
PEAK_RSS=$(awk '/maximum resident set size/{print $1}' "$TIME_STATS")
PEAK_MB=$(echo "scale=1; ${PEAK_RSS:-0} / 1048576" | bc 2>/dev/null || echo "?")

echo ""
echo "=== Results ==="
echo "Scan:     $SCAN_OUT"
echo "Elapsed:  ${REAL}s"
echo "Peak RSS: ${PEAK_MB} MB (${PEAK_RSS} bytes)"
echo "==============="

rm -f "$TIME_STATS"
