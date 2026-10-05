# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crates follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.1.0 - 2026-10-05

First release.

### Added

- `prequal-epp`: Gateway API Inference Extension endpoint picker (Envoy `ext_proc`, `InferencePool` v1), which replaces
  llm-d's picker image ([what it doesn't implement](docs/migrating-from-llm-d.md#check-what-you-would-lose)).
  - Accepts the flags of llm-d's `llm-d-router-standalone` chart (v0.10, v0.11) and warns about any that ask for
    behaviour it doesn't have.
  - llm-d's `llm_d_epp_*` metrics with llm-d's names, labels and buckets, including per-request, pool and per-endpoint
    series and `llm_d_epp_inflight_requests`.
  - llm-d's rejection semantics: 503 without endpoints, 429 for sheddable requests (negative `InferenceObjective`
    priority) while the pool is saturated, 413 for bodies over 10 MiB.
  - Graceful shutdown (`--drain-timeout`), stream and buffer limits (`--max-concurrent-streams`,
    `--max-buffered-body-mib`), and gRPC health for liveness and readiness.
  - A sleep-only `/bin/sh` in the image for the preStop hook llm-d's chart hard-codes.
  - Lean mode (`--prefill-signal`) for Envoy's `response_body_mode: NONE`.
  - `--conformance-test-hooks` for the GIE conformance suite (off by default).
  - Experimental `--engine-priority-handicap`: stamps vLLM's `priority` field for priority-scheduled engines, ordering
    by predicted KV cost with bounded overtaking (off by default).
- `prequal-router`: standalone OpenAI-compatible router for vLLM and SGLang, with static replicas or Kubernetes
  `EndpointSlice` discovery, sharded single-threaded serving, and a Helm chart.
  - `/healthz` and `/readyz` on `--admin-listen`, and draining on SIGTERM (`--drain-secs`).
  - Connection, stream, body-size and buffering limits, and client, connect and response timeouts.
- `prequal-llm`: the shared routing library.
  - Prefix affinity from an approximate prefix-cache index, fresh load from engine metrics plus in-flight placements,
    KV-cache headroom, hot-prefix spreading, overload shedding, and coordinated cold placement across router
    replicas.
  - Each replica's cache model is sized from the engine's KV capacity and the request bytes per engine token,
    measured from the engine's prefix-cache query counter.
  - Prompts are keyed past the prefix every replica holds, so a system prompt shared by all traffic no longer reads
    as one hot prefix.
  - Opt-in `prequal-home` policy for engines without prefix-cache counters (SGLang).
  - `Scheduler::replicas` exposes each replica's latest scrape, in-flight count and health.
  - Reimplemented baselines (llm-d, SGLang cache-aware, Dynamo) for comparison.
- `prequal-core`, `prequal-server`, `prequal-tower`, `prequal-pingora`: the Prequal paper (NSDI '24) for ordinary
  request/response services. Asynchronous probing, hot-cold lexicographic selection, piggybacked load headers
  compatible with envoy-prequal, and outlier ejection.
- Benchmarks on llm-d's own stack in kind (llm-d v0.10 and v0.11, including a cache-pressure workload with bounded
  KV caches and llm-d's precise prefix-cache configuration), a data-plane cost harness, and a virtual-time fleet
  simulator with production-trace replay (`prequal-testbed`, not published). See
  [docs/benchmarks.md](docs/benchmarks.md).
- Container images `ghcr.io/prequal-rs/prequal-epp` and `ghcr.io/prequal-rs/prequal-router` (linux/amd64, linux/arm64) and the
  Helm chart `oci://ghcr.io/prequal-rs/charts/prequal-router`, published by the tag-triggered release workflow.
- Prebuilt binaries attached to each GitHub release as `prequal-<version>-<target>.tar.gz` with a `SHA256SUMS` file:
  static `prequal-router` and `prequal-epp` for `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, and
  `prequal-router` for `aarch64-apple-darwin`.
- `prequal-router` accepts replicas by DNS name as well as IP. Names are re-resolved every 2 s, and each address of a
  name becomes a replica.
- A migration guide from llm-d's endpoint picker ([docs/migrating-from-llm-d.md](docs/migrating-from-llm-d.md)).
- A local Docker Compose demo (`deploy/demo`) that compares `prequal-router` with round-robin on two simulated vLLM
  fleets. It needs no GPUs and no cluster.
- A Grafana dashboard and Prometheus alert rules for `prequal-epp` (`deploy/grafana`).

### Notes for users of pre-release snapshots

- Types that will gain fields or variants are `#[non_exhaustive]`: `prequal_core::Config` and `Counters`,
  `prequal_tower`'s `GetResponse` and `ProbeCounts`, `prequal_pingora`'s `ServicePort`, and `prequal_llm`'s
  `Engine`, `EngineStats`, `PrefillSignal`, `Aggregate` and `ReplicaStatus`. Build a `Config` with
  `Config::default()` and then set fields, and `EngineStats` with `EngineStats::new`.
- `prequal_llm::Ticket` exposes `addr()` instead of a public field, and policy inputs (`Candidate`, `Request`) are read
  through accessors. `Policy::pick` takes a `PolicyRng`, so `rand` is not part of `prequal-llm`'s API.
  `prequal-core` re-exports the `rand` it uses as `prequal_core::rand`.
