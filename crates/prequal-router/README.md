# prequal-router

A standalone, OpenAI-compatible router for vLLM and SGLang replicas: no gateway and no Envoy. It reads each request's
prompt, routes it with [`prequal-llm`](../prequal-llm)'s load- and prefix-cache-aware scheduler fed by the engines' own
Prometheus metrics, and streams responses (including SSE) back unchanged.

```sh
cargo install --git https://github.com/prequal-rs/prequal prequal-router
prequal-router --listen 0.0.0.0:8000 --engine vllm 10.0.0.1:8000 10.0.0.2:8000
# in Kubernetes, discovering ready pods behind a Service (needs RBAC `list` on endpointslices):
prequal-router --listen 0.0.0.0:8000 --engine vllm --k8s-service inference/vllm --k8s-port http
```

Clients use `/v1/completions` and `/v1/chat/completions` as usual. `--policy` selects a baseline for comparison, and
`--shards` adds serving threads above about 75 QPS of long streams. `/healthz` and `/readyz` are served on
`--admin-listen`. `prequal-router --help` lists the rest; see also [docs/configuration.md](../../docs/configuration.md).

See the [repository README](../../README.md) for the Helm chart, deployment modes and benchmarks. Requires Rust 1.89.
Licensed under MIT or Apache-2.0.
