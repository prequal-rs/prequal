#!/usr/bin/env bash
# Cache-pressure benchmark on tools/kind-llmd.sh's harness: sim KV caches are bounded (KV_BLOCKS blocks of 16 tokens
# per pod) so they evict, the prefix working set is ~2-3x the fleet's KV capacity with skewed popularity, and the load
# sweeps up past the fleet's capacity. Arms: prequal, prequal-gossip (pickers share placements, for cache-two-epp),
# llmd (optimized baseline), llmd-precise (KV-event routing).
# Usage: tools/kind-cache.sh setup | run <arm> <label> [cache|cache-two-epp] | [ARMS="a b"] suite [workload] [rounds] |
#        epp-check | host-cpu | teardown     (per-stage analysis: node tools/cache-report.mjs <reports dir> [label])
# Env: KV_BLOCKS (6000), MAX_SEQS (8), STAGES ("rate:seconds ..." override), BENCH (~/bench-cache), CPUS (0-4,6-10:
# the kind node's cpuset), plus kind-llmd.sh's LLMD_VERSION, MAX_LOAD, LOAD_WAIT, TAG.
set -euo pipefail
export BENCH=${BENCH:-$HOME/bench-cache}
# shellcheck source=tools/kind-llmd.sh
source "$(dirname "$0")/kind-llmd.sh"

# Every arm's sims publish KV events to router-epp:5557; only llmd-precise listens (others drop them unsent), so the
# sims cost the same in every arm. The later --kv-cache-size overrides sim.yaml's unbounded one (pflag: last wins).
sim_args() {
  echo "--max-num-seqs, \"${MAX_SEQS:-8}\", --time-factor-under-load, \"4\", --kv-cache-size, \"${KV_BLOCKS:-6000}\",
    --zmq-endpoint, \"tcp://router-epp.bench.svc:5557\"" | tr -s ' \n' ' '
}

# Stage 0 is warm-up; the sweep's top stages overload the fleet (10 pods x MAX_SEQS slots).
stages() { echo "${STAGES:-6:120 4:120 6:120 8:120 10:120 12:120 14:120 16:120}"; }

# Tiered popularity (inference-perf's shared_prefix replays its groups uniformly): 8 hot groups get 30% of the rate,
# 72 warm 40%, 220 cold 30%. 300 groups x ~442 prefix blocks is ~2.2x the default fleet capacity (10 x 6000 blocks).
job_specs() {
  printf '%s\n' "inference-perf-hot -hot 1 0.3 8" "inference-perf - 2 0.4 72" "inference-perf-cold -cold 1 0.3 220"
}

# Distinct seeds per job, or jobs would generate the same prefix texts.
data_config() {
  echo "{type: shared_prefix, shared_prefix: {num_groups: $1, num_prompts_per_group: 5, system_prompt_len: 9500,
    question_len: 500, output_len: 1000, enable_multi_turn_chat: false, seed: $1}}" | tr -s ' \n' ' '
}

epp_replicas() { if [ "$1" = cache-two-epp ]; then echo 2; else echo 1; fi; }

job_command() { echo "python, /reports/.ip-stage-barrier.py, \"$1\", \"$(job_specs | wc -l)\""; }

eval "$(declare -f start_jobs | sed '1s/^start_jobs/llmd_start_jobs/')"
start_jobs() {
  rm -f "$reports/.barrier-$1-"*
  cp "$cfg/ip-stage-barrier.py" "$reports/.ip-stage-barrier.py"
  llmd_start_jobs "$@"
}

# The precise EPP keys endpoints as `<ip>:<port>` and takes a publisher's id from its topic `kv@<POD_IP>@<model>`;
# the sim uses POD_IP for nothing else.
deploy_sims() {
  sed -e "s|SIM_EXTRA_ARGS|$(sim_args)|" \
    -e 's|- {name: POD_IP, valueFrom: {fieldRef: {fieldPath: status.podIP}}}|- {name: POD_IP_RAW, valueFrom: {fieldRef: {fieldPath: status.podIP}}}\n        - {name: POD_IP, value: "$(POD_IP_RAW):8000"}|' \
    "$cfg/sim.yaml" | kubectl apply -f -
  k rollout status deploy/llm-d-sim --timeout=5m
}

stop_samplers() {
  local label=$1 job suffix rest
  kill "$sampler" || true
  k logs pod/sim-timeline >"$reports/$label-hits.log" || true
  k delete pod sim-timeline --wait=false
  while read -r job suffix rest; do
    [ "$suffix" = - ] && suffix=
    k logs --timestamps "job/$job" | grep -E 'Stage [0-9]+ - run (started|completed|failed)' \
      >"$reports/$label$suffix-stages.log" || true
  done < <(job_specs)
}

epp_ips() { k get pods -o name | grep router-epp | xargs -r -n1 kubectl -n bench get -o jsonpath='{.status.podIP} '; }

# Sim counters plus, while the router is still up, each EPP's KV-event/index metrics and its ZMQ log lines: the
# evidence that llmd-precise consumes the sims' events.
scrape_sims() {
  local label=$1 pod
  k run "curl-$RANDOM" --rm -i --restart=Never --image=curlimages/curl:latest --image-pull-policy=IfNotPresent -- sh -c \
    "for ip in $(sim_ips); do curl -s \$ip:8000/metrics |
      grep -E '^vllm:(prefix_cache_(hits|queries)(_total)?|request_success_total|num_requests_waiting)[{ ]' |
      sed \"s/^/\$ip /\"; done; for ip in $(epp_ips); do curl -s \$ip:9090/metrics |
      grep -E '^(llm_d_epp_)?(kv_?cache_(events|index)|request_size_bytes_(sum|count))' |
      sed \"s/^/epp-\$ip /\"; done" >"$reports/$label-sims.txt"
  for pod in $(k get pods -o name | grep router-epp); do
    k logs "$pod" -c epp | grep -iE 'subscriber|zmq|kv.?events' | head -20
  done >"$reports/$label-epp.log" || true
}

# Smoke test of llmd-precise: a few requests, then the EPP's event/index counters (lookup hits must be non-zero).
epp_check() {
  deploy_sims
  ! helm status router -n bench >/dev/null 2>&1 || helm uninstall router -n bench --wait
  helm install router "$chart" --version "$router_version" -n bench -f "$baseline" -f "$cfg/common.yaml" \
    -f "$cfg/arm-llmd-precise.yaml" --set "router.epp.image.tag=$router_version"
  k rollout status deploy/router-epp --timeout=5m
  k run "curl-$RANDOM" --rm -i --restart=Never --image=curlimages/curl:latest --image-pull-policy=IfNotPresent -- sh -c \
    "p=\$(seq 1 3000 | tr '\n' ' '); for i in 1 2 3 4 5 6; do curl -s -o /dev/null -w '%{http_code} ' \
      router-epp:80/v1/completions -H 'content-type: application/json' \
      -d \"{\\\"model\\\":\\\"Qwen/Qwen3-8B\\\",\\\"prompt\\\":\\\"\$p q\$i\\\",\\\"max_tokens\\\":5}\"; sleep 2; done"
  sleep 3
  scrape_sims epp-check
  cat "$reports/epp-check-sims.txt" "$reports/epp-check-epp.log"
  helm uninstall router -n bench --wait
}

# Every 5 s `<epoch> <host busy CPU-seconds> <kind node CPU-seconds>`: their difference is other tenants' CPU, which
# the lock doesn't keep out. Run alongside the suite: tools/kind-cache.sh host-cpu >>"$BENCH/reports/host-cpu.log".
host_cpu() {
  local cg
  cg=/sys/fs/cgroup/system.slice/docker-$(docker inspect -f '{{.Id}}' bench-control-plane).scope/cpu.stat
  while :; do
    echo "$(date +%s) $(awk '/^cpu / {printf "%.2f", ($2 + $3 + $4 + $7 + $8 + $9) / 100}' /proc/stat)" \
      "$(awk '/^usage_usec/ {printf "%.2f", $2 / 1e6}' "$cg")"
    sleep 5
  done
}

case ${1:-} in
  setup) setup; docker update --cpuset-cpus "${CPUS:-0-4,6-10}" bench-control-plane >/dev/null ;;
  host-cpu) host_cpu ;;
  run) run "${@:2}" ;;
  suite) suite "${@:2}" ;;
  epp-check) epp_check ;;
  teardown) teardown ;;
  *) sed -n '2,9p' "$0"; exit 2 ;;
esac
