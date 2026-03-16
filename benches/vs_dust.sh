#!/usr/bin/env bash
# Compare disk-cleaner scan speed against dust.
# Requires: hyperfine, dust (cargo install du-dust)
#
# Usage: ./benches/vs_dust.sh [PATH]
#   Defaults to scanning the project directory.

set -euo pipefail

TARGET="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
SCAN_BIN="./target/release/scan_only"

echo "=== Building scan_only (release) ==="
cargo build --release --bin scan_only 2>&1

if ! command -v dust &>/dev/null; then
  echo "Error: dust not found. Install with: cargo install du-dust"
  exit 1
fi

if ! command -v hyperfine &>/dev/null; then
  echo "Error: hyperfine not found. Install with: cargo install hyperfine"
  exit 1
fi

echo ""
echo "=== Benchmarking: $TARGET ==="
echo ""

hyperfine --warmup 2 --runs 10 \
  "$SCAN_BIN $TARGET" \
  "dust --threads 1 $TARGET" \
  "dust --threads 8 $TARGET" \
  --command-name "disk-cleaner (single-threaded)" \
  --command-name "dust --threads 1" \
  --command-name "dust --threads 8"
