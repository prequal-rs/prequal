# Deployment artefacts

All commands run from the repository root. Flags, metrics, error codes and supported versions are in
[docs/configuration.md](../docs/configuration.md).

| Path | What it is |
|---|---|
| [`epp.Dockerfile`](epp.Dockerfile) | `prequal-epp` image (distroless, non-root). Ports 9002 (`ext_proc` gRPC), 9003 (gRPC health), 9090 (Prometheus `/metrics`) |
| [`Dockerfile`](Dockerfile) | `prequal-router` image (distroless, non-root). Ports 8000 (OpenAI API), 8081 (`/healthz`, `/readyz`) |
| [`epp/prequal-epp.yaml`](epp/prequal-epp.yaml) | `prequal-epp` as the endpoint picker of an `InferencePool` (Gateway API Inference Extension v1), for any conformant gateway |
| [`helm/prequal-router`](helm/prequal-router) | Helm chart for the standalone router, discovering ready pods behind a Service |
| [`grafana`](grafana) | Grafana dashboard (`prequal-epp.json`) and Prometheus alert rules (`alerts.yaml`) for `prequal-epp` |
| [`demo`](demo/compose.yaml) | Local A/B in Docker Compose: `prequal-router` vs round-robin on two simulated vLLM fleets |
| [`bench/llmd`](bench/llmd/README.md) | Benchmark manifests for llm-d's stack in kind, driven by `tools/kind-llmd.sh` and `tools/kind-cache.sh` |

## Images

Tagged releases publish `ghcr.io/prequal-rs/prequal-epp`, `ghcr.io/prequal-rs/prequal-router` (linux/amd64 and arm64) and the
chart `oci://ghcr.io/prequal-rs/charts/prequal-router`. To build your own:

```sh
docker build -f deploy/epp.Dockerfile -t <registry>/prequal-epp:0.1.0 .
docker build -f deploy/Dockerfile -t <registry>/prequal-router:0.1.0 .
```

The `prequal-epp` image contains a static `/bin/sh` that only runs `-c "sleep <seconds>"`, for the preStop hook
llm-d's chart hard-codes; the image has no real shell.

## prequal-epp in llm-d

llm-d's `llm-d-router-standalone` chart (v0.10, v0.11) runs the picker with Envoy as a sidecar. `prequal-epp` accepts
the flags the chart passes to every picker (logging, tracing, pprof, config file, leader election; none change its
behaviour, and it logs a warning for any that asks for something it doesn't do), so only the image changes:

```sh
helm install router oci://ghcr.io/llm-d/charts/llm-d-router-standalone --version v0.11.0 \
  --set router.epp.image.registry=ghcr.io/prequal-rs \
  --set router.epp.image.repository=prequal-epp --set router.epp.image.tag=0.1.0
```

`prequal-epp` flags go under `router.epp.flags`, e.g. `--set router.epp.flags.engine=sglang`. For lean mode (Envoy
skips response bodies), see [docs/lean-mode.md](../docs/lean-mode.md).

## prequal-epp with another gateway

[`epp/prequal-epp.yaml`](epp/prequal-epp.yaml) creates the picker's ServiceAccount and RBAC (read `InferencePool`s,
list Pods and, optionally, `InferenceObjective`s), a one-replica Deployment, its Service, and an `InferencePool` named
`vllm-pool` selecting pods labelled `app: vllm` on port 8000. Edit the image, the pool's selector and port, and
`--engine` to match your model servers, then:

```sh
kubectl apply -n <namespace> -f deploy/epp/prequal-epp.yaml
```

Route to it from your gateway with an `HTTPRoute` whose `backendRef` is
`{group: inference.networking.k8s.io, kind: InferencePool, name: vllm-pool}`. Tested gateways: agentgateway (passes
the GIE conformance suite with `prequal-epp` as every pool's picker) and llm-d's standalone Envoy. Istio does not
work: its ext_proc requests carry an invalid `:authority` ([docs/conformance.md](../docs/conformance.md#istio)). The
picker serves `ext_proc` over TLS with a self-signed certificate by default (`--secure-serving=false` for plaintext).

**Replicas.** Every replica serves; there is no leader election. More than one replica works (the `two-epp`
benchmarks run two), but each keeps its own prefix-cache model, so one replica gets the best cache hit rate
([details](../docs/configuration.md#lifecycle-and-replicas)).

## prequal-router

Static replicas, no Kubernetes:

```sh
cargo run --release -p prequal-router -- --listen 0.0.0.0:8000 --engine vllm 10.0.0.1:8000 10.0.0.2:8000
```

In Kubernetes, the chart lists the target Service's EndpointSlices and routes only to ready pods:

```sh
helm install router oci://ghcr.io/prequal-rs/charts/prequal-router --version 0.1.0 \
  --set target.service=vllm --set target.engine=vllm
```

Values ([`values.yaml`](helm/prequal-router/values.yaml)): `target.service`, `target.namespace` (default: the
release namespace), `target.port` (Service port name or number, default `http`), `target.engine`,
`target.metricsPath`, `policy`, `extraArgs` (any other `prequal-router` flag, e.g. `[--shards=2]`), `replicaCount`
(default 2), `listenPort`, `adminPort` (8081, for the readiness and liveness probes), `service.port`, `resources`.
RBAC is a Role allowing `list` on EndpointSlices in the target namespace only.
