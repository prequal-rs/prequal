#!/usr/bin/env bash
# Usage: tools/vsim-queue-order.sh [out-dir] — engine queue order (llm-bench --queue-order) against FCFS on the Mooncake
# traces (tools/fetch-mooncake-traces.sh) in virtual time, one routing policy, across trace speeds.
# Overrides: ORDERS ("fcfs" = no flag), TRACES_USED, SPEEDS, SEEDS, POLICY, JOBS, LLM_BENCH_ARGS, LLM_BENCH, TRACES.
# Real vLLM (started with --scheduling-policy priority) instead of virtual time, one run at a time:
#   JOBS=1 ENGINE_ARGS="--direct --targets 127.0.0.1:8000 --model <name> --token-vocab 30000 --max-context 32000"
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$root/results/vsim-queue-order}"
bin="${LLM_BENCH:-$root/target/release/llm-bench}"
mkdir -p "$out"
rm -f "$out"/*.csv

jobs_file=$(mktemp)
trap 'rm -f "$jobs_file"' EXIT
for trace in ${TRACES_USED:-toolagent conversation synthetic}; do
  for speed in ${SPEEDS:-2 4 6}; do
    for seed in ${SEEDS:-1 2 3}; do
      for order in ${ORDERS:-fcfs oracle noisy:1 history history@20}; do
        flag=$([ "$order" = fcfs ] && echo "" || echo "--queue-order $order")
        # `--` delimits fields so speed 4's glob can't match speed 4.5's parts.
        part="$out/$trace--$speed--${order//[:@]/_}--$seed.part"
        echo "$bin ${ENGINE_ARGS:---virtual --routers 1 --slow-fraction 0 --engines 16 --engine-preset h100-qwen32b} \
          --trace ${TRACES:-$root/target/mooncake-traces}/$trace.jsonl --trace-speed $speed --warmup-stages 1 \
          --policy ${POLICY:-prequal} --seed $seed $flag ${LLM_BENCH_ARGS:-} --csv $part" >>"$jobs_file"
      done
    done
  done
done
xargs -P "${JOBS:-$(nproc)}" -I{} sh -c '{} >/dev/null' <"$jobs_file"
for trace in ${TRACES_USED:-toolagent conversation synthetic}; do
  for speed in ${SPEEDS:-2 4 6}; do
    parts=("$out/$trace--$speed--"*.part)
    { head -1 "${parts[0]}"; tail -q -n +2 "${parts[@]}" | sed "s/^/$trace,$speed,/"; } |
      sed "1s/^/trace,speed,/" >"$out/$trace.$speed.csv"
    rm -f "${parts[@]}"
  done
done
echo "vsim-queue-order: $(wc -l <"$jobs_file") runs -> $out" >&2
