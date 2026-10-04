# Runs in a curl pod (args: sim pod IPs): fleet-wide prefix-cache counters every 5 s as `<epoch> <hits> <queries>`.
# llm-d-inference-sim names them without the `_total` suffix; accept both.
while :; do
  t=$(date +%s)
  for ip in "$@"; do curl -s -m 2 "$ip:8000/metrics"; done |
    awk -v t="$t" '/^vllm:prefix_cache_hits(_total)?[{ ]/ {h += $NF} /^vllm:prefix_cache_queries(_total)?[{ ]/ {q += $NF}
      END {print t, h + 0, q + 0}'
  sleep 5
done
