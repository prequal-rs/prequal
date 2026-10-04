#!/usr/bin/env bash
# Gateway API Inference Extension conformance suite (Gateway profile) in kind, with every InferencePool's endpoint
# picker being either the suite's reference lwepp (control) or prequal-epp. See docs/conformance.md.
# Usage: [GATEWAY=agentgateway|istio] tools/gie-conformance.sh
#          setup | run <prequal|lwepp> [extra suite args, e.g. -run-test X] | report | teardown
# Needs docker, kind, kubectl, helm, go, git, curl on a Linux host. One kind cluster per shared box: setup refuses to
# start while another exists. Both gateways can be installed in the same cluster. `run` holds ~/bench.lock.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cluster=${CLUSTER:-gie-conformance}
gateway=${GATEWAY:-agentgateway}
gie_version=${GIE_VERSION:-v1.6.2}
gateway_api_version=${GATEWAY_API_VERSION:-v1.6.1} # what the suite at $gie_version is built against
istio_version=${ISTIO_VERSION:-1.31.1}
agentgateway_version=${AGENTGATEWAY_VERSION:-v1.5.0}
metallb_version=v0.14.9
work=${WORK:-$HOME/gie-conformance}
reports="$work/reports/$gateway"
epp_image=local.dev/prequal-epp:conformance
lwepp_image="registry.k8s.io/gateway-api-inference-extension/lwepp:$gie_version"
echo_image=gcr.io/k8s-staging-gateway-api/echo-basic:v20251106-v1.3.0-263-g47c3435c
istioctl="$work/bin/istioctl-$istio_version"

node_pull() { docker exec "$cluster-control-plane" crictl pull "$1" >/dev/null; }

install_metallb() {
  kubectl apply -f "https://raw.githubusercontent.com/metallb/metallb/$metallb_version/config/manifests/metallb-native.yaml"
  kubectl -n metallb-system rollout status deploy/controller --timeout=3m
  kubectl -n metallb-system wait pod --all --for=condition=Ready --timeout=3m
  local subnet prefix
  subnet=$(docker network inspect kind -f '{{range .IPAM.Config}}{{.Subnet}} {{end}}' | tr ' ' '\n' | grep -m1 '\.')
  prefix=$(echo "$subnet" | cut -d. -f1-2)
  # The webhook can lag the controller's readiness; retry the pool until it is accepted.
  for _ in $(seq 30); do
    kubectl apply -f - <<EOF && return
apiVersion: metallb.io/v1beta1
kind: IPAddressPool
metadata: {name: kind, namespace: metallb-system}
spec: {addresses: ["$prefix.255.200-$prefix.255.250"]}
---
apiVersion: metallb.io/v1beta1
kind: L2Advertisement
metadata: {name: kind, namespace: metallb-system}
EOF
    sleep 5
  done
  return 1
}

install_istio() {
  if [ ! -x "$istioctl" ]; then
    mkdir -p "$work/bin"
    curl -fsSL "https://github.com/istio/istio/releases/download/$istio_version/istioctl-$istio_version-linux-amd64.tar.gz" \
      | tar -xz -C "$work/bin"
    mv "$work/bin/istioctl" "$istioctl"
  fi
  "$istioctl" install -y --set profile=minimal \
    --set values.pilot.env.ENABLE_GATEWAY_API_INFERENCE_EXTENSION=true \
    --set values.pilot.env.SUPPORT_GATEWAY_API_INFERENCE_EXTENSION=true
  kubectl wait gatewayclass/istio --for=condition=Accepted --timeout=3m
  epp_tls_rules
}

# agentgateway dials every pool's picker over TLS itself (no extra resources needed).
install_agentgateway() {
  local chart=oci://cr.agentgateway.dev/charts
  helm upgrade -i agentgateway-crds "$chart/agentgateway-crds" --version "$agentgateway_version" \
    -n agentgateway-system --create-namespace --wait
  helm upgrade -i agentgateway "$chart/agentgateway" --version "$agentgateway_version" \
    -n agentgateway-system --set inferenceExtension.enabled=true --wait
  kubectl wait gatewayclass/agentgateway --for=condition=Accepted --timeout=3m
}

# Both pickers serve ext_proc over TLS (self-signed) and Istio dials plaintext unless told otherwise. Root-namespace
# rules apply mesh-wide and survive the suite deleting its namespaces after each run.
epp_tls_rules() {
  local svc
  for svc in primary secondary appprotocol-http appprotocol-h2c dp; do
    cat <<EOF
---
apiVersion: networking.istio.io/v1
kind: DestinationRule
metadata: {name: conformance-$svc-epp-tls, namespace: istio-system}
spec:
  host: $svc-endpoint-picker-svc.inference-conformance-app-backend.svc.cluster.local
  trafficPolicy: {tls: {mode: SIMPLE, insecureSkipVerify: true}}
EOF
  done | kubectl apply -f -
}

setup() {
  local others
  others=$(kind get clusters 2>/dev/null | grep -vx "$cluster" || true)
  if [ -n "$others" ]; then
    echo "another kind cluster exists on this host ($others): one cluster at a time; wait for it" >&2
    exit 1
  fi
  mkdir -p "$reports"
  kind get clusters 2>/dev/null | grep -qx "$cluster" || kind create cluster --name "$cluster"
  kubectl apply --server-side -f \
    "https://github.com/kubernetes-sigs/gateway-api/releases/download/$gateway_api_version/standard-install.yaml"
  kubectl apply --server-side -f \
    "https://github.com/kubernetes-sigs/gateway-api-inference-extension/releases/download/$gie_version/manifests.yaml"
  install_metallb
  "install_$gateway"
  node_pull "$lwepp_image"
  node_pull "$echo_image"
  docker build -q -t "$epp_image" -f "$root/deploy/epp.Dockerfile" "$root"
  kind load docker-image "$epp_image" --name "$cluster"
  [ -d "$work/gie.git" ] || git clone -q --bare https://github.com/kubernetes-sigs/gateway-api-inference-extension "$work/gie.git"
  git -C "$work/gie.git" fetch -q --tags
}

# The suite's base manifests run lwepp as all five pools' pickers; the prequal arm swaps in prequal-epp with its
# conformance hooks on, changing nothing else (same args, ports, probes).
swap_epp() {
  local base=$1/conformance/resources/base.yaml
  awk -v from="$lwepp_image" -v to="$epp_image" '
    $1 == "image:" && $2 == from { sub(from, to); swapped++ }
    { print }
    $1 == "args:" && swapped > args { print "        - --conformance-test-hooks"; args++ }
    END { if (swapped != 5 || args != 5) { print "expected 5 lwepp pickers, swapped " swapped > "/dev/stderr"; exit 1 } }
  ' "$base" >"$base.new"
  mv "$base.new" "$base"
}

gateway_version() {
  case $gateway in
    istio) echo "$istio_version" ;;
    agentgateway) echo "$agentgateway_version" ;;
  esac
}

run() {
  local arm=$1 src="$work/src-$1" project version mode gw
  shift
  gw="$gateway $(gateway_version)"
  case $arm in
    prequal) project=prequal-epp version=$(git -C "$root" describe --always 2>/dev/null || echo 0.1.0-dev) mode=epp-prequal ;;
    lwepp) project=lwepp version=$gie_version mode=epp-lwepp ;;
    *) echo "arm must be prequal or lwepp" >&2; exit 2 ;;
  esac
  rm -rf "$src" && mkdir -p "$src" "$reports"
  git -C "$work/gie.git" archive "$gie_version" | tar -x -C "$src"
  [ "$arm" = lwepp ] || swap_epp "$src"
  exec 8>"$HOME/bench.lock"
  flock 8
  # An interrupted run leaves its namespaces behind; start each arm from a clean slate.
  kubectl delete ns inference-conformance-infra inference-conformance-app-backend --ignore-not-found --wait --timeout=5m
  echo "$(date -Is) $arm: $gw, gie $gie_version, $(uptime)" | tee "$reports/$arm.log"
  (cd "$src/conformance" && go test -v -count=1 -timeout 90m . -args -gateway-class "$gateway" \
    -report-output "$reports/$arm-report.yaml" -organization "prequal-rs/prequal (unofficial run)" \
    -project "$project on $gw" -url https://github.com/prequal-rs/prequal -version "$version" \
    -contact https://github.com/prequal-rs/prequal/issues -mode "$mode" "$@") 2>&1 | tee -a "$reports/$arm.log" || true
  flock -u 8
  summary "$arm"
}

# Top-level test results from a run's log.
summary() {
  echo "== $1"
  grep -E '^    --- (PASS|FAIL|SKIP): TestConformance/[A-Za-z]+ ' "$reports/$1.log" | sed -E 's/^ +--- //; s/TestConformance\///' || true
  grep -A12 '^profiles:' "$reports/$1-report.yaml" 2>/dev/null || echo "(no report)"
}

report() {
  echo "# $gateway $(gateway_version), GIE $gie_version"
  for arm in lwepp prequal; do [ ! -f "$reports/$arm.log" ] || summary "$arm"; done
}

teardown() {
  for _ in 1 2 3; do kind delete cluster --name "$cluster" && return; sleep 10; done
  return 1
}

case ${1:-} in
  setup) setup ;;
  run) run "${@:2}" ;;
  report) report ;;
  teardown) teardown ;;
  *) sed -n '2,7p' "$0"; exit 2 ;;
esac
