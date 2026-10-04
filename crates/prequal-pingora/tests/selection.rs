//! `Prequal` selection inside a Pingora `LoadBalancer`, probing local servers.

use std::{net::SocketAddr, time::Duration};

use http::{HeaderMap, HeaderValue};
use pingora_load_balancing::{Backend, Backends, LoadBalancer, discovery::Static};
use prequal_pingora::{Config, Prequal, PrequalSelector};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

/// Minimal keep-alive HTTP server answering every request with a fixed load report.
async fn probe_server(rif: u32) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut conn = BufReader::new(stream);
                let mut line = String::new();
                loop {
                    line.clear();
                    match conn.read_line(&mut line).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) if line == "\r\n" => {
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nx-prequal-rif: {rif}\r\nx-prequal-latency-us: 0\r\ncontent-length: 0\r\n\r\n"
                            );
                            if conn.get_mut().write_all(response.as_bytes()).await.is_err() {
                                return;
                            }
                        }
                        Ok(_) => {}
                    }
                }
            });
        }
    });
    addr
}

async fn balancer(rifs: &[u32], config: Config) -> (LoadBalancer<Prequal>, PrequalSelector, Vec<Backend>) {
    let mut addrs = Vec::new();
    for &rif in rifs {
        addrs.push(probe_server(rif).await);
    }
    let backends: Vec<Backend> = addrs.iter().map(|a| Backend::new(&a.to_string()).unwrap()).collect();
    let discovery = Static::try_from_iter(addrs.iter().map(|a| a.to_string())).unwrap();
    let selector = PrequalSelector::new(config);
    let lb = LoadBalancer::<Prequal>::from_backends_with_config(Backends::new(discovery), Some(selector.clone()));
    lb.update().await.unwrap();
    (lb, selector, backends)
}

async fn route(lb: &LoadBalancer<Prequal>, backends: &[Backend], requests: usize) -> Vec<usize> {
    let mut hits = vec![0; backends.len()];
    for _ in 0..requests {
        let chosen = lb.select(b"", 16).expect("a backend");
        hits[backends.iter().position(|b| *b == chosen).unwrap()] += 1;
        tokio::time::sleep(Duration::from_micros(200)).await;
    }
    hits
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn routes_to_least_loaded_and_survives_rebuilds() {
    let (lb, selector, backends) = balancer(&[30, 25, 0, 40, 35], Config::default()).await;
    let warm = route(&lb, &backends, 300).await;
    assert!(warm[2] > 200, "{warm:?}");

    // Fresh state would route ~uniformly (20% each) until probes return; retained state keeps
    // the least-loaded backend dominant immediately.
    lb.update().await.unwrap();
    let after_rebuild = route(&lb, &backends, 50).await;
    assert!(after_rebuild[2] > 30, "state lost across rebuild: {after_rebuild:?}");

    // Timeouts are tolerated: a saturated test machine can exceed the 50 ms probe timeout.
    let probes = selector.probe_counts();
    assert!(probes.answered > 150, "{probes:?}");
    assert_eq!(probes.failed, 0, "{probes:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failing_backend_is_ejected() {
    // Long ejection: coarse Windows timers can make the routing loop outlast a 1 s ejection.
    let mut config = Config::default();
    (config.base_ejection_us, config.max_ejection_us) = (60_000_000, 60_000_000);
    let (lb, selector, backends) = balancer(&[10, 10, 0, 10], config).await;
    route(&lb, &backends, 100).await;
    for _ in 0..Config::default().eject_after_failures {
        selector.on_response(&backends[2], 503, &HeaderMap::new());
    }
    assert_eq!(selector.counters().ejections, 1);
    let hits = route(&lb, &backends, 200).await;
    assert_eq!(hits[2], 0, "{hits:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fallbacks_spread_when_the_pick_is_rejected() {
    let (lb, _, backends) = balancer(&[0, 0, 0, 0, 0, 0], Config::default()).await;
    let mut hits = vec![0usize; backends.len()];
    for _ in 0..600 {
        let first = std::cell::Cell::new(true);
        // Reject the first candidate (Prequal's pick), as a failing health check would.
        let chosen = lb.select_with(b"", 16, |_, _| !first.replace(false)).unwrap();
        hits[backends.iter().position(|b| *b == chosen).unwrap()] += 1;
    }
    assert!(hits.iter().all(|&h| h > 50), "fallbacks herd: {hits:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn piggybacked_headers_steer_without_probes() {
    let mut config = Config::default();
    (config.probes_per_query, config.removes_per_query) = (0.0, 0.25);
    let (lb, selector, backends) = balancer(&[0, 0, 0, 0], config).await;
    let mut hits = [0usize; 4];
    for _ in 0..200 {
        let chosen = lb.select(b"", 16).unwrap();
        let i = backends.iter().position(|b| *b == chosen).unwrap();
        hits[i] += 1;
        let mut headers = HeaderMap::new();
        headers.insert("x-prequal-rif", HeaderValue::from([20u32, 0, 30, 25][i]));
        headers.insert("x-prequal-latency-us", HeaderValue::from(0u32));
        selector.on_response(&chosen, 200, &headers);
    }
    // Only the chosen backend reports, so exploration comes from random fallbacks when the pool
    // drains; the least-loaded backend should still win the plurality.
    let counters = selector.counters();
    assert_eq!(hits.iter().max(), Some(&hits[1]), "{hits:?} {counters:?}");
    assert!(counters.random_fallbacks < counters.selections / 2, "{counters:?}");
    assert_eq!(selector.probe_counts().sent, 0);
}
