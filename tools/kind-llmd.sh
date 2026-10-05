#!/usr/bin/env bash
# prequal-epp vs llm-d's EPP in kind on llm-d's simulator and benchmark; see deploy/bench/llmd/README.md.
# Usage: tools/kind-llmd.sh setup | run <llmd|prequal|prequal-lean|router> <label> [workload] |
#        [ARMS="a b"] suite [workload] [rounds] | results [label-glob] | teardown | router-image <prequal-router binary>
# Arm `router` replaces the chart's Envoy + EPP pod with prequal-router alone (deploy/bench/llmd/arm-router.yaml).
# Arm `prequal-lean` is lean mode: Envoy's ext_proc skips response bodies, prequal-epp --prefill-signal scrape.
# Workloads: job1 (default), loaded, burst, zipf, two-epp. LLMD_VERSION picks the chart and llm-d EPP release
# (default v0.11.0; v0.10.0 reproduces the earlier results). Needs docker, kind, kubectl, helm, jq. Timed runs hold
# ~/bench.lock (shared-box turn-taking) and start once the 1-min load is below MAX_LOAD (8; wait at most LOAD_WAIT s).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cfg="$root/deploy/bench/llmd"
bench="${BENCH:-$HOME/bench}"
reports="$bench/reports"
chart=oci://ghcr.io/llm-d/charts/llm-d-router-standalone
router_version=${LLMD_VERSION:-v0.11.0}
# llm-d's nightly router values for this release; v0.11 raised Envoy's --concurrency from 8 to 32.
baseline="$bench/optimized-baseline-$router_version.yaml"
images=(ghcr.io/llm-d/llm-d-inference-sim:v0.10.2 "ghcr.io/llm-d/llm-d-router-endpoint-picker:$router_version"
  docker.io/envoyproxy/envoy:distroless-v1.33.2 quay.io/inference-perf/inference-perf:v0.6.0 curlimages/curl:latest)
lean_envoy='s/response_body_mode: FULL_DUPLEX_STREAMED/response_body_mode: NONE/
s/response_trailer_mode: SEND/response_trailer_mode: SKIP/'

k() { kubectl -n bench "$@"; }

sim_args() {
  case $1 in
    job1) echo '--max-num-seqs, "256"' ;;
    loaded | burst | zipf | two-epp) echo '--max-num-seqs, "32", --time-factor-under-load, "4"' ;;
    *) echo "unknown workload $1" >&2; exit 2 ;;
  esac
}

# `rate:seconds` per stage. inference-perf drains between stages, so each burst spike starts on an idle, warm fleet.
stages() {
  case $1 in
    job1) echo 1:15 10:300 ;;
    burst) echo 5:45 35:20 5:45 35:20 ;;
    *) echo 5:60 15:180 25:180 ;;
  esac
}

stage_yaml() {
  local scale=$1
  shift
  printf '%s\n' "$@" | awk -F: -v s="$scale" '{printf "%s{rate: %g, duration: %s}", (NR > 1 ? ", " : "["), $1 * s, $2}
    END {print "]"}'
}

# Load-generator jobs as `<job> <report-label suffix> <cpu request> <rate share> <prefix groups>`. inference-perf's
# shared_prefix replays groups uniformly and its shareGPT file replay can't sustain these rates with 10k-token prompts
# (single-process tokenisation), so `zipf` approximates zipf(1.1) over 150 groups (top group 23%) with two concurrent
# generators: one hot group at 25% of the rate and 149 cold groups at 75%.
job_specs() {
  case $1 in
    zipf) printf '%s\n' "inference-perf - 2 0.75 149" "inference-perf-hot -hot 2 0.25 1" ;;
    *) echo "inference-perf - 4 1 150" ;;
  esac
}

data_config() {
  echo "{type: shared_prefix, shared_prefix: {num_groups: $1, num_prompts_per_group: 5, system_prompt_len: 9500,
    question_len: 500, output_len: 1000, enable_multi_turn_chat: false}}" | tr -s ' \n' ' '
}

# Chart replicas > 1 means active/passive leader election (one serving EPP), so two active routers are made by
# scaling the Deployment after install, identically for both arms.
epp_replicas() { if [ "$1" = two-epp ]; then echo 2; else echo 1; fi; }

job_command() { echo inference-perf; }

start_jobs() {
  local label=$1 workload=$2 job suffix cpu share groups
  while read -r job suffix cpu share groups; do
    [ "$suffix" = - ] && suffix=
    sed -e "s|JOB_NAME|$job|" -e "s|IP_CPU|$cpu|" -e "s|RUN_LABEL|$label$suffix|" \
      -e "s|command: \[inference-perf\]|command: [$(job_command "$label")]|" \
      -e "s|LOAD_STAGES|$(stage_yaml "$share" $(stages "$workload"))|" -e "s|DATA_CONFIG|$(data_config "$groups")|" \
      "$cfg/job.yaml" | kubectl apply -f -
  done < <(job_specs "$workload")
}

deploy_sims() {
  sed "s|SIM_EXTRA_ARGS|$(sim_args "$1")|" "$cfg/sim.yaml" | kubectl apply -f -
  k rollout status deploy/llm-d-sim --timeout=5m
}

setup() {
  mkdir -p "$reports"
  kind get clusters | grep -qx bench || sed "s|REPORTS_DIR|$reports|" "$cfg/kind.yaml" | kind create cluster --config -
  kubectl apply -f https://github.com/kubernetes-sigs/gateway-api-inference-extension/releases/download/v1.6.2/manifests.yaml
  kubectl apply -f https://github.com/kubernetes-sigs/metrics-server/releases/latest/download/components.yaml
  kubectl -n kube-system patch deploy metrics-server --type=json \
    -p='[{"op":"add","path":"/spec/template/spec/containers/0/args/-","value":"--kubelet-insecure-tls"}]' || true
  kubectl get ns bench >/dev/null 2>&1 || kubectl create ns bench
  k get secret hf-secret >/dev/null 2>&1 || k create secret generic hf-secret --from-literal=token="${HF_TOKEN:-none}"
  # Public images are pulled by the node itself: `kind load` fails on multi-platform registry images.
  for image in "${images[@]}"; do docker exec bench-control-plane crictl pull "$image" >/dev/null; done
  docker build -q -t local.dev/prequal-epp:dev -f "$root/deploy/epp.Dockerfile" "$root"
  kind load docker-image local.dev/prequal-epp:dev --name bench
  curl -fsSLo "$baseline" \
    "https://raw.githubusercontent.com/llm-d/llm-d-router/$router_version/test/perf/config/router-configs/optimized-baseline.yaml"
  kubectl -n kube-system rollout status deploy/metrics-server --timeout=5m
  deploy_sims job1
}

# Ready sims only: right after a rollout restart, terminating pods still list as Running.
sim_ips() { k get endpoints llm-d-sim-svc -o jsonpath='{.subsets[*].addresses[*].ip}'; }

# Per-pod prefix-cache counters: the routing-quality signal.
scrape_sims() {
  k run "curl-$RANDOM" --rm -i --restart=Never --image=curlimages/curl:latest --image-pull-policy=IfNotPresent -- sh -c \
    "for ip in $(sim_ips); do curl -s \$ip:8000/metrics |
      grep -E '^vllm:(prefix_cache_(hits|queries)(_total)?|request_success_total)[{ ]' | sed \"s/^/\$ip /\"; done" \
    >"$reports/$1-sims.txt"
}

start_samplers() {
  local label=$1
  kubectl -n kube-system rollout status deploy/metrics-server --timeout=5m >/dev/null
  # set +e: `kubectl top` fails until metrics-server has scraped the new pods, which under the inherited errexit used
  # to end the sampler on its first iteration (empty -top.log).
  (set +e; while sleep 10; do
    echo "load $(cut -d' ' -f1-3 /proc/loadavg) $(date +%s)"
    kubectl top pod -n bench --containers --no-headers 2>/dev/null | grep --line-buffered -E 'router-epp|inference-perf'
  done) >"$reports/$label-top.log" &
  sampler=$!
  # shellcheck disable=SC2046
  k run sim-timeline --restart=Never --image=curlimages/curl:latest --image-pull-policy=IfNotPresent --command -- \
    sh -c "$(cat "$cfg/sim-timeline.sh")" sim-timeline $(sim_ips)
  k wait --for=condition=Ready pod/sim-timeline --timeout=2m
}

stop_samplers() {
  local label=$1
  kill "$sampler" || true
  k logs pod/sim-timeline >"$reports/$label-hits.log" || true
  k delete pod sim-timeline --wait=false
  k logs --timestamps job/inference-perf | grep -E 'Stage [0-9]+ - run (started|completed|failed)' \
    >"$reports/$label-stages.log" || true
}

run() {
  local arm=$1 label=$2 workload=${3:-job1} replicas extra=() jobs job
  replicas=$(epp_replicas "$workload")
  # Two pods' CPU requests don't fit the 12-CPU node at the shared 2-CPU default; limits are unchanged.
  [ "$replicas" = 1 ] || extra=(--set router.epp.resources.requests.cpu=1)
  # The chart embeds envoy.yaml as one string value, so lean mode edits the rendered manifest instead.
  [ "$arm" != prequal-lean ] ||
    extra+=(-f "$cfg/arm-prequal.yaml" --post-renderer sed "--post-renderer-args=$lean_envoy")
  [[ $arm != llmd* ]] || extra+=(--set "router.epp.image.tag=$router_version")
  [ -n "${round_locked:-}" ] || { exec 8>"$HOME/bench.lock"; flock 8; }
  deploy_sims "$workload"
  k rollout restart deploy/llm-d-sim && k rollout status deploy/llm-d-sim --timeout=5m  # cold caches
  ! helm status router -n bench >/dev/null 2>&1 || helm uninstall router -n bench --wait
  [ ! -f "$cfg/arm-$arm-extra.yaml" ] || k apply -f "$cfg/arm-$arm-extra.yaml"
  if [ "$arm" = router ]; then
    helm install router "$root/deploy/helm/prequal-router" -n bench -f "$cfg/arm-router.yaml"
  else
    helm install router "$chart" --version "$router_version" -n bench \
      -f "$baseline" -f "$cfg/common.yaml" -f "$cfg/arm-$arm.yaml" \
      "${extra[@]}"
  fi
  [ "$replicas" = 1 ] || k scale deploy/router-epp --replicas="$replicas"
  k rollout status deploy/router-epp --timeout=5m
  wait_quiet "$label"
  start_samplers "$label"
  start_jobs "$label" "$workload"
  jobs=$(job_specs "$workload" | cut -d' ' -f1)
  for job in $jobs; do
    k wait --for=condition=complete "job/$job" --timeout=60m || k logs "job/$job" --tail=30
  done
  stop_samplers "$label"
  scrape_sims "$label"
  for job in $jobs; do k delete "job/$job" "cm/$job-config" --ignore-not-found; done
  helm uninstall router -n bench --wait
  # Idle sims keep their KV cache resident (GBs each after a loaded run); deploy_sims scales them back up.
  k scale deploy/llm-d-sim --replicas=0
  [ -n "${round_locked:-}" ] || flock -u 8
}

# Other tenants' jobs skew timings. Holding the lock keeps locked peers out; this also waits (at most LOAD_WAIT s)
# for the 1-min load to fall below MAX_LOAD, then logs the start load, flagging runs that started above it.
wait_quiet() {
  local deadline=$((SECONDS + ${LOAD_WAIT:-1800})) max=${MAX_LOAD:-8} flag=
  while awk -v m="$max" '{exit !($1 >= m)}' /proc/loadavg && [ "$SECONDS" -lt "$deadline" ]; do sleep 15; done
  awk -v m="$max" '{exit !($1 >= m)}' /proc/loadavg && flag=" ABOVE-MAX_LOAD=$max"
  echo "$(date -Is) $1 ($router_version): $(uptime) $(free -g | awk '/^Mem/ {print $7 "G available"}')$flag" \
    >>"$reports/box-load.log"
}

# Alternating rounds, prequal first: prequal r1, llmd r1, prequal r2, ... Each round's arms run back to back under
# one lock hold. TAG distinguishes repeated suites; ARMS replaces the arm list.
suite() {
  local workload=${1:-job1} rounds=${2:-2} i arm round_locked=1
  for i in $(seq 1 "$rounds"); do
    exec 8>"$HOME/bench.lock"
    flock 8
    for arm in ${ARMS:-prequal llmd}; do run "$arm" "$arm-$workload${TAG:+-$TAG}-r$i" "$workload"; done
    flock -u 8
  done
}

# Per-stage fleet prefix-hit rate: counter deltas between the 5 s samples bracketing each stage (`<stage> <rate>`).
stage_hits() {
  local label=$1
  [ -s "$reports/$label-stages.log" ] && [ -s "$reports/$label-hits.log" ] || return 0
  sed -E 's/^([^ ]+) .*Stage ([0-9]+) - run (started|completed|failed).*/\1 \2 \3/' "$reports/$label-stages.log" |
    while read -r ts stage what; do echo "$stage $what $(date -d "$ts" +%s)"; done |
    awk 'NR == FNR {t[NR] = $1; h[NR] = $2; q[NR] = $3; n = NR; next}
      $2 == "started" {s[$1] = $3; next}
      { a = 1; for (i = 1; i <= n; i++) if (t[i] <= s[$1]) a = i
        b = n; for (i = n; i >= 1; i--) if (t[i] >= $3) b = i
        dq = q[b] - q[a]; print $1, (dq > 0 ? sprintf("%.3f", (h[b] - h[a]) / dq) : "null") }' \
      "$reports/$label-hits.log" -
}

# Peak per-sample totals across pods: CPU (millicores) and memory (MiB) per container, and the box's 1-min load.
peaks() {
  [ -s "$reports/$1-top.log" ] || return 0
  awk -v run="$1" '
    function flush(c) { for (c in cpu) { if (cpu[c] > pc[c]) pc[c] = cpu[c]; if (mem[c] > pm[c]) pm[c] = mem[c] }
      delete cpu; delete mem }
    $1 == "load" { flush(); if ($2 > load) load = $2; next }
    { cpu[$2] += $3 + 0; mem[$2] += $4 + 0 }
    END { flush(); printf "{\"run\":\"%s\",\"peak_load1\":%s", run, load + 0
      for (c in pc) printf ",\"%s_mcpu\":%d,\"%s_mib\":%d", c, pc[c], c, pm[c]; print "}" }' "$reports/$1-top.log"
}

results() {
  local dir label stage n hit
  for dir in "$reports"/${1:-*}-2*/; do
    label=$(basename "$dir" | sed 's/-[0-9].*$//')
    declare -A hits=()
    while read -r n hit; do hits[$n]=$hit; done < <(stage_hits "$label")
    for stage in "$dir"/stage_*_lifecycle_metrics.json; do
      n=$(basename "$stage" _lifecycle_metrics.json)
      jq -c --arg run "$label" --arg stage "$n" --argjson hit "${hits[${n#stage_}]:-null}" \
        '{run: $run, stage: $stage, ok: .successes.count, fail: .failures.count,
          ttft_p50: .successes.latency.time_to_first_token.median, ttft_p90: .successes.latency.time_to_first_token.p90,
          ttft_p99: .successes.latency.time_to_first_token.p99, out_tok_s: .successes.throughput.output_tokens_per_sec,
          prefix_hit: $hit}' "$stage"
    done
    unset hits
    [ -f "$reports/$label-sims.txt" ] && awk -v run="$label" \
      '/prefix_cache_hits(_total)?[{ ]/ {h += $NF} /prefix_cache_queries(_total)?[{ ]/ {q += $NF}
        END {if (q) printf "{\"run\":\"%s\",\"prefix_hit_rate\":%.3f}\n", run, h / q}' "$reports/$label-sims.txt"
    peaks "$label"
  done
}

teardown() {
  local i
  # Docker sometimes reports "did not receive an exit event"; a retry finishes the delete.
  for i in 1 2 3; do kind delete cluster --name bench && return; sleep 10; done
  return 1
}

# A prebuilt prequal-router (built on this box, glibc-compatible with the base) as the `router` arm's image.
router_image() {
  local ctx; ctx=$(mktemp -d)
  cp "${1:?prequal-router binary}" "$ctx/prequal-router"
  printf 'FROM gcr.io/distroless/cc-debian13:nonroot\nCOPY prequal-router /usr/local/bin/prequal-router\nENTRYPOINT ["/usr/local/bin/prequal-router"]\n' \
    | docker build -q -t local.dev/prequal-router:dev -f - "$ctx"
  rm -rf "$ctx"
  kind load docker-image local.dev/prequal-router:dev --name bench
}

[ "${BASH_SOURCE[0]}" = "$0" ] || return 0  # sourced by tools/kind-cache.sh for its functions

case ${1:-} in
  setup) setup ;;
  router-image) router_image "${@:2}" ;;
  run) run "${@:2}" ;;
  suite) suite "${@:2}" ;;
  results) results "${@:2}" ;;
  teardown) teardown ;;
  *) sed -n '2,9p' "$0"; exit 2 ;;
esac
