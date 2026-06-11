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
# Fixture: 31,200 files / 450 dirs, created once and reused across
# invocations (image kept at $IMAGE).

set -euo pipefail

IMAGE="/tmp/disk-cleaner-coldbench.sparseimage"
VOLUME="/Volumes/coldbench"
RUNS="${RUNS:-5}"

cd "$(dirname "$0")/.."

if [[ "${1:-}" == "--clean" ]]; then
  hdiutil detach -quiet "$VOLUME" 2>/dev/null || true
  rm -f "$IMAGE"
  echo "Removed $IMAGE"
  exit 0
fi

detach() { hdiutil detach -quiet "$VOLUME" 2>/dev/null || true; }
trap detach EXIT

# --- Fixture (created once, reused) ---
if [[ ! -f "$IMAGE" ]]; then
  echo "=== Creating fixture image (one-time) ==="
  hdiutil create -size 2g -fs APFS -volname coldbench -type SPARSE -quiet "$IMAGE"
  hdiutil attach -quiet "$IMAGE"
  for i in $(seq 1 50); do
    for j in $(seq 1 8); do
      d="$VOLUME/top_$i/mid_$j"
      mkdir -p "$d"
      (cd "$d" && touch file_{001..075}.dat \
        && for k in 1 2 3; do head -c 8192 /dev/zero > "blob_$k.bin"; done)
    done
  done
  echo "Fixture: $(find "$VOLUME" -type f | wc -l | tr -d ' ') files"
  detach
fi

echo "=== Building stat_bench ==="
cargo bench --bench stat_bench --no-run 2>&1 | tail -1

echo ""
echo "=== Cold scan: $RUNS runs (detach/attach between each) ==="
times=()
for run in $(seq 1 "$RUNS"); do
  hdiutil attach -quiet "$IMAGE"
  sleep 1
  ms=$(BENCH_DIR="$VOLUME" BENCH_RUNS=1 BENCH_WARMUP=0 \
       cargo bench --bench stat_bench 2>&1 \
       | awk '/Run  1:/ {print $3}')
  echo "  run $run: ${ms} ms"
  times+=("$ms")
  detach
done

echo ""
printf '%s\n' "${times[@]}" | awk '
  { sum += $1; sumsq += $1 * $1; n++ }
  END {
    mean = sum / n
    sd = (n > 1) ? sqrt((sumsq - sum * sum / n) / (n - 1)) : 0
    printf "Cold scan: %.1f ± %.1f ms (%d runs)\n", mean, sd, n
  }'
