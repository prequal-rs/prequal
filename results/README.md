# Results

Raw data behind the numbers in the [README](../README.md) and [docs/benchmarks.md](../docs/benchmarks.md). All runs
were on one shared 12-thread Linux host with no GPUs. Section numbers refer to benchmarks.md.

| Directory | Backs | Produced by |
|---|---|---|
| [`kind/v0.11/`](kind/v0.11) | §1: `loaded`, `zipf`, `two-epp`, `burst`, `job1`, lean mode, resource use; the drop-in check (`check-*`, `v011-check.log`) | `tools/kind-llmd.sh suite <workload> 2` (`ARMS="prequal llmd prequal-lean"` for `loaded`) |
| [`kind/router-cache/`](kind/router-cache) | §2 final-build cache runs (one and two pickers), §1 final-build check (`loaded/`) | `tools/kind-cache.sh suite cache 2`, `suite cache-two-epp 1`; `tools/kind-llmd.sh suite loaded 1` |
| [`kind/cache-fix/`](kind/cache-fix) | §2, the cache-model calibration fix; `regression/`: `loaded` and `two-epp` with that build | as above |
| [`kind/cache/`](kind/cache) | §2, the first cache runs, llm-d's precise configurations, and the eviction calibration (`calibration.jsonl`) | `ARMS="prequal llmd llmd-precise" tools/kind-cache.sh suite cache 2`, then `llmd-precise-ob` |
| [`kind/gossip/`](kind/gossip) | [docs/peer-index.md](../docs/peer-index.md): two pickers with and without `--gossip-peers` | `ARMS="prequal-gossip prequal" tools/kind-cache.sh suite cache-two-epp 2` |
| [`kind/main/`](kind/main) | §5, llm-d v0.10 | `LLMD_VERSION=v0.10.0 tools/kind-llmd.sh suite <workload> 2` |
| [`kind/lean/`](kind/lean) | Lean vs default prequal-epp on llm-d v0.10 | `ARMS="prequal-lean prequal" TAG=ab tools/kind-llmd.sh suite loaded 2` |
| [`kind/router/`](kind/router) | §5, standalone router vs Envoy + picker (llm-d v0.10) | `tools/kind-llmd.sh run <router\|prequal\|llmd> <label> loaded` |
| [`vsim-routercache/`](vsim-routercache) | §4: current scorecard (`scorecard-final.md`), collapse, production-trace replays (`trf-s1/`, `trf-s2/`), and the router-only ideas that lost | `tools/vsim-compete.sh`, `tools/vsim-collapse.sh`, `tools/llm-compare.mjs` |
| [`vsim-cachefix/`](vsim-cachefix) | The calibration fix in simulation: scorecards before and after, bytes-per-token sweeps with `--diagnose` | as above, `llm-bench --virtual --token-bytes N --diagnose` |
| [`vsim-peer-index/`](vsim-peer-index), [`vsim-peer-index-restart/`](vsim-peer-index-restart) | [docs/peer-index.md](../docs/peer-index.md): one to four routers, today against the engine-cache oracle and placement gossip | `tools/vsim-peer-index.sh` (arms in that page) |
| [`vsim/`](vsim) | The scorecard before the cache-pressure work | `tools/vsim-compete.sh` |
| [`vsim-collapse/`](vsim-collapse) | Host-slowdown collapse counts before the cache-pressure work | `tools/vsim-collapse.sh` |
| [`vsim-lean/`](vsim-lean) | [Lean mode](../docs/lean-mode.md) in simulation | `tools/vsim-compete.sh` with `--prefill-signal scrape` |
| [`http/`](http) | README, general-purpose load balancing (p99 vs `p2c` + `PeakEwma`, failing servers) | `tools/grid.sh`, `prequal-testbed` |
| [`conformance/`](conformance) | [docs/conformance.md](../docs/conformance.md): GIE suite reports and per-test summaries, per gateway and picker | `[GATEWAY=istio] tools/gie-conformance.sh run <lwepp\|prequal>` |

Section 3 of benchmarks.md (data-plane cost from `tools/epp-load.sh`) has no stored data: each harness run prints one
line per configuration.

## kind

Each `reports/` directory holds inference-perf report directories (per-stage `stage_N_lifecycle_metrics.json`,
`summary_lifecycle_metrics.json`, the run's `config.yaml`) plus the driver's logs per run: `-sims.txt` (simulator
prefix-cache counters at the end), `-hits.log` (the same counters every 5 s), `-stages.log` (stage boundaries),
`-top.log` (`kubectl top` every 10 s) and `box-load.log` (host load at each run's start). The cache runs add
`-epp.log` (picker metrics), `host-cpu.log` (CPU used by other jobs on the host) and one report directory per load
generator (`-hot`, `-cold`). Per-request logs are omitted for size.

The `*.jsonl` files are summaries, one line per run and stage with TTFT in seconds:

```sh
BENCH=results/kind/v0.11 tools/kind-llmd.sh results '*-loaded-*'    # adds hit rate and peak CPU/memory per container
node tools/cache-report.mjs results/kind/router-cache/reports '^(prequal|llmd)-cache-r.$' --md   # per-arm tables
```

`cache-report.mjs` combines the three load generators' TTFT percentiles into mixture quantiles (inference-perf has no
per-request data at these sizes), and computes prefix-hit rate from the simulator counter timeline, recomputed prompt
tokens per request, and capacity at a TTFT p90 target (`--slo-p90`, default 0.5 s).

Labels: `-r1`/`-r2` are alternating rounds; `-hot` and `-cold` are a workload's extra load generators; `prequal-lean`
is the lean-mode arm. In `kind/cache/`, `prequal-cache-r2` overlapped another job's CPU spike and was rerun as
`prequal-cache-r3`; the tables use r1 and r3. `cal-*` runs predate the stage barrier and are used only for whole-run
hit rates. In `kind/main/`, `v2-loaded` is `loaded` after that build's final policy revision and `job1-v2` the
re-run of `job1`; those `-top.log` files predate a sampler fix, so §5's resource numbers come from `kind/router/`.

## vsim

`*.csv` are `llm-bench --virtual` rows, one per policy, seed and load stage; `scorecard*.md` are the comparisons.
Runs are deterministic per seed. Commands are in [benchmarks.md §4](../docs/benchmarks.md#4-virtual-time-simulator);
the production traces are fetched by `tools/fetch-mooncake-traces.sh` and not stored here.

## http

`prequal-testbed` runs 20 HTTP servers and 20 clients on one host (sleep-based work, 2 seeds per cell).

- `grid.csv`: `tools/grid.sh` with `SEEDS="1 2" LOADS="0.7 0.9 1.05"`; `node tools/summarize.mjs results/http/grid.csv`.
  The README's 37% is `Prequal p=0.5+pb Rif` against `P2cEwma` at load 0.9.
- `five-seed/`: the later five-seed rerun of the same grid and sinkhole test, which gives the README's 28% (same cell)
  and 3,554 → 536 failed requests.
- `sinkhole.csv`: 2 of 20 servers fail instantly while reporting zero load, seed 1, one `p2c-ewma` run and two
  Prequal runs (the file predates the columns recording ejection settings; the README quotes the better one). It
  was recorded with an earlier testbed revision, so absolute counts from the current binary differ:

  ```sh
  target/release/prequal-testbed --policy p2c-ewma --failing-servers 2 --load 0.8 --csv results/http/sinkhole.csv
  target/release/prequal-testbed --policy prequal --probes-per-query 0.5 --piggyback --failing-servers 2 --load 0.8 \
    --csv results/http/sinkhole.csv
  ```
