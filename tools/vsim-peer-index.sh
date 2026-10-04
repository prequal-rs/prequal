#!/usr/bin/env bash
# Usage: tools/vsim-peer-index.sh [out-dir] — what routers sharing a fleet gain from knowing each other's placements,
# in virtual time: today's prequal against routers that see the engines' real caches (llm-bench --oracle-index) and
# routers that tell each other where they sent each prompt (--gossip-ms, --gossip-loss), per workload and router count.
# Overrides: ARMS (";"-separated llm-bench flags; "none" is today's prequal), WORKLOADS (shared, cache: the
# `two-routers` and `cache-two-routers` scenarios of vsim-compete.sh), ROUTERS, SEEDS, JOBS, LLM_BENCH_ARGS, LLM_BENCH.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$root/results/vsim-peer-index}"
bin="${LLM_BENCH:-$root/target/release/llm-bench}"
mkdir -p "$out"
rm -f "$out"/*.csv

workload_args() {
  case $1 in
    shared) echo "--engines 8 --engine-preset h100-qwen32b --warmup-stages 1 \
      --stages 15:50,3:20,10:20,15:20,20:38,22:34,25:30,30:25,35:21,40:38" ;;
    cache) echo "--engines 10 --engine-preset llmd-sim-cache --groups 300 --prompts-per-group 5 --system-len 10690 \
      --question-len 560 --popularity tiers:8=0.3,72=0.4,220=0.3 --warmup-stages 1 \
      --stages 6:120,4:120,6:120,8:120,10:120,12:120,14:120,16:120" ;;
    *) echo "unknown workload $1" >&2; exit 2 ;;
  esac
}

IFS=';' read -ra arms <<<"${ARMS:-none;--oracle-index events;--gossip-ms 100;--gossip-ms 1000;--gossip-ms 5000}"
jobs_file=$(mktemp)
trap 'rm -f "$jobs_file"' EXIT
for workload in ${WORKLOADS:-shared cache}; do
  for routers in ${ROUTERS:-1 2 3 4}; do
    for seed in ${SEEDS:-1 2 3 4 5}; do
      for i in "${!arms[@]}"; do
        flags=$([ "${arms[$i]}" = none ] && echo "" || echo "${arms[$i]}")
        echo "$bin --virtual --workload shared-prefix --slow-fraction 0 --policy prequal $(workload_args "$workload") \
          --routers $routers --seed $seed $flags ${LLM_BENCH_ARGS:-} --csv $out/$workload-${routers}r--$i--$seed.part" \
          >>"$jobs_file"
      done
    done
  done
done
xargs -P "${JOBS:-$(nproc)}" -I{} sh -c '{} >/dev/null' <"$jobs_file"
for workload in ${WORKLOADS:-shared cache}; do
  for routers in ${ROUTERS:-1 2 3 4}; do
    parts=("$out/$workload-${routers}r--"*.part)
    { head -1 "${parts[0]}"; tail -q -n +2 "${parts[@]}"; } >"$out/$workload-${routers}r.csv"
    rm -f "${parts[@]}"
  done
done
echo "vsim-peer-index: $(wc -l <"$jobs_file") runs -> $out" >&2
