# prequal-epp vs llm-d's EPP on llm-d's own simulator and benchmark

GPU-free head-to-head in kind, following llm-d-router's nightly perf job (`test/perf/config/shared_prefix_job1.yaml`)
with the standalone chart (Envoy sidecar in the EPP pod, no Gateway). Only the EPP image and its flags differ between
arms. Results: [docs/benchmarks.md](../../../docs/benchmarks.md); data: [results/kind](../../../results/README.md#kind).

## Drivers

[`tools/kind-llmd.sh`](../../../tools/kind-llmd.sh) needs docker, kind, kubectl, helm and jq. Reports go to
`$BENCH/reports` (default `~/bench`), mounted into the kind node at `/reports`.

```sh
tools/kind-llmd.sh setup                    # cluster `bench`, GIE CRDs, metrics-server, images, simulators
tools/kind-llmd.sh suite loaded 2           # alternating rounds: prequal r1, llmd r1, prequal r2, llmd r2
tools/kind-llmd.sh run prequal my-label zipf   # one run of one arm
ARMS="prequal-lean prequal" TAG=ab tools/kind-llmd.sh suite loaded 2   # lean mode against the default
tools/kind-llmd.sh router-image target/release/prequal-router && tools/kind-llmd.sh run router my-label loaded
tools/kind-llmd.sh results 'prequal-loaded-*'  # one JSON line per run and stage, plus hit rate and peak CPU/memory
tools/kind-llmd.sh teardown
```

Timed runs hold `~/bench.lock`, so other benchmarks on the same host can take turns.

`LLMD_VERSION` selects the chart and llm-d EPP release (default `v0.11.0`; `LLMD_VERSION=v0.10.0` reproduces the
earlier results). `setup` downloads that release's `optimized-baseline.yaml` (llm-d's nightly router values); v0.11
raised Envoy's `--concurrency` from 8 to 32 there. Give each version its own `BENCH` directory so results don't mix.

[`tools/kind-cache.sh`](../../../tools/kind-cache.sh) runs the cache-pressure workload on the same harness (it sources
`kind-llmd.sh`), with bounded simulator KV caches and a load sweep past the fleet's capacity:

```sh
BENCH=~/bench-cache tools/kind-cache.sh setup        # also pins the kind node to $CPUS (default 0-4,6-10)
ARMS="prequal llmd llmd-precise" tools/kind-cache.sh suite cache 2
ARMS="prequal llmd" tools/kind-cache.sh suite cache-two-epp 1
tools/kind-cache.sh epp-check                        # llmd-precise: shows the picker consuming KV events
tools/kind-cache.sh host-cpu >> ~/bench-cache/reports/host-cpu.log &   # CPU used by other jobs on the host
node tools/cache-report.mjs ~/bench-cache/reports '^(prequal|llmd)-cache-r.$' --md
tools/kind-cache.sh teardown
```

`KV_BLOCKS` (6000 per simulator), `MAX_SEQS` (8) and `STAGES` override the workload.

## Files

| File | Role |
|---|---|
| `kind.yaml` | Single-node kind cluster; `REPORTS_DIR` is substituted by the driver |
| `sim.yaml` | 10 llm-d-inference-sim v0.10.2 replicas; `SIM_EXTRA_ARGS` substituted per workload |
| `common.yaml` | Chart values shared by all arms, on top of llm-d's `optimized-baseline.yaml` |
| `arm-prequal.yaml` | Picker image `prequal-epp` |
| `arm-prequal-lean.yaml` | Lean mode on top of `arm-prequal.yaml` (`--prefill-signal scrape`); the driver also sets Envoy's `response_body_mode: NONE` |
| `arm-prequal-gossip.yaml` | `arm-prequal.yaml` plus `--gossip-peers`; the driver applies `arm-prequal-gossip-extra.yaml`, the headless Service naming the pickers |
| `arm-llmd.yaml` | Picker image `llm-d-router-endpoint-picker`, tagged `LLMD_VERSION` by the driver |
| `arm-llmd-precise.yaml` | llm-d's nightly precise prefix-cache configuration (KV events), adapted to the simulators: tokenizing through their render endpoint and one central event listener |
| `arm-llmd-precise-ob.yaml` | The same precise index with the optimized-baseline scorers and weights |
| `arm-router.yaml` | `prequal-router` alone (`deploy/helm/prequal-router`) in place of Envoy + picker, under the chart's Service name |
| `job.yaml` | inference-perf v0.6.0 job; stages and data substituted per workload |
| `ip-stage-barrier.py` | Wraps inference-perf so concurrent load generators start each stage together |
| `sim-timeline.sh` | Pod script sampling the fleet's prefix-cache counters every 5 s |

## Workloads

`kind-llmd.sh` workloads use llm-d's shared-prefix data: 150 groups × 5 prompts, 9500-token system prompt, 500-token
question, 1000 output tokens.

| Workload | |
|---|---|
| `job1` | llm-d's nightly as-is: 1 QPS × 15 s, then 10 QPS × 300 s |
| `loaded` | Sims slow down as they fill (`--max-num-seqs 32 --time-factor-under-load 4`); 5 → 15 → 25 QPS |
| `zipf` | `loaded`, with one hot prefix group at 25% of the rate (a second generator) |
| `two-epp` | `loaded` with two active picker replicas |
| `burst` | `loaded` sims; 5 QPS, a 35 QPS spike for 20 s, twice |
| `cache`, `cache-two-epp` (`kind-cache.sh`) | Bounded KV caches; 300 groups in three popularity tiers (8 hot at 30% of requests, 72 warm at 40%, 220 cold at 30%); 6 QPS warm-up, then 4 → 16 QPS; one or two pickers |

Deviations from the nightly (state them with any result): the sims use `--force-dummy-tokenizer` instead of per-pod
vLLM render sidecars (10 × 8 GiB doesn't fit a 31 GB host), there is no zone spread (kind has no zones), and resource
requests are smaller.
