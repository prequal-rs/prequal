//! HTTP reverse proxy balancing with Prequal (or Pingora's round robin, for comparison).
//!
//! ```sh
//! PREQUAL_LISTEN=127.0.0.1:6188 cargo run --release -p prequal-pingora --features proxy --example proxy -- \
//!     127.0.0.1:8001 127.0.0.1:8002 127.0.0.1:8003
//! ```
//! `PREQUAL_POLICY=round-robin` selects Pingora's built-in round robin instead; `PREQUAL_STATS=1`
//! logs Prequal counters to stderr every 2 s. Backends should serve `GET /prequal/probe` with
//! `x-prequal-rif` / `x-prequal-latency-us` (see `prequal-server`) and ideally attach them to
//! every response.

use std::time::Duration;

use pingora_core::server::Server;
use pingora_load_balancing::{Backends, LoadBalancer, discovery::Static, selection::RoundRobin};
use prequal_pingora::{Config, Prequal, PrequalSelector, proxy::serve};

fn main() {
    let backends: Vec<String> = std::env::args().skip(1).collect();
    assert!(!backends.is_empty(), "usage: proxy <backend-addr>...");
    let listen = std::env::var("PREQUAL_LISTEN").unwrap_or_else(|_| "127.0.0.1:6188".to_owned());

    let mut server = Server::new(None).expect("server");
    server.bootstrap();
    let discovery = Static::try_from_iter(backends.iter().map(String::as_str)).expect("backend addresses");

    if std::env::var("PREQUAL_POLICY").as_deref() == Ok("round-robin") {
        serve(server, LoadBalancer::<RoundRobin>::from_backends(Backends::new(discovery)), None, &listen);
    }
    let mut config = Config::default();
    (config.probes_per_query, config.removes_per_query) = (0.5, 0.5);
    let selector = PrequalSelector::new(config);
    if std::env::var_os("PREQUAL_STATS").is_some() {
        let (selector, listen) = (selector.clone(), listen.clone());
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(2));
                eprintln!("[{listen}] {:?} {:?}", selector.counters(), selector.probe_counts());
            }
        });
    }
    let lb = LoadBalancer::<Prequal>::from_backends_with_config(Backends::new(discovery), Some(selector.clone()));
    serve(server, lb, Some(selector), &listen);
}
