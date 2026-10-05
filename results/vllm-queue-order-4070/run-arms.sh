#!/usr/bin/env bash
# Usage: run-arms.sh <out-dir> — one tools/vsim-queue-order.sh run per (speed, seed, arm) against real vLLM in WSL,
# restarting vLLM before every run and alternating which arm goes first across seeds.
# Env: SPEEDS, SEEDS, ARMS, TRACES (trace dir), VLLM_ARGS (extra `vllm serve` flags), MODEL.
# Writes <out>/toolagent.<speed>.<arm>.<seed>.csv and one line per run to <out>/engine.tsv (see header there).
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
serve="$(wsl.exe -d Ubuntu -- wslpath -a "$(cygpath -m "$here")/serve.sh" | tr -d '\r\0')"
out="$1"
model="${MODEL:-Qwen/Qwen2.5-1.5B-Instruct}"
read -ra arms <<<"${ARMS:-fcfs history+kv@60}"
mkdir -p "$out"
[ -f "$out/engine.tsv" ] || printf 'speed\tseed\tarm\tkv_usage_max\tpreemptions\tprefix_hits\tprefix_queries\twall_s\n' >"$out/engine.tsv"

metric() { curl -s http://127.0.0.1:8000/metrics | awk -v m="$1" '$1 ~ "^"m"({|$)" {s += $2} END {print s + 0}'; }

for speed in ${SPEEDS:?}; do
  for seed in ${SEEDS:-1 2 3}; do
    order=("${arms[@]}")
    [ $((seed % 2)) = 0 ] && order=("${arms[1]}" "${arms[0]}")
    for arm in "${order[@]}"; do
      [ -f "$out/toolagent.$speed.${arm//[:@]/_}.$seed.csv" ] && continue
      # vLLM under WSL sometimes dies during startup (spurious CUDA OOM, or no output at all); a retry is a fresh server.
      for attempt in 1 2 3 4; do
        bash "$here/stop.sh"
        MSYS_NO_PATHCONV=1 wsl.exe -d Ubuntu -- bash "$serve" \
          --scheduling-policy priority ${VLLM_ARGS:-} &
        bash "$here/wait-ready.sh" 600 && break
        echo "start attempt $attempt failed (speed=$speed seed=$seed arm=$arm)"
        [ "$attempt" -lt 4 ] || exit 1
      done
      peak="$(mktemp)"
      (while :; do metric vllm:kv_cache_usage_perc; sleep 1; done | awk '$1 > m {m = $1; print m > f; close(f)}' f="$peak") &
      sampler=$!
      tmp="$(mktemp -d)"
      started=$SECONDS
      JOBS=1 LLM_BENCH="$root/target/release/llm-bench.exe" TRACES="${TRACES:-$root/target/mooncake-traces}" \
        ENGINE_ARGS="--direct --targets 127.0.0.1:8000 --model $model --token-vocab 30000 --max-context 32000" \
        ORDERS="$arm" TRACES_USED=toolagent SPEEDS="$speed" SEEDS="$seed" LLM_BENCH_ARGS="${LLM_BENCH_ARGS:-}" \
        bash "$root/tools/vsim-queue-order.sh" "$tmp" 2>&1 | grep -v '^trace: dropped'
      kill "$sampler" 2>/dev/null
      mv "$tmp/toolagent.$speed.csv" "$out/toolagent.$speed.${arm//[:@]/_}.$seed.csv"
      printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$speed" "$seed" "$arm" "$(cat "$peak")" \
        "$(metric vllm:num_preemptions_total)" "$(metric vllm:prefix_cache_hits_total)" \
        "$(metric vllm:prefix_cache_queries_total)" "$((SECONDS - started))" >>"$out/engine.tsv"
      rm -rf "$tmp" "$peak"
      echo "done speed=$speed seed=$seed arm=$arm ($((SECONDS - started))s)"
    done
  done
done
wsl.exe -d Ubuntu -- pkill -f 'vllm serve'
