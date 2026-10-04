#!/usr/bin/env bash
# Usage: tools/vsim-compete.sh [out-dir] — tools/llm-compete.sh's scenarios in virtual time (llm-bench --virtual), all
# runs in parallel, seconds each. For iterating on policies; confirm finalists with tools/llm-compete.sh.
# Overrides: POLICIES, SEEDS, SCENARIOS, JOBS (parallel runs, default: CPU count), LLM_BENCH_ARGS (extra flags for
# every run, e.g. "--prefill-signal scrape"), LLM_BENCH (binary).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$root/results/vsim}"
bin="${LLM_BENCH:-$root/target/release/llm-bench}"
mkdir -p "$out"
rm -f "$out"/*.csv

policies="${POLICIES:-prequal llmd-default llmd-guide sglang-cache-aware dynamo round-robin}"
# Same ladder as llm-compete.sh, capped at 40 QPS: above it every policy fails in this fleet.
ladder="15:50,3:20,10:20,15:20,20:38,22:34,25:30,30:25,35:21,40:38"
common="--virtual --workload shared-prefix ${LLM_BENCH_ARGS:-}"
h100="--engines 8 --engine-preset h100-qwen32b"
ladder_args="$h100 --stages $ladder --warmup-stages 1"

scenario_args() {
  case $1 in
    canonical) echo "--routers 1 --slow-fraction 0 $ladder_args" ;;
    zipf) echo "--routers 1 --slow-fraction 0 --popularity zipf:1.1 $ladder_args" ;;
    unique) echo "--routers 1 --slow-fraction 0 --unique-questions $ladder_args" ;;
    burst) echo "--routers 1 --slow-fraction 0 $h100 --closed-loop 400 --duration-s 120 --warmup-s 30" ;;
    mixed) echo "--routers 1 --slow-fraction 0.5 $ladder_args" ;;
    two-routers) echo "--routers 2 --slow-fraction 0 $ladder_args" ;;
    # tools/kind-llmd.sh's `loaded` workload; opt-in via SCENARIOS.
    loaded) echo "--routers 1 --slow-fraction 0 --engines 10 --engine-preset llmd-sim-loaded --system-len 9500 \
      --question-len 500 --stages 5:60,15:180,25:180" ;;
    # tools/kind-cache.sh's `cache` workload (bounded KV, hot/warm/cold tiers, ~45 KB prompts); opt-in via SCENARIOS.
    cache | cache-two-routers) echo "--routers $([ "$1" = cache ] && echo 1 || echo 2) --slow-fraction 0 --engines 10 \
      --engine-preset llmd-sim-cache --groups 300 --prompts-per-group 5 --system-len 10690 --question-len 560 \
      --popularity tiers:8=0.3,72=0.4,220=0.3 --stages 6:120,4:120,6:120,8:120,10:120,12:120,14:120,16:120 \
      --warmup-stages 1" ;;
    # Mooncake production traces (tools/fetch-mooncake-traces.sh), replayed at TRACE_SPEED; opt-in via SCENARIOS.
    trace-conversation | trace-toolagent | trace-synthetic) echo "--routers 1 --slow-fraction 0 --engines 16 \
      --engine-preset h100-qwen32b --trace ${TRACES:-$root/target/mooncake-traces}/${1#trace-}.jsonl \
      --trace-speed ${TRACE_SPEED:-2} --warmup-stages 1" ;;
    *) echo "unknown scenario $1" >&2; exit 2 ;;
  esac
}

jobs_file=$(mktemp)
trap 'rm -f "$jobs_file"' EXIT
for scenario in ${SCENARIOS:-canonical zipf unique burst mixed two-routers}; do
  for seed in ${SEEDS:-1 2 3}; do
    for policy in $policies; do
      # One CSV per run so parallel appends never interleave; merged below. `:` (policy parameters) isn't a valid
      # Windows file-name character.
      part="$out/$scenario.${policy//:/_}.$seed.part"
      echo "$bin $common $(scenario_args "$scenario") --policy $policy --seed $seed --csv $part" >>"$jobs_file"
    done
  done
done
xargs -P "${JOBS:-$(nproc)}" -I{} sh -c '{} >/dev/null' <"$jobs_file"
for scenario in ${SCENARIOS:-canonical zipf unique burst mixed two-routers}; do
  parts=("$out/$scenario".*.part)
  { head -1 "${parts[0]}"; tail -q -n +2 "${parts[@]}"; } >"$out/$scenario.csv"
  rm -f "${parts[@]}"
done
echo "vsim-compete: $(wc -l <"$jobs_file") runs -> $out" >&2
