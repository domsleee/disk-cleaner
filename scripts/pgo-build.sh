#!/usr/bin/env bash
# Profile-Guided Optimization (PGO) build pipeline.
#
# Produces an optimized release binary using real scan workloads as profile data.
# Requires: cargo-pgo (`cargo install cargo-pgo`), llvm-tools (`rustup component add llvm-tools`)
#
# Usage:
#   ./scripts/pgo-build.sh                  # PGO build, profile ~/git
#   ./scripts/pgo-build.sh /path/to/scan    # PGO build, profile specific dir
#   ./scripts/pgo-build.sh --bench-only     # PGO build using only criterion benchmarks
#
# The optimized binary is placed in target/release-dist/disk-cleaner

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

TRIPLE=$(rustc -vV | awk '/^host:/ { print $2 }')
PROFILE_DIR="target/pgo-profiles"
SCAN_TARGET="${1:-}"

# ── Preflight ──────────────────────────────────────────────────
command -v cargo-pgo >/dev/null 2>&1 || {
    echo "error: cargo-pgo not found. Install with: cargo install cargo-pgo"
    exit 1
}

rustup component list --installed | grep -q llvm-tools || {
    echo "error: llvm-tools not installed. Run: rustup component add llvm-tools"
    exit 1
}

# ── Step 1: Instrumented build ─────────────────────────────────
echo "==> Building instrumented binary..."
cargo pgo build -- --bin disk-cleaner --bin scan_only

# ── Step 2: Collect profiles ──────────────────────────────────
# Note: cargo pgo build already clears the profile dir and bakes the
# output path into the instrumented binary. Do NOT override LLVM_PROFILE_FILE.
SCAN_ONLY="target/${TRIPLE}/release/scan_only"

if [ "$SCAN_TARGET" = "--bench-only" ]; then
    echo "==> Collecting profiles from criterion benchmarks..."
    cargo pgo bench -- --bench scan_bench --bench tree_ops
else
    # Determine scan directory
    if [ -z "$SCAN_TARGET" ]; then
        if [ -d "$HOME/git" ]; then
            SCAN_TARGET="$HOME/git"
        else
            SCAN_TARGET="$HOME"
        fi
    fi

    if [ ! -d "$SCAN_TARGET" ]; then
        echo "error: scan target not a directory: $SCAN_TARGET"
        exit 1
    fi

    echo "==> Collecting profiles by scanning: $SCAN_TARGET"

    # Run scan_only 3 times to get stable profile data.
    # Do NOT use `cargo pgo bench` here — it clears the profile directory
    # on start and instrumented criterion benchmarks can segfault.
    for i in 1 2 3; do
        echo "    run $i/3..."
        "$SCAN_ONLY" "$SCAN_TARGET" >/dev/null
    done
fi

PROFRAW_COUNT=$(find "$PROFILE_DIR" -name '*.profraw' 2>/dev/null | wc -l | tr -d ' ')
echo "==> Collected $PROFRAW_COUNT profile files"

if [ "$PROFRAW_COUNT" -eq 0 ]; then
    echo "error: no profile data collected"
    exit 1
fi

# ── Step 3: Merge profiles ────────────────────────────────────
LLVM_PROFDATA=$(find "$(rustc --print sysroot)" -name llvm-profdata -type f 2>/dev/null | head -1)
if [ -z "$LLVM_PROFDATA" ]; then
    echo "error: llvm-profdata not found in rustc sysroot"
    exit 1
fi

MERGED="${PROFILE_DIR}/merged.profdata"
echo "==> Merging profile data..."
"$LLVM_PROFDATA" merge -o "$MERGED" "$PROFILE_DIR"/*.profraw

# ── Step 4: Optimized build with release-dist profile ─────────
# Build with release-dist (fat LTO + codegen-units=1) plus PGO profile data.
# We invoke cargo directly instead of `cargo pgo optimize` because cargo-pgo
# only supports the release profile, not custom profiles like release-dist.
echo "==> Building PGO-optimized binary (release-dist + PGO)..."
RUSTFLAGS="-Cprofile-use=${PWD}/${MERGED}" \
    cargo build --profile release-dist --bin disk-cleaner --bin scan_only

BINARY="target/release-dist/disk-cleaner"
SIZE=$(stat -f%z "$BINARY" 2>/dev/null || stat -c%s "$BINARY" 2>/dev/null || echo "unknown")

echo ""
echo "==> PGO build complete!"
echo "    Binary: $BINARY"
echo "    Size:   $(echo "$SIZE" | awk '{ printf "%.1f MB", $1/1048576 }')"
echo ""
echo "    Compare with: ./benches/ab.sh $BINARY"
