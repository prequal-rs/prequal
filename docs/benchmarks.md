# Benchmarks

All results come from one shared 12-thread Linux host, with no GPUs. Treat absolute numbers as indicative; the
comparisons are like for like: arms alternate, every run starts from cold caches, and each run logs the host's load.
Every table names the llm-d release, the number of rounds and anything that differed. Raw data:
[`results/`](../results/README.md).

1. [llm-d v0.11 in kind](#1-llm-d-v011-in-kind): llm-d's chart, simulator and load generator, only the picker swapped.
2. [Cache pressure](#2-cache-pressure): simulator KV caches small enough to evict, where routing decides GPU cost.
3. [Data-plane cost](#3-data-plane-cost-harness): Envoy and picker CPU per request.
4. [Virtual-time simulator](#4-virtual-time-simulator): every policy, many seeds, production traces.
5. [Earlier results on llm-d v0.10](#5-earlier-results-llm-d-v010).

## 1. llm-d v0.11 in kind

**Setup.** llm-d's [`llm-d-router-standalone`][chart] chart v0.11.0 with its nightly `optimized-baseline` values
(queue, KV-utilization, approximate-prefix and no-hit-LRU scorers; Envoy as a sidecar of the picker pod). The llm-d
arm is that release's endpoint picker with those values' flags, including verbose logging (`--v=4`) as llm-d's
nightly runs it; the prequal arm changes only the picker image. Model
servers are 10 [llm-d-inference-sim][sim] v0.10.2 replicas and the load generator is [inference-perf][perf] v0.6.0.
Deviations from llm-d's nightly: the simulators use a dummy tokenizer instead of per-pod render sidecars, there is no
zone spread, and resource requests are smaller. Driver: [`tools/kind-llmd.sh`](../tools/kind-llmd.sh), see
[`deploy/bench/llmd`](../deploy/bench/llmd/README.md).

**Build.** prequal-epp as of 2026-10-02, before the [cache-pressure](#2-cache-pressure) changes. Those changes leave
routing on these workloads unchanged (the simulators' caches are unbounded and every prefix group starts
differently), except the two-picker hash weight, which acts only in `two-epp`. A one-round check of `loaded` on the
final build is [below](#final-build-check).

**Workloads.** 150 prefix groups, ~10k-token prompts, 1,000 output tokens.

| Name | What it adds |
|---|---|
| `job1` | llm-d's nightly unchanged: 1 QPS for 15 s, then 10 QPS for 300 s |
| `loaded` | Simulators slow down as they fill (`--max-num-seqs 32 --time-factor-under-load 4`); 5 → 15 → 25 QPS |
| `zipf` | `loaded`, plus one hot prefix group taking 25% of the rate (a second load generator) |
| `two-epp` | `loaded` with two picker replicas sharing the fleet, both active in both arms |
| `burst` | 5 QPS, then a 35 QPS spike for 20 s, twice |

TTFT is p50 / p90 / p99 in ms, as ranges over two alternating rounds per arm. No run had a failed request.

| `loaded` | prequal-epp | llm-d v0.11 |
|---|---|---|
| 5 QPS | 28–41 / 55–61 / 72–108 | 32–34 / 59–63 / 92–123 |
| 15 QPS | **24 / 30–31 / 115–116** | 42–43 / 69–71 / 151–164 |
| 25 QPS | **43–44 / 130–136 / 245–266** | 100–106 / 212–232 / 369–409 |

| `two-epp` | prequal-epp | llm-d v0.11 |
|---|---|---|
| 5 QPS | **27–30 / 56–57 / 88–93** | 56 / 62–66 / 96–145 |
| 15 QPS | **25 / 35 / 118–120** | 60 / 123–130 / 215–261 |
| 25 QPS | **64–65 / 194–206 / 411–448** | 401–443 / 917–1,021 / 1,503–1,676 |

Prefix-cache hit rate over the run: 97.3% with prequal, 94.6–94.8% with llm-d (97.5% for both with one picker). The
chart's two-replica mode turns on leader election, so only one llm-d picker would serve; both arms instead run two
active replicas.

| `burst` | prequal-epp | llm-d v0.11 |
|---|---|---|
| 5 QPS | 47 / 55–56 / 100–109 | 54 / 60–65 / 116–131 |
| 35 QPS spike 1 | **91–92 / 239–294 / 422–551** | 213–230 / 415–456 / 616–736 |
| 5 QPS | 20 / 26–27 / 42–81 | 26–27 / 29 / 37–102 |
| 35 QPS spike 2 | **77–83 / 224–273 / 422–515** | 238–245 / 453–481 / 668–753 |

inference-perf drains between stages, so each spike lands on an idle fleet with warm caches.

| `job1` (llm-d's nightly) | prequal-epp | llm-d v0.11 |
|---|---|---|
| 1 QPS (15 requests) | 62–65 / 96–97 / 100–107 | 63–65 / 75–97 / 88–105 |
| 10 QPS | **22 / 25–26** / 96–100 | 29 / 45–46 / 117–122 |

At 1 QPS the arms tie (with 15 requests, p99 is the single slowest one). At 10 QPS p99 overlaps within noise.
Hit rate was 93.9% in both arms.

| `zipf`, 25 QPS | prequal-epp | llm-d v0.11 |
|---|---|---|
| The other 149 groups | **46 / 110–133 / 257–289** | 115–116 / 255–280 / 561–659 |
| Hot group only | **43–45 / 78–82 / 142–147** | 114–116 / 224–245 / 892–1,161 |

Treat `zipf` per stage with care: its two load generators start each stage when their own data generation finishes,
so stage boundaries drift apart (fixed later for the cache workload by a stage barrier). Whole-run hit rate was
97.4% in both arms. llm-d v0.10 kept the hot prefix on one replica until it collapsed; v0.11 no longer does.

**Resource use** (peak, picker container, summed across replicas):

| Workload | prequal-epp | llm-d v0.11 |
|---|---|---|
| `loaded` | 0.64–0.66 cores / 21–32 MiB | 1.89–1.90 cores / 159–178 MiB |
| `zipf` | 0.64–0.65 cores / 22–23 MiB | 1.92–1.93 cores / 187–204 MiB |
| `two-epp` | 0.84–0.85 cores / 37 MiB | 2.36–2.37 cores / 1,021–1,167 MiB |
| `burst` | 0.55–0.63 cores / 28–38 MiB | 1.78–1.79 cores / 304–370 MiB |
| `job1` | 0.34 cores / 15–16 MiB | 1.63–1.69 cores / 105 MiB |

The chart's Envoy sidecar peaked at 2.4–3.1 cores in front of prequal-epp and 2.1–2.5 in front of llm-d's picker
from 15 QPS up (v0.11 raised Envoy's `--concurrency` from 8 to 32), and at 1.3 against 1.6–1.7 cores on `job1`.
[Lean mode](lean-mode.md) removes most of Envoy's work for the picker.

**Lean mode** (`loaded`, same suite; Envoy `response_body_mode: NONE`, `prequal-epp --prefill-signal scrape`):

| | prequal-epp, lean | prequal-epp, default | llm-d v0.11 |
|---|---|---|---|
| 5 QPS | 35–45 / 55–60 / 85–103 | 28–41 / 55–61 / 72–108 | 32–34 / 59–63 / 92–123 |
| 15 QPS | **23 / 26 / 106–108** | 24 / 30–31 / 115–116 | 42–43 / 69–71 / 151–164 |
| 25 QPS | **30 / 52–68 / 144–155** | 43–44 / 130–136 / 245–266 | 100–106 / 212–232 / 369–409 |
| Picker CPU, peak | **0.06 cores** | 0.64–0.66 cores | 1.89–1.90 cores |
| Envoy CPU, peak | **1.67–1.70 cores** | 2.70–2.77 cores | 2.19–2.21 cores |
| Picker + Envoy | **1.73–1.76 cores** | 3.35–3.43 cores | 4.07–4.11 cores |

**Drop-in check.** Under 10 QPS of `job1`'s traffic (1,500 requests), the picker pod was deleted once per arm.
- prequal-epp started on the chart's v0.11 flags without changes and exported its `llm_d_epp_*` series. The chart's
  `/bin/sh -c "sleep 5"` preStop hook ran (the image ships a sleep-only `/bin/sh`, see
  [configuration](configuration.md#lifecycle-and-replicas)), the picker drained and exited after 5 s, and a
  replacement was ready 9.8 s after the delete. 35 of 1,500 requests failed.
- llm-d's picker image has no shell, so the same hook failed (`FailedPreStopHook`) and the pod ran out its 30 s grace
  period. Its replacement was ready after 35 s, and 132 of 1,500 requests failed.

### Final-build check

One round each on the final build, `loaded`, with the kind node pinned to 10 of the host's 12 threads (so both arms
run slower than above): at 25 QPS prequal-epp 85 / 262 / 550 ms against llm-d's 191 / 399 / 659 ms, picker peak 0.58
against 1.69 cores; at 15 QPS 26 / 35 / 117 against 48 / 102 / 210. Hit rate 97.5% in both arms.

## 2. Cache pressure

The workloads above never fill a simulator's KV cache, so every prefix-aware picker reaches the same hit rate. In
production, prompt prefixes outgrow the fleet's cache, and where a request lands decides what gets evicted and how
much prompt work is recomputed: that is the GPU cost. This workload makes the simulators evict.

**Setup.** Same stack and chart as section 1. Each simulator holds 6,000 blocks of 16 tokens (`--kv-cache-size
6000`; 60k blocks across the fleet) and runs 8 requests at once (`--max-num-seqs 8`). Prefix popularity is tiered,
with three load generators: 8 hot groups take 30% of requests, 72 warm groups 40% and 220 cold groups 30%. The
prefixes total 2.2–2.8 times the fleet's cache. A barrier starts every generator's stage at the same second. Stages:
a 6 QPS warm-up, then 4 to 16 QPS in steps of 2, 120 s each. The latency target, fixed before the runs: TTFT p90 at or
below 500 ms with at most 1% failures; capacity is the highest rate at which every stage up to it meets the target.
No run had a failure. "Recomputed" is prompt tokens recomputed per request, out of about 10,000. Driver:
[`tools/kind-cache.sh`](../tools/kind-cache.sh); report: [`tools/cache-report.mjs`](../tools/cache-report.mjs).

**Eviction is real.** With bounded caches llm-d's picker hits 0.615 of prompt blocks, against 0.909 for the same
run with unbounded ones. An event-fed index in llm-d's precise configuration (below) holds about 60k blocks, the
fleet's capacity. A miss that forces an eviction costs the simulator 115–170 ms against 14–20 ms for a hit; most of
that is the simulator's eviction scan, not modelled prefill.

**Final build, one picker** (two rounds each, means; TTFT in ms):

| QPS | prequal-epp: hit / recomputed / p50 / p90 / p99 | llm-d v0.11 |
|---|---|---|
| 4 | 0.621 / 3,784 / 32 / 175 / 193 | 0.627 / 3,734 / 38 / 199 / 217 |
| 8 | 0.639 / 3,608 / 34 / 198 / 232 | 0.646 / 3,539 / 43 / 216 / 285 |
| 10 | 0.633 / 3,667 / 37 / 204 / 241 | 0.649 / 3,509 / 60 / 248 / 563 |
| 12 | 0.633 / 3,672 / 45 / **219** / 308 | 0.631 / 3,696 / 131 / **638** / 2,702 |
| 14 | 0.631 / 3,685 / 80 / **283** / 1,446 | 0.607 / 3,933 / 354 / **2,032** / 3,567 |
| 16 (overloaded) | 0.632 / 3,677 / 734 / 2,663 / 4,181 | 0.518 / 4,816 / 8,924 / 13,528 / 15,709 |
| **Max QPS at 500 ms** | **14, 14** | 10, 12 |
| Max QPS at 300 ms | 14, 14 | 10, 10 |
| Run hit rate | **0.625** | 0.604 |
| Picker CPU mean / peak, memory | **0.28 / 0.50 cores, 30 MiB** | 1.20 / 1.55 cores, 367 MiB |

Up to 10 QPS llm-d hits 0.6–1.6 points more and recomputes slightly fewer tokens; from 12 QPS its hit rate falls as
load spreads requests, while prequal-epp's holds. prequal-epp is faster at every rate.

**Final build, two pickers** (one round each):

| QPS | prequal-epp: hit / recomputed / p50 / p90 | llm-d v0.11 |
|---|---|---|
| 4 | 0.604 / 3,956 / 32 / 180 | 0.466 / 5,343 / 118 / 209 |
| 8 | 0.604 / 3,965 / 37 / 202 | 0.465 / 5,352 / 151 / 277 |
| 12 | 0.568 / 4,322 / 65 / 247 | 0.436 / 5,644 / 425 / 2,205 |
| **Max QPS at 500 ms** | **12** | 10 |
| Run hit rate | **0.563** | 0.406 |
| Picker CPU mean / peak, memory (both replicas) | **0.38 / 0.64 cores, 48 MiB** | 1.58 / 1.94 cores, 701 MiB |

With two pickers llm-d recomputes 27–36% more prompt tokens per request at every rate up to 12 QPS.

**How prequal-epp got there.** The first run of this workload exposed a loss: hit rate 0.40 against llm-d's 0.60,
and about 6,000 recomputed tokens per request against 3,600. prequal sized each replica's cache model assuming
4 bytes of request per engine token. These prompts run about 12 bytes per token, so it believed each replica held a
third of what it did, forgot warm prefixes the simulators still held, and placed them elsewhere. Each replica now
measures its bytes per token from the engine's prompt-token counter (`fleet.rs`); that raised the hit rate to 0.62.
A second change keys prompts past the prefix every replica holds and weights the shared hash more firmly with
several pickers (see the [simulator](#4-virtual-time-simulator)). Together they took the two-picker hit rate from
0.511 to 0.563.
Before/after data: [`results/kind/cache`](../results/kind/cache), [`cache-fix`](../results/kind/cache-fix).

**llm-d's precise prefix-cache routing** (run against the first prequal build; llm-d arms only, two rounds). llm-d
v0.11's nightly precise configuration feeds its index from the simulators' KV events (about 150k event messages per
run were consumed; a request 8 s after its twin still found all its blocks in the index). Run hit rate and capacity
at 500 ms / 300 ms:

| llm-d configuration | Run hit rate | Max QPS at 500 ms | at 300 ms | Picker CPU mean, memory |
|---|---|---|---|---|
| Optimized baseline (approximate index) | 0.599 | 12, 10 | 10, 10 | 1.19 cores, 395 MiB |
| Precise, llm-d's own weights | 0.419 | 12, 12 | 8, 8 | 1.30 cores, 421 MiB |
| Precise index, baseline weights | 0.590 | 12, 10 | 10, 10 | 1.26 cores, 406 MiB |

Exact cache state bought nothing here: with one picker, the approximate index's last placement is usually where the
prefix still is. Two changes were needed to run it on simulators (tokenizing through the simulators' render endpoint
and one central event listener); with that listener each of two pickers would see only part of the fleet, so the
precise arm was not run with two pickers.

**Caveats.** The simulator's prefill doesn't slow concurrent decodes, so capacity here is bound by decode slots and
favours spreading load; on GPUs, recomputed tokens cost more than they do here. The capacity margin is one or two
2-QPS steps. CPU use by other jobs on the host was sampled throughout the final runs and stayed at about zero.

## 3. Data-plane cost (harness)

[`tools/epp-load.sh`](../tools/epp-load.sh) drives a real Envoy 1.33, using llm-d v0.10's chart configuration verbatim,
in front of the picker. Fake model servers stream ~1,000 tokens per response at llm-d-inference-sim's pacing. At 25
QPS it agrees with kind to within about 10%.

| Envoy configuration | Envoy + picker CPU per request |
|---|---|
| Chart as shipped (`--concurrency 8`, response bodies streamed to the picker) | 84 ms |
| `--concurrency 2` | 49 ms (−41%) |
| `response_body_mode: NONE` (lean mode) | 47 ms (−44%) |
| `--concurrency 2`, no TLS to the picker, lean mode | 28.5 ms (−66%) |
| `prequal-router` alone, no Envoy | 17 ms |

TTFT was unchanged across these configurations.

## 4. Virtual-time simulator

`llm-bench --virtual` runs the real scheduler and policies against modelled vLLM engines on a virtual clock, so a
36-run comparison takes about a minute. Wins, losses and ties against `llmd-optimized` (a reimplementation of llm-d's
optimized-baseline scorers) are counted per load stage on p99 TTFT over 8 seeds; within ±10% is a tie.

| Scenario | W / L / T | Notes |
|---|---|---|
| burst | 1 / 0 / 0 | p99 1.1 s vs 1.8 s |
| zipf | 5 / 0 / 4 | 25 QPS: 282 ms vs 4.1 s |
| two-routers | 9 / 0 / 0 | hit rate 77–96% vs 67–71% |
| mixed | 2 / 1 / 6 | the loss: 15 QPS p99 108 ms vs 94 ms |
| canonical | 0 / 0 / 9 | both at ~100% hits |
| unique | 0 / 0 / 9 | no shared prefixes to exploit |
| cache | 4 / 0 / 3 | the cache-pressure workload above; hits tie at 66–69% |
| cache-two-routers | 3 / 0 / 4 | hit rate 59–68% vs 48–50% |

In the simulator `llmd-optimized` shares prequal's cache-model sizing, so on `cache` it gains from the same
calibration; llm-d itself keeps a fixed-size index (`--hide-kv-capacity` models that). Under a simulated host
slowdown of the 25 QPS stage (20 seeds), prequal and `llmd-optimized` both collapse 0/20 runs at 3.2× and 2/20 at
3.4×; llm-d's plain default scorer collapses 20/20.

**Production traces.** `llm-bench --trace` replays the public [Mooncake traces][mooncake] (Kimi, FAST '25,
Apache-2.0; fetch with [`tools/fetch-mooncake-traces.sh`](../tools/fetch-mooncake-traces.sh), not stored here) on
16 modelled H100 engines, whose total cache holds 2–5% of the traces' distinct prompt blocks. Hit rate and TTFT p50 /
p90 / p99 in ms, replayed at twice the recorded rate:

| Trace | prequal | llmd-optimized | llmd-guide | sglang-cache-aware |
|---|---|---|---|---|
| conversation | **19.5%, 351 / 1,635 / 5,044** | 19.1%, 2,451 / 7,727 / 13,895 | 18.5%, 364 / 1,618 / 5,230 | 20.1%, 460 / 2,149 / 6,180 |
| tool-agent | 46.8%, **137** / 1,212 / 4,334 | 47.1%, 547 / 5,117 / 11,234 | 46.5%, 162 / **940 / 3,571** | 47.2%, 383 / 1,657 / 5,564 |
| synthetic | 38.1%, 142 / 2,191 / 3,924 | 39.9%, 126 / 2,041 / 3,793 | 30.8%, 241 / 2,347 / 4,035 | 40.3%, 161 / 2,204 / 4,126 |

Both traces share a system prompt across all traffic. Keying prompts past the prefix every replica holds took
prequal from 10.3% to 19.5% (conversation) and 40.3% to 46.8% (tool-agent). prequal trails on `synthetic` by about
2 points, and llm-d's guide configuration has better tails on tool-agent. Router-only ideas that lost here,
including eviction-cost placement, admitting one-off prompts to a scratch subset and deduplicating hot prefixes, are
listed in the [`Prequal`](../crates/prequal-llm/src/policy/prequal.rs) documentation.

Reproduce ([`results/vsim-routercache`](../results/vsim-routercache) holds the outputs):

```sh
cargo build --release -p prequal-testbed
POLICIES="prequal llmd-optimized" SEEDS="1 2 3 4 5 6 7 8" tools/vsim-compete.sh out/vsim
node tools/llm-compare.mjs out/vsim out/vsim prequal llmd-optimized
SCENARIOS="cache cache-two-routers" POLICIES="prequal llmd-optimized" SEEDS="1 2 3 4 5 6 7 8" \
  tools/vsim-compete.sh out/cache
node tools/llm-compare.mjs out/cache out/cache prequal llmd-optimized
POLICIES="prequal llmd-optimized llmd-default" FACTORS="3.2 3.4" SEEDS="$(seq 1 20)" tools/vsim-collapse.sh
tools/fetch-mooncake-traces.sh
TRACE_SPEED=2 SCENARIOS="trace-conversation trace-toolagent trace-synthetic" SEEDS=1 \
  POLICIES="prequal llmd-optimized llmd-guide sglang-cache-aware" tools/vsim-compete.sh out/trace
```

## 5. Earlier results: llm-d v0.10

The same suite on llm-d's chart v0.10.0 (two rounds, an earlier prequal build; [`results/kind/main`](../results/kind/main)).
At 25 QPS on `loaded`, prequal-epp 33 / 64–88 / 160–212 ms against llm-d's 66 / 158–163 / 296–307 ms. On `zipf`,
llm-d v0.10 kept the hot prefix on one replica until it collapsed: hot-group TTFT p50 15.7–22.9 s against
prequal-epp's 33–34 ms.

**Standalone router** (`loaded`, 25 QPS, llm-d v0.10 chart; [`results/kind/router`](../results/kind/router)):

| Arm | Router CPU | Memory | TTFT p50 / p90 / p99 |
|---|---|---|---|
| Envoy + llm-d EPP | 1.78 + 1.75 = 3.53 cores | ~170–270 MiB | 66 / 166 / 300 |
| Envoy + prequal-epp | 2.21 + 0.48 = 2.69 cores | 52 MiB | 34 / 62–83 / 169–192 |
| `prequal-router` alone | **0.66 cores** | **13 MiB** | **26 / 37–40 / 123–128** |

## What these results don't show

- **No real GPUs.** llm-d-inference-sim models latency and a KV cache, but not prefill slowing concurrent decodes.
- **Small samples on a shared host.** Two rounds per arm, one for the two-picker cache run and the final-build check.
- **Popularity is approximated**: one hot group (`zipf`) or three tiers (cache), since inference-perf has no skew.
- **Lean mode** is measured end to end only on `loaded`; on `burst` and `zipf` only in the simulator.
- **A shared system prompt** across all traffic, where the prompt-key change matters, is tested only in simulation.

[chart]: https://github.com/llm-d/llm-d-router
[sim]: https://github.com/llm-d/llm-d-inference-sim
[perf]: https://github.com/kubernetes-sigs/inference-perf
[mooncake]: https://github.com/kvcache-ai/Mooncake
