# Configuration and operations

Reference for running `prequal-epp` and `prequal-router`. `--help` on either binary lists every flag with its default.

## Supported versions

| Component | Versions | Notes |
|---|---|---|
| llm-d `llm-d-router-standalone` chart | v0.10.0, v0.11.0 | The chart's flags are accepted verbatim; tested in kind with Envoy as the chart configures it |
| Gateway API Inference Extension | `InferencePool` v1 (`inference.networking.k8s.io`) | Conformance suite v1.6.2, Gateway profile: 14/14 behind agentgateway v1.5.0 ([conformance](conformance.md)) |
| Gateways | llm-d's standalone Envoy, agentgateway v1.5.0 | Istio 1.31.1 does not work: its ext_proc requests carry an invalid `:authority` ([details](conformance.md#istio)) |
| Model servers | vLLM and SGLang Prometheus metrics (`--engine vllm\|sglang`) | Benchmarked against llm-d-inference-sim v0.10.2, which exports vLLM's metric names. Not yet benchmarked on GPUs |
| Kubernetes | 1.32 or later | Built against the 1.32 API; tested on 1.37 in kind |
| Rust | Libraries 1.85; `prequal-epp`, `prequal-router` and `prequal-pingora`'s `kubernetes` feature 1.89 | |

## prequal-epp

### Flags

| Flag | Default | |
|---|---|---|
| `--pool-name`, `--pool-namespace`, `--pool-group` | –, `default`, `inference.networking.k8s.io` | The `InferencePool` to serve; its Pods and `InferenceObjective`s are read from the same namespace |
| `--endpoints ip:port,...` | | Fixed endpoints instead of a pool, for local testing |
| `--engine` | `vllm` | `vllm` or `sglang`: which metric names to scrape |
| `--metrics-path`, `--scrape-ms` | `/metrics`, 50 | Model-server metrics path and scrape interval |
| `--policy` | `prequal` | See [Routing policies](#routing-policies) |
| `--prefill-signal` | `first-chunk` | What ends a request's prefill reservation. `scrape` (vLLM) or `headers` (SGLang) for [lean mode](lean-mode.md) |
| `--grpc-port`, `--grpc-health-port`, `--metrics-port` | 9002, 9003, 9090 | ext_proc, gRPC health, Prometheus `/metrics` |
| `--secure-serving` | `true` | ext_proc over TLS with a self-signed certificate; `--secure-serving=false` for plaintext |
| `--refresh-ms` | 1000 | How often the pool, its Pods and `InferenceObjective`s are re-read |
| `--fallbacks` | 0 | Alternative endpoints listed after the pick, for gateways that retry down the list |
| `--max-concurrent-streams` | 20000 | ext_proc streams (one per in-flight request) open at once; beyond it, `RESOURCE_EXHAUSTED`, so the gateway's failure mode applies. 0 = unlimited |
| `--max-buffered-body-mib` | 1024 | Request bodies buffered across all streams while awaiting a pick; requests beyond it get 503. Each body is also capped at 10 MiB (413), as in llm-d |
| `--drain-timeout` | `25s` | Time in-flight streams get after SIGTERM; keep it below the pod's `terminationGracePeriodSeconds` |
| `--ext-proc-threads`, `--ext-proc-coalesce-us` | 1, 100 | Threads serving ext_proc, and how long a thread lingers to batch messages. Raise threads only if one saturates |
| `--admission-limit` | 0 | Experimental late binding; leave off |
| `--engine-priority-handicap` | off | Experimental. Stamps each body with a vLLM `priority` so requests predicted to hold little KV run first; a costlier request is overtaken only by ones arriving within this long after it (e.g. `60s`). `InferenceObjective` priority still dominates, and a client's own `priority` is overridden. vLLM only (`--engine vllm`), and every server needs `--scheduling-policy priority`, which otherwise rejects the field. The stamps are not comparable with llm-d's `requestHandler.propagatePriority` values, so don't front one pool with both. Learns output lengths from response bodies, so lean mode leaves it in arrival order |
| `--gossip-peers`, `--gossip-port` | off, 9004 | Experimental. `host:port` resolving to every picker of the pool (a headless Service, re-resolved every 5 s): each picker tells the others over UDP where it sent each prompt, so replicas share one view of the fleet's prefix caches ([peer-index](peer-index.md)). The messages are unauthenticated; restrict the port to the pickers ([`gossip.yaml`](../deploy/epp/gossip.yaml)) |
| `--conformance-test-hooks` | off | Test only: the conformance suite's endpoint-selection request header and served-endpoint response header. Any client could use the header to steer its requests |

**llm-d chart flags.** `prequal-epp` accepts the flags llm-d's charts and the conformance manifests pass to every
picker: `--zap-encoder`, `--zap-log-level`, `--v`, `--config-file`, `--config-text`, `--tracing`,
`--metrics-endpoint-auth`, `--enable-pprof` and `--ha-enable-leader-election`, repeated or not. None change its
behaviour. When a value asks for something `prequal-epp` doesn't do, it logs one warning at startup, for example that
a `--config-file`'s plugins are not applied (routing is set by `--policy`) or that `/metrics` has no authentication.

### Lifecycle and replicas

- **Health.** gRPC health on `--grpc-health-port`. `liveness` always serves. `readiness` (also answered as
  `inference-extension` and `envoy.service.ext_proc.v3.ExternalProcessor`) serves once endpoints are known.
- **Shutdown.** On SIGTERM or SIGINT, readiness turns `NOT_SERVING`, new ext_proc streams are refused, and in-flight
  streams get `--drain-timeout` to finish.
- **preStop.** llm-d's chart (v0.11) hard-codes a `/bin/sh -c "sleep 5"` preStop hook. The image is distroless, so it
  ships a static `/bin/sh` that only runs `-c "sleep <seconds>"` and refuses anything else. The hook succeeds without
  adding a shell to the image.
- **Replicas.** Every replica serves: there is no leader election, and `--ha-enable-leader-election` is accepted
  but ignored (active-active). Each replica keeps its own prefix-cache model. Replicas notice each other's load on
  the shared fleet and then place new prefixes by a hash they all share, so they mostly agree. One replica still gets
  the best cache hit rate: in the [cache-pressure benchmark](benchmarks.md#2-cache-pressure), 0.625 with one picker
  against 0.563 with two. `--gossip-peers` (experimental) closes that gap: 0.623 with two pickers
  ([peer-index](peer-index.md#real-pickers-in-kind)).

### Request outcomes

Errors follow llm-d's EPP: an immediate response with the status and the body
`inference error: <error_code> - <message>`, counted in `llm_d_epp_request_error_total{error_code}`. A 503 for want of
endpoints also carries `x-llm-d-request-dropped-reason: rejected-no-endpoints`.

| Status | `error_code` | When |
|---|---|---|
| 503 | `ServiceUnavailable` | No ready endpoint (or none allowed by the endpoint-selection hint), or the request-body buffers are full |
| 429 | `ResourceExhausted` | A sheddable request while the pool is saturated (below) |
| 413 | `BadRequest` | Request body over 10 MiB |

**Priority and shedding.** As in llm-d, the `x-llm-d-inference-objective` header (alias
`x-gateway-inference-objective`) names an `InferenceObjective` for this pool; its `spec.priority` applies. Unknown or
unset is 0. A negative priority marks the request sheddable. It is rejected with 429 when the pool's saturation, the
mean over endpoints of `max(waiting / 5, KV-cache use / 0.8)` (a stale or unscraped endpoint counting 1), reaches 1.
Reading `InferenceObjective`s needs RBAC `list` on `inferenceobjectives`; without it every request has priority 0.
`prequal-epp` has no flow control: the fairness ID header (`x-llm-d-inference-fairness-id`) is only a metric label.

### Metrics

`/metrics` on `--metrics-port` serves llm-d's EPP series with llm-d's names, labels and buckets, so existing
dashboards keep working, plus `prequal-epp`'s own.

| Series | Labels | Compared with llm-d |
|---|---|---|
| `llm_d_epp_request_total`, `_request_error_total`, `_request_running` | `model_name`, `target_model_name`, `fairness_id`, `priority` (+ `error_code`) | Same. `target_model_name` always equals `model_name`: no model rewrite |
| `llm_d_epp_request_duration_seconds`, `_request_size_bytes`, `_response_size_bytes` | as above | Same buckets. Response size and duration need response bodies, so they are absent in lean mode |
| `llm_d_epp_scheduler_e2e_duration_seconds` | | Same |
| `llm_d_epp_ready_endpoints`, `_average_queue_size`, `_std_dev_queue_size`, `_average_kv_cache_utilization`, `_std_dev_kv_cache_utilization`, `_average_running_requests`, `_std_dev_running_requests` | `name` (pool) | Same |
| `llm_d_epp_per_endpoint_queue_size` | `name`, `model_server_endpoint` | Same |
| `llm_d_epp_inflight_requests` | `endpoint_name`, `namespace` | No `fairness_id` or `priority` split |
| `llm_d_epp_extproc_streams_inflight`, `_extproc_streams_total` | `code` (total only) | Same |
| `llm_d_epp_info` | `commit`, `build_ref` | Same |
| `prequal_epp_picks_total` | `result` (`routed`, `rejected`) | prequal-epp only |
| `prequal_epp_ext_proc_messages_total`, `_ext_proc_message_body_bytes_total` | `type` | prequal-epp only: ext_proc messages by kind, which shows lean mode's effect |
| `process_cpu_seconds_total`, `process_resident_memory_bytes` | | |

Not exported: llm-d's flow-control, latency-predictor, KV-cache-event and plugin-specific series, since `prequal-epp`
has none of those components.

A Grafana dashboard and Prometheus alert rules for these series are in [deploy/grafana](../deploy/grafana).

## prequal-router

| Flag | Default | |
|---|---|---|
| `host:port ...` or `--k8s-service namespace/name` | | Replicas by IP or DNS name, or ready pods behind a Service (needs RBAC `list` on `endpointslices`). Names are re-resolved every 2 s, and a name with several addresses (a headless Service, scaled compose services) adds one replica per address; a name that fails to resolve keeps its last addresses |
| `--k8s-port` | `http` | Service port name or number |
| `--listen` | `127.0.0.1:8000` | Client (OpenAI API) traffic |
| `--admin-listen` | `127.0.0.1:8081` | `/healthz` (liveness) and `/readyz` (a replica is up and the router is not draining) |
| `--engine`, `--metrics-path`, `--scrape-ms`, `--policy` | `vllm`, `/metrics`, 50, `prequal` | As for `prequal-epp` |
| `--max-body-mib` | 16 | Largest request body (413 above it) |
| `--max-buffered-mib` | 64 | Request bytes buffered across all requests; beyond it, 503 |
| `--max-connections`, `--max-streams` | 4096, 256 | Client connections served at once (more wait in the backlog); HTTP/2 streams per connection |
| `--header-timeout-secs`, `--body-timeout-secs` | 30, 60 | Client request headers (HTTP/1) and body (408 after); 0 = no limit |
| `--connect-timeout-ms` | 5000 | Connecting to a replica (502 after) |
| `--response-timeout-secs` | 600 | Waiting for a replica's response headers (504 after). Bounds non-streaming completions too; streamed bodies have no limit |
| `--drain-secs` | 25 | After SIGTERM, `/readyz` fails and in-flight requests get this long; keep it below `terminationGracePeriodSeconds` |
| `--shards`, `--coalesce-us` | 1, 100 | Single-threaded runtimes serving clients, and how long one lingers before parking. Raise shards above about 75 QPS of long streams |
| `--admission-limit` | 0 | Experimental late binding; leave off |

With no replica up, requests get 503. The [Helm chart](../deploy/helm/prequal-router) wires `/readyz` and `/healthz`
to the admin port and runs two replicas; each router replica keeps its own cache model, as with `prequal-epp`.

## Routing policies

`--policy` takes `prequal` (the default) or a baseline reimplemented for comparison: `round-robin`,
`least-request`, `llmd-default`, `llmd-optimized`, `llmd-guide`, `sglang-cache-aware`, `dynamo`.

`prequal-home` is `prequal` plus one opt-in rule for engines whose prefix-cache counters `prequal` can't read
(SGLang): a prompt nobody is believed to hold goes back to the replica its key last went to. Without those counters
`prequal` sizes each replica's cache model assuming 4 bytes of request per engine token, and when prompts run larger
it forgets prefixes the engine still holds. In simulation at 12 bytes per token, `prequal-home` raised the hit rate
from 61% to 88%; where the 4-byte assumption holds it cost about one point of hit rate and doubled p99, so it is not
the default.
