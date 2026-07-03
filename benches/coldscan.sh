#!/usr/bin/env bash
# Cold-cache scan benchmark (macOS, no sudo required).
#
# Scans a fixture on an APFS sparse image, detaching/reattaching the
# volume between runs — detach evicts the volume's vnode/metadata cache,
# so every run is a true cold scan. Avoids needing `sudo purge`.
#
# Usage:
#   ./benches/coldscan.sh                # 5 cold runs
#   RUNS=10 ./benches/coldscan.sh        # custom run count
#   SCAN_THREADS=16 ./benches/coldscan.sh  # thread-count experiments
#   ./benches/coldscan.sh --clean        # delete cached image and exit
#
# Fixture: 31,200 data files (+ 1 completion marker) / 450 dirs, created
# once and reused across invocations (image kept at $IMAGE).

set -euo pipefail

IMAGE="/tmp/disk-cleaner-coldbench.sparseimage"
MOUNT="/tmp/disk-cleaner-coldbench-mnt"
MARKER="$MOUNT/.fixture-complete"
RUNS="${RUNS:-5}"

cd "$(dirname "$0")/.."

attach() { hdiutil attach -quiet -mountpoint "$MOUNT" "$IMAGE"; }
detach() { hdiutil detach -quiet "$MOUNT" 2>/dev/null || true; }
trap detach EXIT

if [[ "${1:-}" == "--clean" ]]; then
  detach
  rm -f "$IMAGE"
  echo "Removed $IMAGE"
  exit 0
fi

# Reject 0 and leading-zero/octal forms (checked after --clean, which
# ignores RUNS).
if ! [[ "$RUNS" =~ ^[1-9][0-9]*$ ]]; then
  echo "RUNS must be a positive integer, got: '$RUNS'" >&2
  exit 1
fi

# --- Fixture (created once, reused; marker guards partial creation) ---
if [[ -f "$IMAGE" ]]; then
  attach
  if [[ ! -f "$MARKER" ]]; then
    echo "Incomplete fixture image, recreating..."
    detach
    rm -f "$IMAGE"
  fi
fi
if [[ ! -f "$IMAGE" ]]; then
  echo "=== Creating fixture image (one-time) ==="
  hdiutil create -size 2g -fs APFS -volname coldbench -type SPARSE -quiet "$IMAGE"
  attach
  for i in $(seq 1 50); do
    for j in $(seq 1 8); do
      d="$MOUNT/top_$i/mid_$j"
      mkdir -p "$d"
      (cd "$d" && touch file_{001..075}.dat \
        && for k in 1 2 3; do head -c 8192 /dev/zero > "blob_$k.bin"; done)
    done
  done
  touch "$MARKER"
  echo "Fixture: $(find "$MOUNT" -type f | wc -l | tr -d ' ') files"
fi
detach

echo "=== Building stat_bench ==="
cargo bench --bench stat_bench --no-run 2>&1 | tail -1

echo ""
echo "=== Cold scan: $RUNS runs (detach/attach between each) ==="
times=()
for run in $(seq 1 "$RUNS"); do
  attach
  sleep 1
  ms=$(BENCH_DIR="$MOUNT" BENCH_RUNS=1 BENCH_WARMUP=0 \
       cargo bench --bench stat_bench 2>&1 \
       | awk '/Run  1:/ {print $3}')
  detach
  if ! [[ "$ms" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
    echo "  run $run: failed to parse scan time (got: '$ms')" >&2
    exit 1
  fi
  echo "  run $run: ${ms} ms"
  times+=("$ms")
done

echo ""
printf '%s\n' "${times[@]}" | awk '
  { sum += $1; sumsq += $1 * $1; n++ }
  END {
    mean = sum / n
    var = (n > 1) ? (sumsq - sum * sum / n) / (n - 1) : 0
    if (var < 0) var = 0   # guard fp cancellation on identical samples
    printf "Cold scan: %.1f ± %.1f ms (%d runs)\n", mean, sqrt(var), n
  }'
