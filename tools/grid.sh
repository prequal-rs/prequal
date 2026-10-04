#!/usr/bin/env bash
# Usage: tools/grid.sh [out.csv] [extra testbed args...]  — policies x LOADS x SEEDS, sequentially, on the HTTP
# testbed (prequal-testbed binary). Summarise with tools/summarize.mjs.
# RUN_PREFIX wraps each run (e.g. "tools/quiet-run taskset -c 0,1").
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$root/target/grid.csv}"
shift || true
bin="$root/target/release/prequal-testbed"
mkdir -p "$(dirname "$out")"

runs=(
  "--policy p2c-lr"
  "--policy p2c-ewma"
  "--policy prequal --probes-per-query 0 --piggyback"
  "--policy prequal --probes-per-query 0.5 --piggyback"
  "--policy prequal --probes-per-query 0.5 --piggyback --estimator model"
)
for seed in ${SEEDS:-1 2}; do
  for load in ${LOADS:-0.7 0.9 1.05}; do
    for run in "${runs[@]}"; do
      # shellcheck disable=SC2086
      ${RUN_PREFIX:-} "$bin" $run --load "$load" --seed "$seed" --csv "$out" "$@" | tail -1
    done
  done
done
