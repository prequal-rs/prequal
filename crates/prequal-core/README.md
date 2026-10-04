# prequal-core

Runtime-agnostic core of [Prequal][paper] (Wydrowski et al., NSDI '24), the load balancer behind YouTube: a pool of
asynchronously probed load reports, hot-cold lexicographic (HCL) replica selection, and outlier ejection. No I/O and no
runtime; callers supply the clock and RNG.

Most users want a crate that drives this state machine for them: [`prequal-tower`](../prequal-tower) or
[`prequal-pingora`](../prequal-pingora). Servers report their load with [`prequal-server`](../prequal-server).

```rust
use prequal_core::{Config, Prequal, ProbeResponse};
use rand::{SeedableRng, rngs::SmallRng};

let mut rng = SmallRng::seed_from_u64(1);
let mut balancer = Prequal::new(Config::default(), 4);

for replica in balancer.probe_targets(&mut rng) {
    // Probe asynchronously in a real client; answers route later queries.
    balancer.record_probe(replica, ProbeResponse { rif: 2, latency_us: 1_000 }, 0, &mut rng);
}
let replica = balancer.select(0, &mut rng);
balancer.record_success(replica); // or record_failure(replica, now_us)
```

Part of [prequal](../../README.md). Licensed under MIT or Apache-2.0.

[paper]: https://www.usenix.org/conference/nsdi24/presentation/wydrowski
