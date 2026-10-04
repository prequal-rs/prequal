//! Load-aware, prefix-cache-aware routing for LLM inference fleets (vLLM, SGLang).
//!
//! Engines already publish their load on Prometheus `/metrics`, so no engine changes are needed:
//! - [`EngineProber`] scrapes queue depth, KV usage and prefix-cache counters. As a Prequal
//!   [`prequal_tower::Prober`] it drives plain load balancing (running + waiting = RIF).
//! - [`Scheduler`] routes whole requests: it hashes the prompt into blocks ([`Prompt`]), keeps an approximate,
//!   self-correcting model of each replica's prefix cache, and picks with a [`Policy`], by default
//!   [`policy::Prequal`] (sticky prefix affinity that replicates only hot prefixes, with load-bounded escapes).
//!   [`scrape_forever`] keeps it current.
//!
//! Used by `prequal-router` and `prequal-epp` (Gateway API Inference Extension endpoint picker).
//!
//! # Example
//!
//! ```
//! use std::net::SocketAddr;
//! use prequal_llm::{EngineStats, Prompt, Scheduler, policy};
//!
//! let scheduler = Scheduler::new(Box::new(policy::Prequal::default()));
//! let replicas: Vec<SocketAddr> = vec!["10.0.0.1:8000".parse().unwrap(), "10.0.0.2:8000".parse().unwrap()];
//! scheduler.sync(replicas.iter().copied());
//! // In production `scrape_forever(scheduler.clone(), EngineProber::new(Engine::Vllm), interval)` does this.
//! for &addr in &replicas {
//!     scheduler.observe(addr, EngineStats::default());
//! }
//!
//! let body = br#"{"model":"m","prompt":"You are a helpful assistant. ..."}"#;
//! let mut ticket = scheduler.route(&Prompt::from_body(body), 256, |_| true).expect("a replica is up");
//! // Forward `body` to `ticket.addr()`, then report the response's progress:
//! ticket.response_started();
//! ticket.first_token();
//! drop(ticket); // the response ended
//! ```

mod engine;
mod fleet;
mod heat;
mod index;
pub mod metrics;
pub mod policy;
mod prompt;
pub mod queue_order;
mod rate;
mod scheduler;
mod scrape;
mod ticket;

pub use engine::{Engine, EngineProber, EngineStats};
pub use policy::Policy;
pub use prompt::{
    BLOCK_BYTES, BYTES_PER_TOKEN, DEFAULT_MAX_TOKENS, Prompt, completion_tokens, max_tokens, prompt_region,
};
pub use scheduler::{Clock, ReplicaStatus, Scheduler};
pub use scrape::scrape_forever;
pub use ticket::{PrefillSignal, Ticket};
