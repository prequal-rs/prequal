# Roadmap: engine-assisted prefix caching

Today prequal is router-only. It learns which prefixes each replica holds from its own placements, sizes that
model from the engines' Prometheus metrics, and never needs an engine change. The projects below use engine features
to go further. Each one is optional: when the engine feature is absent or switched off, prequal behaves exactly as it
does today.

Engine versions referenced: vLLM v0.30, SGLang v0.5.21, llm-d-router v0.11. Items marked *(to verify)* have not been
checked against the engine's source.

## Principles

- **Token-free first.** prequal hashes 256-byte blocks of the request body and never tokenizes. KV-cache events,
  SGLang's pin API and exact tier tracking all work in engine token IDs. Projects that need no tokenizer come first;
  a tokenizer bridge is a separate investment, built only when a measurement justifies it.
- **Gate on an oracle.** The virtual-time simulator's engines hold the true cache contents. Before building any
  engine integration, a simulator arm with perfect knowledge bounds the possible gain. If the oracle doesn't gain,
  the integration won't either.
- **Stay a drop-in for llm-d.** Where llm-d already wires an engine feature through request headers
  (`x-prefiller-host-port`, `x-kv-cache-source-host-port`), `prequal-epp` sets the same headers so it keeps working with
  llm-d's charts and routing sidecar. `prequal-router` has no sidecar and does that orchestration itself.
- **Where engine help pays.** A single router's own placements already explain nearly all engine state: with one
  picker, llm-d's precise (event-fed) index tied its approximate one in our cache-pressure benchmark. The gains are
  where a router can't see engine state: several routers sharing a fleet, router restarts, engines whose eviction
  isn't LRU, offloaded KV tiers, and traffic that bypasses the router.

## Projects

| # | Project | Effort | Needs | Depends on |
|---|---|---|---|---|
| 1 | Per-request cache-hit feedback | 1–1.5 weeks | Engine flags | — |
| 2 | Tier-aware prefix index | 1–1.5 weeks | KV offload on | — |
| 3 | Prefill/decode pair selection | 2–3 weeks (EPP), +2 weeks (router) | P/D deployment | — |
| 4 | KV-event hybrid index | 2-day gate, then about 2 weeks | `--kv-events-config` | 1 (decision metric) |
| 5 | Prefix pull from the holding replica | 1.5–2 weeks + GPU validation | vLLM P2P tier, offload on | 2 |
| 6 | Pin hints for agentic turns | 1.5–2 weeks after a tokenizer bridge | SGLang + HiCache | 4 (tokenizer bridge) |

Efforts include the simulator work. GPU validation is separate unless stated.

### 1. Per-request cache-hit feedback

**Why.** prequal corrects its index only through fleet-wide engine counters, once per scrape, mixed across routers.
Engines can report each request's cached tokens. That gives ground truth per request, with no tokenizer.

**Engine side.**
- vLLM: `--enable-prompt-tokens-details` adds `usage.prompt_tokens_details.cached_tokens`. Streamed responses carry
  it only in the final usage chunk, so also `--enable-force-include-usage` (or the client's
  `stream_options.include_usage`). Cache-hit counts under P/D were wrong before vLLM PR #54222.
- SGLang: `--enable-cache-report`. A miss omits `prompt_tokens_details` entirely rather than reporting 0.
- Optional residency metrics: vLLM `--kv-cache-metrics` exports `vllm:kv_block_idle_before_evict_seconds`, which is
  the eviction horizon an approximate index needs. SGLang exports `sglang:evicted_tokens_total` and related gauges.

**Design.**
- A response scanner (new `prequal-llm` module) reads `cached_tokens` and `prompt_tokens` from the last chunk, the same
  way the prompt scanner reads request bodies.
- On response end, the ticket reports observed against predicted cached tokens to the replica.
- **Over-prediction** (prequal expected blocks the engine had evicted) gives a per-replica eviction age. Index
  entries older than it expire, which keeps the index right when engine eviction isn't LRU (SGLang `lfu`, `tlru`,
  `priority`).
- **Under-prediction** (the engine had more than expected) signals other routers warming the same fleet.
- `prompt_tokens` per request calibrates bytes per token. This fixes SGLang replicas, which today never calibrate
  because SGLang has no prefix-query counter.
- Metric `prequal_prefix_prediction{outcome="hit|over|under"}`. This is the measurement that decides whether
  projects 2 and 4 pay on a given fleet.
- Flags: `--hit-feedback auto|off`, `--engine-cache-report on|off`. Opt-in `--inject-include-usage` for operators who
  accept the extra streamed chunk; client requests are not rewritten by default.

**Degrades to** today's counter-based behaviour when the field is absent. Off in lean mode, where response bodies
don't reach the picker.

**Benchmark.** Simulator engines report true hits. Scenarios: `cache`, `cache-two-routers`, an engine with `lfu`
eviction. In kind, llm-d-inference-sim must fill `cached_tokens` *(to verify; may need a small upstream patch)*.

**Risk.** Small hit-rate gain where calibration already works. The main value is robustness on non-LRU engines and
SGLang, plus the measurement.

### 2. Tier-aware prefix index

**Why.** With KV offload on, engines hold two to five times their GPU cache in CPU memory. prequal sizes each
replica's index from GPU blocks only, so it forgets prefixes the CPU tier still holds and sends them elsewhere as
cold prompts. A CPU-tier hit costs little more than a GPU hit (PCIe reload against recompute: worth about 0.9–0.95
of a GPU hit for 8B–70B models), so this matters wherever offload is on. llm-d's tiered-cache guide measured HBM+CPU
cutting mean TTFT from 8.08 s to 0.41 s against HBM only.

**Engine side.**
- vLLM: `--kv-offloading-size <GiB>`, native backend by default since v0.15. Exports
  `vllm:external_prefix_cache_{queries,hits}` but no CPU-tier occupancy metric.
- LMCache: CPU, disk and remote tiers; `lmcache:*` metrics.
- SGLang HiCache: `--enable-hierarchical-cache`. The only engine exporting host-tier occupancy
  (`sglang:hicache_host_used_tokens`, `sglang:hicache_host_total_tokens`).

**Design.**
- The index gains an optional second LRU tier: blocks evicted from the GPU tier demote to it. A match returns GPU and
  CPU blocks separately.
- CPU capacity comes from SGLang's host-tier metric, or from `--offload-tokens-per-replica` for vLLM. The CPU tier
  calibrates from `external_prefix_cache_hits` the way the GPU tier calibrates today.
- Scoring weights CPU-tier blocks with `--tier-weight cpu=0.9,storage=0.4`.
- Accept llm-d's tiered `EndpointPickerConfig` when the chart passes it.

**Degrades to** today's behaviour when no offload is configured (tier capacity 0).

**Benchmark.** A CPU tier in the simulator's engine model (demote on eviction, configurable reload cost). Scenario
`cache-tiered`: working set twice the GPU cache, half of GPU+CPU. llm-d-inference-sim models no tiers, so kind
needs an upstream sim change or is skipped. GPU: vLLM with offload against llm-d's tiered guide workload.

**Risk.** Engines differ in when they offload (vLLM writes at store time, not on eviction), and vLLM's offload
capacity isn't observable from metrics.

### 3. Prefill/decode pair selection

**Why.** prequal can't run in llm-d's disaggregated prefill/decode setup at all today. prequal's existing signals
map directly onto it: pending prefill tokens for the prefill pool, KV headroom and in-flight requests for the decode
pool.

**What a picker must do.**
1. Pick the decode replica first; it holds the KV for the whole generation.
2. Compute the uncached part of the prompt on that replica and decide whether to disaggregate.
3. If so, pick the prefill replica by prefix affinity and prefill token load.
4. Route to the decode replica and pass the prefill replica in `x-prefiller-host-port`.

**Design.**
- `prequal-llm`: a P/D scheduler holding two schedulers (prefill pool, decode pool), each with its own index and
  policy weights. The prompt is recorded in both indexes, since the decode replica receives the KV.
- Disaggregate when the uncached tokens on the decode replica and the prompt length pass thresholds, matching llm-d's
  `prefix-based-pd-decider` semantics and defaults. A cost-based decision can follow.
- `prequal-epp`: read the role label from pods (exact label key *to verify*), accept llm-d's
  `disagg-profile-handler` configuration from the chart, set `x-prefiller-host-port`. Later: `Prefer: if-available`.
- `prequal-router`: orchestrate without a sidecar (vLLM's two-step prefill then decode with `kv_transfer_params`;
  SGLang's bootstrap injection). Done after the EPP.
- Flags: `--pd off|labels`, `--pd-role-label`, `--pd-non-cached-tokens 512`, `--pd-prompt-tokens 1024`.

**Degrades to** a single pool when pods carry no role label, and to local prefill when the prefill pool is empty.

**Benchmark.** Prefill-only and decode-only engine kinds with a transfer delay in the simulator; long (10k:1k) and
short (200:200) prompts to show the threshold's value. Arms: prequal, a reimplementation of llm-d's P/D scorers,
SGLang's gateway. In kind: llm-d's P/D guide with simulators that model transfer latency.

**Risk.** The llm-d multi-profile contract is still moving. prequal's gain here is "not worse, plus our load
handling", because the engine does the heavy lifting.

### 4. KV-event hybrid index

**Why.** Events give every router the fleet's real cache contents. They fix the cases a router can't see: several
routers (our two-picker benchmark hit rate trails one picker's), restarts (an empty index warms in seconds from the
engine's replay buffer), non-LRU eviction and out-of-band traffic.

**Gate first (2 days).** Add an oracle-index arm to the simulator with configurable delay and visibility. Compare
prequal with and without it on `cache`, `cache-two-routers`, a router-restart scenario and an `lfu` engine. Build the
integration only if the oracle gains at least 3 hit-rate points or 10% at TTFT p90 in some scenario.

**Gate result (2026-10-04).** Met only with several routers: no gain with one, 3–24 hit-rate points with two to four.
Routers telling each other their placements reach the same hit rate with no engine feature, so that comes first and
this project waits on a case it can't cover (traffic bypassing the routers, non-LRU eviction). The lfu-engine scenario
was not run. See [peer-index](peer-index.md).

**Engine side.** vLLM and SGLang `--kv-events-config`: ZMQ PUB with `BlockStored`, `BlockRemoved`,
`AllBlocksCleared`, a replay endpoint, and a `medium` field for tiers. vLLM switched event encoding from arrays to
maps in v0.24, so a parser must handle both.

**Design.**
- Behind a cargo feature `kv-events`, off by default: a wire decoder, a subscriber per pod with sequence-gap detection
  and replay, and a bridge from engine blocks to prequal's byte blocks.
- Bridges, cheapest first:
  - **Eviction rate only:** counts stores and removals per replica, for capacity and TTL calibration.
  - **Correlation:** a per-request key the engine echoes in `BlockStored` (`session_id`, *to verify*) maps that
    request's engine blocks onto its byte-block chain. Exact for this router's own requests.
  - **Tokenize:** via the engine's render endpoint or in-process with the `tokenizers` crate and chat templates.
    Exact for all routers' blocks; built only if the gate shows headroom the correlation bridge can't reach.
- The replica keeps an optional exact index beside the approximate one; a match takes the larger of the two. The
  policy is unchanged.
- Flags: `--kv-events off|bind:ADDR|per-pod:PORT`, `--kv-events-replay-port`. Accept llm-d's `kvEventsConfig` shape.
  With more than one picker replica, a central bind sees only part of the fleet, so per-pod mode is required.

**Degrades to** the approximate index when events are missing, gapped beyond replay, or stale.

**Risk.** Wire-format churn, a ZMQ dependency, and a small gain outside multi-router setups.

### 5. Prefix pull from the holding replica

**Why.** prequal's hardest trade-off is affinity against load: a replica holding a hot prefix gets overloaded. If
the chosen replica can pull the prefix's KV from its holder, spreading a hot prefix costs a transfer instead of a
recompute. Pulling beats recomputing above roughly 10–16 Gbps for 8B–70B models, so RDMA clears it by a wide margin.

**Engine side.** vLLM's offloading connector P2P tier: per request,
`kv_transfer_params.remote_kv_source = {kv_request_id, remote_host, remote_port}`. It serves CPU-tier blocks only,
needs identical block size and hash seed across peers, and is new (mid-2026). llm-d's `p2p-source-producer` sets
`x-kv-cache-source-host-port`, and its routing sidecar turns that into `remote_kv_source`.

**Design.**
- Candidates gain the best pullable match from other replicas (CPU-tier blocks, so this needs project 2).
- The policy credits pullable blocks by a pull efficiency computed from configured bandwidth, KV bytes per token and
  prefill rate.
- The ticket records a source, sampled among near-best holders by queue depth so pulls don't all hit one source.
- `prequal-epp` sets `x-kv-cache-source-host-port`; `prequal-router` injects `remote_kv_source` into the body.
- Flags: `--kv-pull off|header|body`, `--kv-pull-min-tokens 1024`, `--kv-pull-gbps`, `--kv-bytes-per-token`,
  `--p2p-port`.

**Gate.** A pull path in the simulator's engine model at 25, 100 and 400 Gbps. Build only if pulls beat plain
prequal by 10% at p90 or capacity on hot-prefix, two-router or overload scenarios.

**Risk.** The engine feature is new and already renamed its keys once. Wrong cost-model defaults could make it worse
than off. No published measurement exists for a picker choosing pull against local prefill.

### 6. Pin hints for agentic turns

**Why.** In agent loops, a turn's context comes back after a tool call. Pinning it for the expected tool duration
keeps it from being evicted in between; Continuum reports 1.1–3.7x lower job completion time.

**Engine side.** SGLang `POST /hicache/pin_prefix` with token IDs and a TTL, refreshed on hit; needs HiCache and a
non-zero pinned ratio. vLLM's retention API is an open RFC (#37003).

**Design.** When a response ends in a tool call (or the client sends a cache-control hint), send a fire-and-forget
pin to the replica that served it, with a TTL from tool-duration history. Pinned prefixes are exempt from the index's
eviction-age expiry.

**Blocked on** token IDs (the tokenizer bridge from project 4). SGLang-only until vLLM ships its API.

## Router-only work alongside

These need no engine features:
- **Placement gossip between routers.** In simulation it recovers the hit rate several routers lose
  ([peer-index](peer-index.md)). `prequal-epp --gossip-peers` implements it; still to do: a kind benchmark with two
  pickers, and the same flag on `prequal-router`.
- **Benchmark arms:** SGLang's Rust router (event-aware) and llm-d's "sticky until saturated" configuration.
- **A shared-system-prompt workload in kind.** Keying prompts past the prefix every replica holds is measured only
  on production-trace replays in the simulator so far.

Already measured and dropped, router-only: eviction-cost placement (after Preble), steering one-off prompts to a
scratch subset of replicas, bounded replication of hot prefixes, and a token-weighted queue. With one router,
prequal already sends a prompt to a replica holding it about 99% of the time and matches a single pooled cache's hit
rate, so placement alone has little left to gain; see the [`Prequal`](../crates/prequal-llm/src/policy/prequal.rs)
documentation.

## Not planned

- **Live request migration** (Llumnix): no upstream support in vLLM or SGLang.
- **Non-prefix chunk reuse** (CacheBlend, EPIC): LMCache-only on the engine side, and approximate blending costs
  accuracy.
- **A tokenizer bridge ahead of measurement:** built only if project 4's gate shows it pays.
