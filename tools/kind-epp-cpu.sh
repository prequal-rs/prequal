#!/usr/bin/env bash
# EPP/Envoy CPU per request on llm-d's real stack in kind (Envoy sidecar, llm-d-inference-sim, inference-perf).
# Runs in the shared `bench` cluster of tools/kind-llmd.sh (set up there) only while holding ~/bench.lock, with its
# own image tags, job and report subdirectory, so it never disturbs that script's runs. Don't start a second
# cluster beside it: a loaded run's 10 sims grow to ~2-3 GB each and two clusters OOM the 31 GB box.
# Usage: tools/kind-epp-cpu.sh image <tag> [prebuilt-binary] | cpu <rate> <seconds> <variant>...
#   variant = <tag>[,<epp-flag>=<value>...], e.g. c2,ext-proc-coalesce-us=250. One lock hold measures every variant:
#   30 s idle, then `seconds` of steady load from WARM (45) s after the first routed request. PERF=<s> also
#   samples the EPP with perf for that long (CG=lbr|dwarf) into ~/eppperf/prof/<variant>/; TRACE=1 then counts its
#   syscalls for 20 s (tools/epp-syscalls.bt).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cfg="$root/deploy/bench/llmd"
work=$HOME/eppperf
chart=oci://ghcr.io/llm-d/charts/llm-d-router-standalone
router_version=v0.10.0

k() { kubectl -n bench "$@"; }
node() { docker exec bench-control-plane "$@"; }

# deploy/epp.Dockerfile (the deployed build), or a prebuilt binary on a glibc-compatible base for fast iteration.
image() {
  local ref=local.dev/prequal-epp:${1:?tag}
  if [ -n "${2:-}" ]; then
    local ctx; ctx=$(mktemp -d)
    cp "$2" "$ctx/prequal-epp"
    printf 'FROM gcr.io/distroless/cc-debian13:nonroot\nCOPY prequal-epp /usr/local/bin/prequal-epp\nENTRYPOINT ["/usr/local/bin/prequal-epp"]\n' \
      | docker build -q -t "$ref" -f - "$ctx"
    rm -rf "$ctx"
  else
    docker build -q -t "$ref" -f "$root/deploy/epp.Dockerfile" "$root"
  fi
  kind load docker-image "$ref" --name bench
}

# CPU seconds (utime+stime) of a router-epp container's main process, read inside the kind node.
container_cpu() {
  local id pid
  id=$(node crictl ps -q --name "^$1\$" --label io.kubernetes.pod.namespace=bench | head -1)
  pid=$(node crictl inspect --output go-template --template '{{.info.pid}}' "$id")
  node cat "/proc/$pid/stat" | awk '{sub(/.*\) /, ""); printf "%.2f", ($12 + $13) / 100}'
}

sample() {
  local ip; ip=$(k get pod -o wide --no-headers | awk '/^router-epp/ && /Running/ {print $6; exit}')
  printf '%s t=%s epp=%s envoy=%s ' "$1" "$(date +%s.%N)" "$(container_cpu epp)" "$(container_cpu envoy-proxy)"
  node curl -s "$ip:9090/metrics" | awk '/^prequal_epp_(picks|ext_proc_messages)_total/ {
    split($1, a, "\""); printf "%s=%s ", a[2], $2 }'
  echo
}

# Per-request CPU between two `sample` lines over routed picks, plus inbound ext_proc messages per request by type.
diff_samples() {
  awk '{ for (i = 2; i <= NF; i++) { split($i, kv, "="); v[NR, kv[1]] = kv[2]; if (NR == 2) keys[kv[1]] } }
    END { dt = v[2, "t"] - v[1, "t"]; n = v[2, "routed"] - v[1, "routed"]
      printf "secs=%.0f picks=%d epp_cores=%.3f envoy_cores=%.3f", dt, n, (v[2, "epp"] - v[1, "epp"]) / dt,
        (v[2, "envoy"] - v[1, "envoy"]) / dt
      if (n > 0) {
        printf " epp_ms/req=%.2f envoy_ms/req=%.2f", (v[2, "epp"] - v[1, "epp"]) * 1e3 / n,
          (v[2, "envoy"] - v[1, "envoy"]) * 1e3 / n
        for (key in keys) if (key ~ /^(request|response)_/) printf " %s/req=%.1f", key, (v[2, key] - v[1, key]) / n
      }
      print "" }'
}

# Own inference-perf job (deploy/bench/llmd/job.yaml belongs to kind-llmd.sh and changes under it): llm-d's
# shared_prefix data at one constant rate, reports in a subdirectory outside kind-llmd.sh's `results` glob.
job() {
  cat <<EOF
apiVersion: v1
kind: ConfigMap
metadata: {name: epp-cpu-load, namespace: bench}
data:
  config.yml: |
    load: {type: constant, stages: [{rate: $1, duration: $2}], num_workers: 10, worker_max_concurrency: 100,
           worker_max_tcp_connections: 2500}
    api: {type: completion, streaming: true}
    server: {type: vllm, model_name: Qwen/Qwen3-8B, base_url: "http://router-epp:80", ignore_eos: true}
    data: {type: shared_prefix, shared_prefix: {num_groups: 150, num_prompts_per_group: 5, system_prompt_len: 9500,
           question_len: 500, output_len: 1000, enable_multi_turn_chat: false}}
    report: {request_lifecycle: {summary: true, per_stage: true, per_request: false}}
    storage: {local_storage: {path: "/reports/eppperf/cpu-$1-{timestamp}"}}
---
apiVersion: batch/v1
kind: Job
metadata: {name: epp-cpu-load, namespace: bench}
spec:
  backoffLimit: 0
  template:
    metadata: {labels: {app: inference-perf}}
    spec:
      restartPolicy: Never
      containers:
      - name: inference-perf
        image: quay.io/inference-perf/inference-perf:v0.6.0
        command: [inference-perf]
        args: [--config_file, /cfg/config.yml, --log-level, INFO]
        resources: {requests: {cpu: "4", memory: 4Gi}}
        volumeMounts: [{name: cfg, mountPath: /cfg}, {name: reports, mountPath: /reports}]
      volumes:
      - {name: cfg, configMap: {name: epp-cpu-load}}
      - {name: reports, hostPath: {path: /reports, type: Directory}}
EOF
}

# Leaves the shared cluster as kind-llmd.sh expects it. Loaded sims hold GBs until restarted; its next run
# re-applies them.
cleanup() {
  k delete job/epp-cpu-load cm/epp-cpu-load --ignore-not-found >/dev/null
  helm uninstall router -n bench >/dev/null 2>&1 || true
  k scale deploy/llm-d-sim --replicas=0 >/dev/null
}

profile() {
  local dir=$work/prof/$1 pid
  pid=$(pgrep -f '^/usr/local/bin/prequal-epp --pool-name')
  rm -rf "$dir"; mkdir -p "$dir"
  sudo perf record -q -F 499 --call-graph "${CG:-lbr}" -p "$pid" -o "$dir/perf.data" -- sleep "$PERF" 2>/dev/null
  sudo perf report --no-inline -i "$dir/perf.data" --no-children --sort symbol --percentage relative --stdio -g none \
    --percent-limit 0.4 2>/dev/null | grep -v -e '^#' -e '^$' | ~/.cargo/bin/rustfilt | cut -c1-200 >"$dir/self.txt"
  sudo perf script --no-inline -i "$dir/perf.data" 2>/dev/null | ~/.cargo/bin/inferno-collapse-perf |
    ~/.cargo/bin/rustfilt >"$dir/folded.txt"
}

measure() {
  local rate=$1 seconds=$2 variant=$3 tag=${3%%,*} flags=() a b f
  IFS=, read -ra f <<<"${variant#"$tag"}"
  for kv in "${f[@]}"; do [ -n "$kv" ] && flags+=(--set "router.epp.flags.${kv%%=*}=${kv#*=}"); done
  k rollout restart deploy/llm-d-sim >/dev/null && k rollout status deploy/llm-d-sim --timeout=5m >/dev/null
  helm uninstall router -n bench >/dev/null 2>&1 || true
  helm install router "$chart" --version "$router_version" -n bench -f "$work/optimized-baseline.yaml" \
    -f "$cfg/common.yaml" -f "$cfg/arm-prequal.yaml" --set "router.epp.image.tag=$tag" "${flags[@]}" >/dev/null
  k rollout status deploy/router-epp --timeout=5m >/dev/null
  sleep 20
  a=$(sample idle); sleep 30; b=$(sample idle)
  echo "$variant idle: $(printf '%s\n%s\n' "$a" "$b" | diff_samples)"
  job "$rate" $((${WARM:-45} + seconds + ${PERF:-0} + ${TRACE:+20} + 30)) | kubectl apply -f - >/dev/null
  until sample wait | grep -q 'routed=[1-9]'; do sleep 2; done
  sleep "${WARM:-45}"
  a=$(sample load); sleep "$seconds"; b=$(sample load)
  echo "$variant load: $(printf '%s\n%s\n' "$a" "$b" | diff_samples)"
  [ -z "${PERF:-}" ] || profile "$variant"
  [ -z "${TRACE:-}" ] || sudo bpftrace -q "$root/tools/epp-syscalls.bt" "$(pgrep -f '^/usr/local/bin/prequal-epp --pool-name')"
  k delete job/epp-cpu-load cm/epp-cpu-load --wait=true >/dev/null
}

cpu() {
  local rate=$1 seconds=$2 variant
  exec 8>"$HOME/bench.lock"
  flock 8
  trap cleanup EXIT
  sed 's|SIM_EXTRA_ARGS|--max-num-seqs, "32", --time-factor-under-load, "4"|' "$cfg/sim.yaml" | kubectl apply -f - >/dev/null
  for variant in "${@:3}"; do measure "$rate" "$seconds" "$variant"; done
}

case ${1:-} in
  image) image "${@:2}" ;;
  cpu) cpu "${@:2}" ;;
  *) sed -n '2,9p' "$0"; exit 2 ;;
esac
