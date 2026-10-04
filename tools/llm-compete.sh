#!/usr/bin/env bash
# Usage: tools/llm-compete.sh [out-dir] — prefix-aware routing policies head to head on simulated H100 engines.
# Scenarios: llm-d's published shared-prefix ladder, Zipf popularity, unique questions, closed-loop bursts, mixed
# fleet. One CSV per scenario. Holds ~/bench.lock for the whole run. CPU pinning assumes a dedicated 6-core/12-thread
# Linux host with CPUs 5 and 11 left free; adjust the taskset lists below for other machines.
# Overrides: POLICIES, SEEDS, SCENARIOS, TIME_SCALE (wall seconds per simulated second), ROUTER_ARGS.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$root/results/llm-compete}"
rel="$root/target/release"
mkdir -p "$out"

exec 8>"$HOME/bench.lock"
echo "llm-compete: waiting for ~/bench.lock" >&2
flock 8
echo "llm-compete: lock held from $(date -u +%T)" >&2
export BENCH_LOCK="$HOME/.bench-inner-$$.lock" QUIET_IGNORE_CPUS="5 11" PREQUAL_PROXY_CPUS=4,10
trap 'rm -f "$BENCH_LOCK"' EXIT

policies="${POLICIES:-prequal llmd-guide llmd-default sglang-cache-aware dynamo round-robin}"
# llm-d-benchmark guide_optimized-baseline_1: 15 QPS warmup, then 3 -> 60 QPS.
ladder="15:50,3:20,10:20,15:20,20:38,22:34,25:30,30:25,35:21,40:38,43:36,46:33,49:30,52:29,55:27,57:26,60:25"
# One router instance, as llm-d's guides deploy it; the `two-routers` scenario tests replicated routers.
common=(--router-bin "$rel/prequal-router" --engines 8 --engine-preset h100-qwen32b
  --workload shared-prefix --time-scale "${TIME_SCALE:-0.25}")
ladder_args=(--stages "$ladder" --warmup-stages 1)
# ROUTER_ARGS: extra flags for every router instance, e.g. "--admission-limit 2".
for arg in ${ROUTER_ARGS:-}; do common+=(--router-arg "$arg"); done

run() {
  local scenario=$1
  shift
  echo "== $scenario ($(date -u +%T))" >&2
  for seed in ${SEEDS:-1 2}; do
    for policy in $policies; do
      "$root/tools/quiet-run" taskset -c 0-3,6-9 "$rel/llm-bench" "${common[@]}" --policy "$policy" --seed "$seed" \
        --csv "$out/$scenario.csv" "$@" | tail -1
    done
  done
}

for scenario in ${SCENARIOS:-canonical zipf unique burst mixed two-routers}; do
  case $scenario in
    canonical) run canonical --routers 1 --slow-fraction 0 "${ladder_args[@]}" ;;
    zipf) run zipf --routers 1 --slow-fraction 0 --popularity zipf:1.1 "${ladder_args[@]}" ;;
    unique) run unique --routers 1 --slow-fraction 0 --unique-questions "${ladder_args[@]}" ;;
    burst) run burst --routers 1 --slow-fraction 0 --closed-loop 400 --duration-s 120 --warmup-s 30 ;;
    mixed) run mixed --routers 1 --slow-fraction 0.5 "${ladder_args[@]}" ;;
    two-routers) run two-routers --routers 2 --slow-fraction 0 "${ladder_args[@]}" ;;
    *) echo "unknown scenario $scenario" >&2; exit 2 ;;
  esac
done
echo "== done ($(date -u +%T))" >&2
