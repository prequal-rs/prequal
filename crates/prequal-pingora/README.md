# prequal-pingora

[Prequal][paper] load balancing for [Pingora]. Pingora's built-in selections (round robin, random, hashing) ignore
backend load. `Prequal` is a drop-in `BackendSelection` that routes each request using asynchronously probed load
reports, skips backends ejected after consecutive failures, and still defers to Pingora's health checks.

```rust
use pingora_load_balancing::{Backends, LoadBalancer, discovery::Static};
use prequal_pingora::{Config, Prequal, PrequalSelector};

let selector = PrequalSelector::new(Config::default());
let discovery = Static::try_from_iter(["10.0.0.1:8000", "10.0.0.2:8000"]).unwrap();
let lb = LoadBalancer::<Prequal>::from_backends_with_config(Backends::new(discovery), Some(selector.clone()));

// upstream_peer:            let backend = lb.select(b"", 256);
// upstream_response_filter: selector.on_response(&backend, status, headers);
// fail_to_connect / error_while_proxy: selector.on_failure(&backend);
```

Backends should answer `GET /prequal/probe` and ideally attach the same load headers to every response (see
[`prequal-server`](../prequal-server)).

## Features

- `proxy`: a ready-made reverse proxy (`prequal_pingora::proxy::serve`); see `examples/proxy.rs`.
- `kubernetes`: `ServiceDiscovery` from a Service's EndpointSlices. Needs Rust 1.89, and the final binary must enable
  one `k8s-openapi` version feature (e.g. `v1_32`).

Building `pingora-core` (the `proxy` feature) needs `cmake`.

Part of [prequal](../../README.md). Licensed under MIT or Apache-2.0.

[Pingora]: https://github.com/cloudflare/pingora
[paper]: https://www.usenix.org/conference/nsdi24/presentation/wydrowski
