# prequal-server

Server side of [Prequal][paper] load balancing: track requests in flight (RIF), estimate latency, and answer probes
from clients such as [`prequal-tower`](../prequal-tower) or [`prequal-pingora`](../prequal-pingora).

Wrap the routes that do real work in `ProbeLayer` (a tower layer), answer `GET /prequal/probe` from the same
`ProbeState`, and ideally piggyback the report on every response so clients need few explicit probes. Report headers
are `x-prequal-rif` and `x-prequal-latency-us`, compatible with envoy-prequal.

```rust
use prequal_server::{HEADER_LATENCY_US, HEADER_RIF, ProbeLayer, ProbeState, RifOnly};
use tower_layer::Layer;

let state = ProbeState::new(RifOnly);
let work_service = ProbeLayer::new(state.clone()).layer(my_service);

// In the probe handler (and optionally on every response):
let report = state.probe();
let headers = [(HEADER_RIF, report.rif.to_string()), (HEADER_LATENCY_US, report.latency_us.to_string())];
```

`RifOnly` is the recommended estimator. `RecentMedian` (the paper's) and `ServiceTimeModel` can make clients herd onto
few replicas and are experimental.

Part of [prequal](../../README.md). Licensed under MIT or Apache-2.0.

[paper]: https://www.usenix.org/conference/nsdi24/presentation/wydrowski
