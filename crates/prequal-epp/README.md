# prequal-epp

An endpoint picker (EPP) for the Kubernetes [Gateway API Inference Extension][gie]: Envoy calls it over `ext_proc`
and it picks the model-server Pod for each request with [`prequal-llm`](../prequal-llm)'s load- and
prefix-cache-aware scheduler. It serves `InferencePool` v1 and accepts the flags of llm-d's router charts, so it can
replace their picker image as-is.

```sh
cargo install prequal-epp
prequal-epp --pool-name vllm-pool --pool-namespace inference                     # in-cluster
prequal-epp --endpoints 127.0.0.1:8001,127.0.0.1:8002 --secure-serving=false    # local testing
```

Useful flags: `--engine vllm|sglang`, `--policy`, and `--prefill-signal scrape|headers|...` for lean mode (Envoy's
`response_body_mode: NONE`). `prequal-epp --help` lists the rest; flags, metrics and error codes are documented in
[docs/configuration.md](../../docs/configuration.md).

See the [repository README](../../README.md) for the quick start with llm-d, deployment modes and benchmarks. Requires
Rust 1.89. Licensed under MIT or Apache-2.0.

[gie]: https://github.com/kubernetes-sigs/gateway-api-inference-extension
