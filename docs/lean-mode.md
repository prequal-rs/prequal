# Lean mode

By default a Gateway API Inference Extension gateway streams every response chunk through the endpoint picker over
`ext_proc`. With llm-d's chart that is about 1,000 gRPC messages per request, nearly all of the picker's CPU and a
large share of Envoy's. `prequal-epp` only needs the response headers and the end of the stream, so Envoy can skip
response bodies entirely. That is lean mode: 6–8 messages per request instead of ~1,006.

## Configuration

**Envoy.** In the `ext_proc` filter that calls the picker:

```yaml
processing_mode:
  request_header_mode: SEND
  response_header_mode: SEND               # keep: served endpoint, error status, conformance
  request_body_mode: FULL_DUPLEX_STREAMED  # keep: the picker reads the prompt
  response_body_mode: NONE                 # was FULL_DUPLEX_STREAMED
  request_trailer_mode: SEND
  response_trailer_mode: SKIP
```

**prequal-epp.** Without response bodies the picker no longer sees the first token, which is when it normally releases
a request's prefill reservation on its replica. Tell it what to use instead with `--prefill-signal`:

| Engine | Flag | Why |
|---|---|---|
| vLLM | `--prefill-signal scrape` | vLLM sends response headers when it accepts the request, before queueing and prefill. `scrape` holds the reservation until the replica's next metrics scrape, which then counts the request in its own queue. |
| SGLang, llm-d-inference-sim | `--prefill-signal headers` | These send headers with the first token, so `headers` is exactly the default signal. |

The default, `first-chunk`, needs response bodies. `estimate:<prefill tokens/s>` and `end` also work without bodies,
but `estimate` is sensitive to the rate you give it and `end` overloads hot replicas, so neither is recommended.

End of request needs nothing extra: Envoy closes the `ext_proc` stream when the request finishes (`CANCELLED` in
Envoy 1.33, a clean close in 1.34+ with `envoy.reloadable_features.ext_proc_graceful_grpc_close`), and
`prequal-epp` treats either as the end.

### llm-d's `llm-d-router-standalone` chart

The chart embeds Envoy's config as a string in its values (`router.proxy.presets.envoy.configMap.data."envoy.yaml"`),
so take a copy of the chart's values, change the `ext_proc` filter's `processing_mode` as above, and pass the edited
file alongside the picker image and flag:

```sh
helm show values oci://ghcr.io/llm-d/charts/llm-d-router-standalone --version v0.11.0 > lean-values.yaml
# edit lean-values.yaml: response_body_mode: NONE, response_trailer_mode: SKIP
helm install router oci://ghcr.io/llm-d/charts/llm-d-router-standalone --version v0.11.0 -f lean-values.yaml \
  --set router.epp.image.registry=ghcr.io/prequal-rs \
  --set router.epp.image.repository=prequal-epp --set router.epp.image.tag=0.1.0 \
  --set router.epp.flags.prefill-signal=scrape
```

### Other gateways

Gateways that generate the `ext_proc` filter themselves (for an `InferencePool` route) configure full-duplex response
streaming. Lean mode needs that one field overridden, which is gateway-specific; `prequal-epp` needs no other change.

## What it costs and saves

**CPU.** In the [data-plane harness](benchmarks.md#3-data-plane-cost-harness), the picker's CPU per request falls
from ~20 ms to ~2 ms, and Envoy plus picker from 84 ms to 47 ms. Adding Envoy `--concurrency 2` and plaintext
`ext_proc` brings the pair to 28.5 ms. TTFT was unchanged.

End to end in kind, on llm-d v0.11's chart and simulator with the [`loaded` workload](benchmarks.md#1-llm-d-v011-in-kind)
at 25 QPS, lean mode cut the picker's peak CPU from 0.65 to 0.06 cores and Envoy's from 2.7 to 1.7 cores: 1.7 cores
for the pair, against 3.4 for default prequal-epp and 4.1 for Envoy plus llm-d's picker. TTFT was better at 15 and
25 QPS (25 QPS p50 / p90 / p99 30 / 52–68 / 144–155 ms against 43–44 / 130–136 / 245–266 for the default). At 5 QPS
on a cold fleet, p90 and p99 overlapped and lean's p50 was 35–45 ms against 28–41 ms, over two rounds.

**Routing precision.** Losing the exact first-token time matters only where many requests share a warm prefix. In the
virtual-time simulator ([`results/vsim-lean`](../results/vsim-lean/scorecard.md), 8 seeds), lean mode with
`scrape` against the default:

| Scenario | p99 TTFT, lean vs default |
|---|---|
| burst | 1.3 s vs 1.1 s |
| zipf, 25 QPS | 495 ms vs 357 ms |
| mixed, 10 QPS | 354 ms vs 297 ms |
| canonical, unique, two-routers, other stages | within ±10% |

Against `llmd-optimized` (a model of llm-d's deployed scorer set, which streams every body) lean mode still scored
16 wins and 1 loss per stage, including burst (1.3 s vs 1.8 s) and zipf at 25 QPS (495 ms vs 4.1 s). These
simulation results predate the cache-pressure changes in [benchmarks](benchmarks.md#2-cache-pressure).

llm-d's own picker can't run this way at the same cost: it uses response bodies for its metrics, usage accounting,
response-side model-name rewrite and latency-predictor training, and llm-d documents full-duplex streaming as the
only supported mode.

**Not yet measured:** lean mode end to end in kind on `burst` and `zipf`, the workloads where the simulator shows its
cost.
