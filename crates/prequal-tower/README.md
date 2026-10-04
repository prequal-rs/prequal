# prequal-tower

A [tower] load balancer implementing [Prequal][paper]: asynchronous probes, hot-cold lexicographic selection,
piggybacked load reports, and outlier ejection driven by request outcomes. It balances over any `tower::discover`
source, and probing never blocks a request.

Servers report requests in flight, e.g. with [`prequal-server`](../prequal-server). On a 20-server HTTP testbed at 0.9
load, this cut p99 latency by 28–37% (depending on the run) against tower's `p2c` + `PeakEwma`.

```rust
use prequal_tower::{Config, PrequalBalance, ProbeResponse, Prober};

struct LoadProber;

impl Prober<usize> for LoadProber {
    async fn probe(&self, key: &usize) -> Option<ProbeResponse> {
        // e.g. GET /prequal/probe on replica `key`, reading x-prequal-rif
        Some(ProbeResponse { rif: 0, latency_us: 0 })
    }
}

let mut config = Config::default();
(config.probes_per_query, config.removes_per_query) = (1.0, 0.34);
let mut balancer = PrequalBalance::from_services(replicas, LoadProber, config);
// `balancer` is a `tower::Service`: poll_ready, then call.
```

Use `PrequalHandle` to feed load reports piggybacked on responses, `with_classifier` to count e.g. HTTP 5xx as
failures, and export `counters()` / `probe_counts()` as metrics. See the crate docs for tuning.

Part of [prequal](../../README.md). Licensed under MIT or Apache-2.0.

[tower]: https://github.com/tower-rs/tower
[paper]: https://www.usenix.org/conference/nsdi24/presentation/wydrowski
