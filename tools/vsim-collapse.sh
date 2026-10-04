#!/usr/bin/env bash
# Usage: tools/vsim-collapse.sh [out-dir] — collapse frequency on the kind `loaded` workload in virtual time: every
# policy × host-slowdown factor × seed, the slowdown covering the 25 QPS stage (llm-bench --host-slowdown). A run
# collapses when that stage's TTFT p90 exceeds 1 s. Overrides: POLICIES, SEEDS, FACTORS, JOBS, LLM_BENCH (binary).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$root/results/vsim-collapse}"
bin="${LLM_BENCH:-$root/target/release/llm-bench}"
mkdir -p "$out"
rm -f "$out"/*.part
loaded="--virtual --workload shared-prefix --routers 1 --slow-fraction 0 --engines 10 --engine-preset llmd-sim-loaded \
  --system-len 9500 --question-len 500 --stages 5:60,15:180,25:180"

jobs_file=$(mktemp)
trap 'rm -f "$jobs_file"' EXIT
for factor in ${FACTORS:-1 2.5 3}; do
  for seed in ${SEEDS:-$(seq 1 10)}; do
    for policy in ${POLICIES:-prequal llmd-default round-robin}; do
      part="$out/${policy//:/_}@$factor@$seed.part"
      echo "$bin $loaded --host-slowdown $factor:240:180 --policy $policy --seed $seed --csv $part" >>"$jobs_file"
    done
  done
done
xargs -P "${JOBS:-$(nproc)}" -I{} sh -c '{} >/dev/null' <"$jobs_file"
# CSV columns: 1 policy, 8 ttft_p50_ms, 9 ttft_p99_ms, 14 stage, 17 ttft_p90_ms.
for part in "$out"/*.part; do
  IFS=@ read -r _ factor _ <<<"$(basename "$part" .part)"
  awk -F, -v factor="$factor" 'NR > 1 && $14 == 2 {print $1, factor, $8, $17, $9}' "$part"
done | sort -k1,1 -k2,2n | awk '
  function flush() { if (n) printf "%-28s x%-4s collapsed %2d/%-2d  median p50/p90/p99 %s/%s/%s ms\n", key, f, c, n,
    med(p50), med(p90), med(p99) }
  function med(a,   k, i, j, t) { k = n; for (i = 1; i <= k; i++) for (j = i + 1; j <= k; j++) if (a[j] < a[i]) { t = a[i]; a[i] = a[j]; a[j] = t }
    return a[int((k + 1) / 2)] }
  { if ($1 != key || $2 != f) { flush(); key = $1; f = $2; n = c = 0 }
    n++; p50[n] = $3; p90[n] = $4; p99[n] = $5; c += ($4 > 1000) }
  END { flush() }'
