# Tools

Benchmark drivers and helpers. Each script's header has its full usage; outputs and the exact commands behind the
published numbers are in [results/README.md](../results/README.md).

| Script | What it does |
|---|---|
| `kind-llmd.sh` | prequal-epp vs llm-d's picker on llm-d's chart, simulator and load generator in kind ([deploy/bench/llmd](../deploy/bench/llmd/README.md)) |
| `kind-cache.sh` | The cache-pressure workload on `kind-llmd.sh`'s harness: bounded simulator KV caches, tiered popularity, a load sweep, llm-d's precise prefix-cache arms |
| `cache-report.mjs` | Per-stage tables of `kind-cache.sh` runs: mixture TTFT across load generators, hit rate, recomputed tokens, capacity at a TTFT target |
| `fetch-mooncake-traces.sh` | Downloads the public Mooncake request traces for `llm-bench --trace` and the `trace-*` simulator scenarios |
| `gie-conformance.sh` | Gateway API Inference Extension conformance suite in kind, with lwepp or prequal-epp as every pool's picker ([docs/conformance.md](../docs/conformance.md)) |
| `kind-epp-cpu.sh` | Picker and Envoy CPU per request in the same kind cluster, for picker-flag variants; optional `perf` and syscall tracing |
| `epp-load.sh` | Data-plane cost harness (`prequal-epp` example `epp_load`, real Envoy in front) run over ssh on a remote Linux host |
| `vsim-compete.sh` | All policies × scenarios × seeds in the virtual-time simulator (`llm-bench --virtual`), in parallel |
| `vsim-collapse.sh` | Collapse frequency under simulated host slowdown on the kind `loaded` workload |
| `vsim-queue-order.sh` | Engine queue orders (`llm-bench --queue-order`, vLLM priority scheduling) against FCFS on the Mooncake traces; `ENGINE_ARGS` points it at real vLLM servers instead of virtual time |
| `vsim-peer-index.sh` | What routers sharing a fleet gain from an engine-cache oracle or from gossiping placements, for one to four routers ([docs/peer-index.md](../docs/peer-index.md)) |
| `queue-order-summary.mjs` | `vsim-queue-order.sh` output as per-trace markdown tables, each order's change against FCFS |
| `llm-compete.sh` | The same scenarios over real sockets (`llm-bench` with `prequal-router` processes); slow |
| `llm-compare.mjs` | Per-stage win/loss table of one policy against a rival, from `vsim-compete.sh` or `llm-compete.sh` output |
| `llm-summary.mjs` | TTFT, throughput and hit rate per policy and stage, as markdown |
| `grid.sh` | General-purpose Prequal on the HTTP testbed (`prequal-testbed`): policies × loads × seeds |
| `summarize.mjs` | Seed-averaged markdown table of `grid.sh` output |
| `quiet-run` | Runs a command once the host is idle, under a shared lock (used by `llm-compete.sh`) |
| `folded-share.sh` | Share of profile samples per stack regex, from a folded-stack file |
| `epp-syscalls.bt` | bpftrace: syscall counts and bytes of one process (used by `kind-epp-cpu.sh`) |
| `with-cmake.sh` | Puts Visual Studio's bundled cmake on `PATH` on Windows, for the Pingora crates |

## Host assumptions

The simulator scripts (`vsim-*.sh`, `*.mjs`) run anywhere with bash and Node. The rest were written for one shared,
dedicated Linux host with 6 cores / 12 threads and 31 GB of RAM:

- `kind-llmd.sh`, `kind-cache.sh`, `kind-epp-cpu.sh`, `gie-conformance.sh`, `llm-compete.sh` and `epp-load.sh` take
  turns through `flock ~/bench.lock`.
- `llm-compete.sh` and `epp-load.sh` pin work with `taskset`, and `kind-cache.sh` pins the kind node, keeping CPUs 5
  and 11 free; adjust the CPU lists for another machine (`epp-load.sh` takes `EPP_CPUS` and `DRIVER_CPUS`,
  `kind-cache.sh` takes `CPUS`).
- `epp-load.sh` drives the host named by `BENCH_HOST` (required) over ssh, in a checkout at `~/$BOX` (default
  `work/prequal-rs-perf`), with Envoy at `$ENVOY` (default `~/work/bin/envoy-1.33.2`, extracted from
  `envoyproxy/envoy:distroless-v1.33.2`). `perf` needs `perf`, `rustfilt` and `inferno` on that host.
- `kind-epp-cpu.sh` reuses `kind-llmd.sh`'s cluster and expects llm-d's `optimized-baseline.yaml` in `~/eppperf`
  (`kind-llmd.sh setup` downloads it to `$BENCH`).
