# prequal-llm

Load- and prefix-cache-aware routing for LLM inference fleets (vLLM, SGLang). It reads the Prometheus metrics the
engines already export, so no engine changes are needed. This is the routing library behind
[`prequal-epp`](../prequal-epp) and [`prequal-router`](../prequal-router).

- `Scheduler` routes whole requests: it hashes each prompt into blocks (`Prompt`), keeps an approximate,
  self-correcting model of each replica's prefix cache (sized from the engine's KV capacity and its measured request
  bytes per token), and picks with a `Policy`. `Scheduler::replicas` reports each replica's latest state.
- `policy::Prequal` (the default) combines prefix affinity, fresh load, KV-cache headroom, hot-prefix spreading and
  overload shedding. Baselines (`round-robin`, `llmd-optimized`, `sglang-cache-aware`, `dynamo`, ...) are available
  through `policy::by_name` for comparison.
- `EngineProber` + `scrape_forever` keep replica state current from `/metrics`.

```rust
use std::time::Duration;
use prequal_llm::{Engine, EngineProber, Prompt, Scheduler, policy, scrape_forever};

let scheduler = Scheduler::new(Box::new(policy::Prequal::default()));
scheduler.sync(replica_addrs);
tokio::spawn(scrape_forever(scheduler.clone(), EngineProber::new(Engine::Vllm), Duration::from_millis(50)));

let mut ticket = scheduler.route(&Prompt::from_body(&body), max_tokens, |_| true).expect("a replica");
// Forward the request to `ticket.addr()`; call `ticket.first_token()` on the first body chunk,
// and drop the ticket when the response ends.
```

Part of [prequal](../../README.md). Licensed under MIT or Apache-2.0.
