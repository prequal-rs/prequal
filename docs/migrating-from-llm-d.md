# Migrating from llm-d's endpoint picker

`prequal-epp` replaces the image of llm-d's endpoint picker (EPP) and nothing else. The gateway, `InferencePool`,
model servers and clients stay as they are, and switching back is one `helm rollback`.

## Check what you would lose

`prequal-epp` routes with its own scorer and does not implement every llm-d component. Before switching, check that
you don't depend on any of these:

| llm-d feature | In prequal-epp |
|---|---|
| Scorer plugins and weights from `--config-file` / `--config-text` | Accepted but not applied; routing is set by `--policy`. A warning is logged at startup |
| Flow control and fairness queuing | None. The fairness ID header is only a metric label |
| Latency predictor | None |
| Precise prefix-cache scoring from KV events | None. Prefix affinity is learned from the picker's own placements ([roadmap](roadmap.md)) |
| Model rewrite (`target_model_name` ≠ `model_name`) | None |
| Leader election (`--ha-enable-leader-election`) | Accepted but ignored: every replica serves (active-active) |
| Istio as the gateway | Not supported ([conformance](conformance.md#istio)). llm-d's standalone Envoy and agentgateway work |

These carry over unchanged: `InferencePool` v1, `InferenceObjective` priorities and sheddable requests (429 under
saturation), llm-d's error responses, and llm-d's EPP metric names, labels and buckets. Existing dashboards keep
working. Series for components `prequal-epp` doesn't have (flow control, latency predictor, KV events) are absent.
Details are in [configuration.md](configuration.md#prequal-epp).

## Switch

With llm-d's `llm-d-router-standalone` chart (v0.10 or v0.11), change the image and keep your other values:

```sh
helm upgrade router oci://ghcr.io/llm-d/charts/llm-d-router-standalone --version v0.11.0 --reuse-values \
  --set router.epp.image.registry=ghcr.io/prequal-rs \
  --set router.epp.image.repository=prequal-epp \
  --set router.epp.image.tag=0.1.0
```

`prequal-epp` accepts the flags the chart passes, so nothing else changes. Set `--engine sglang` with
`--set router.epp.flags.engine=sglang` if your model servers are SGLang. For another gateway, see
[deploy/README.md](../deploy/README.md#prequal-epp-with-another-gateway).

## Verify

1. **The pod is ready.** Readiness turns on once the pool's endpoints are known.
2. **Startup warnings.** Check `kubectl logs` for warnings about flags whose values `prequal-epp` doesn't act on,
   such as a scorer config that is no longer applied.
3. **Requests are routed.** `prequal_epp_picks_total{result="routed"}` rises with traffic, and
   `llm_d_epp_ready_endpoints` matches the number of model-server pods.
4. **Latency.** Compare TTFT on your existing dashboards. Prefix affinity is learned from traffic, so the new picker
   starts with an empty prefix model: compare once the usual prompts have been seen.

## Roll back

```sh
helm rollback router
```

This restores the previous release, including llm-d's image. `prequal-epp` keeps no state outside the pod, so there
is nothing to clean up.

## Afterwards

- **Lean mode.** With Envoy's `response_body_mode: NONE`, `prequal-epp` uses about a tenth of the picker CPU. llm-d's
  picker can't run this way. See [lean-mode.md](lean-mode.md).
- **Replicas.** Each picker replica keeps its own prefix-cache model, so one replica gets the best hit rate. Run more
  for availability, not throughput: one replica handled every benchmark load, up to 35 QPS.
