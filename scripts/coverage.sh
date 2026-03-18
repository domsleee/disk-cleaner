#!/usr/bin/env bash
# Run test coverage with cargo-llvm-cov and print a summary.
# Usage:
#   ./scripts/coverage.sh          # summary table
#   ./scripts/coverage.sh --html   # open HTML report in browser
#   ./scripts/coverage.sh --json   # emit JSON for CI parsing

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

MODE="${1:-}"

case "$MODE" in
  --html)
    cargo llvm-cov --html
    echo "Report written to target/llvm-cov/html/index.html"
    open target/llvm-cov/html/index.html 2>/dev/null || true
    ;;
  --json)
    cargo llvm-cov --json
    ;;
  *)
    cargo llvm-cov --summary-only
    ;;
esac
