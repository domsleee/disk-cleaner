#!/usr/bin/env bash
# Save or compare against a named benchmark baseline.
#
# Usage:
#   ./benches/baseline.sh save [name]      # save current benchmarks as baseline (default: "main")
#   ./benches/baseline.sh compare [name]   # run benchmarks and compare against baseline
#   ./benches/baseline.sh                  # shortcut: compare against "main"
#
# Workflow:
#   1. On main branch:     ./benches/baseline.sh save
#   2. Make your changes
#   3. Run comparison:     ./benches/baseline.sh compare
#
# Criterion reports regressions/improvements inline and generates HTML reports.

set -euo pipefail

cd "$(dirname "$0")/.."

ACTION="${1:-compare}"
BASELINE_NAME="${2:-main}"

case "$ACTION" in
    save)
        echo "=== Saving baseline: $BASELINE_NAME ==="
        cargo bench -- --save-baseline "$BASELINE_NAME"
        echo ""
        echo "Baseline '$BASELINE_NAME' saved. Make your changes, then run:"
        echo "  ./benches/baseline.sh compare $BASELINE_NAME"
        ;;
    compare)
        echo "=== Comparing against baseline: $BASELINE_NAME ==="
        cargo bench -- --baseline "$BASELINE_NAME"
        echo ""
        echo "HTML reports: target/criterion/*/report/index.html"
        ;;
    *)
        echo "Usage: $0 {save|compare} [baseline-name]"
        exit 1
        ;;
esac
