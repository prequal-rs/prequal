use std::{
    net::SocketAddr,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use prequal_tower::{Config, PrequalBalance, PrequalHandle, ProbeCounts, StaticList};
use rand::{RngExt, SeedableRng, rngs::SmallRng};
use tokio::{sync::mpsc, task::JoinHandle};
use tower::{
    BoxError, Service, ServiceExt,
    balance::p2c,
    discover::ServiceList,
    load::{CompleteOnResponse, PeakEwma, PendingRequests},
};

use crate::http::{HttpEndpoint, HttpProber};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Policy {
    Prequal,
    P2cEwma,
    P2cLr,
    RoundRobin,
}

pub struct LoadSpec {
    pub rate_per_client: f64,
    pub mean_work_us: f64,
    pub warmup: Duration,
    pub duration: Duration,
    pub request_timeout: Duration,
    pub prequal: Config,
    pub probe_timeout: Duration,
    pub max_in_flight_probes: usize,
    pub piggyback: bool,
}

/// `Some(latency_us)` for a completed request, `None` for an error or timeout.
pub type Outcome = Option<u64>;

fn exponential(rng: &mut SmallRng, mean: f64) -> f64 {
    -mean * (1.0 - rng.random::<f64>()).ln()
}

struct RoundRobin {
    endpoints: Vec<HttpEndpoint>,
    next: usize,
}

impl Service<u64> for RoundRobin {
    type Response = ();
    type Error = reqwest::Error;
    type Future = <HttpEndpoint as Service<u64>>::Future;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, units_us: u64) -> Self::Future {
        self.next = (self.next + 1) % self.endpoints.len();
        self.endpoints[self.next].call(units_us)
    }
}

/// Open-loop Poisson arrivals: latency includes client-side waiting for the balancer.
async fn drive<S>(mut svc: S, spec: Arc<LoadSpec>, seed: u64, out: mpsc::UnboundedSender<Outcome>) -> S
where
    S: Service<u64>,
    S::Error: Into<BoxError>,
    S::Future: Send + 'static,
{
    let mut rng = SmallRng::seed_from_u64(seed);
    let start = Instant::now();
    let measure_from = start + spec.warmup;
    let end = measure_from + spec.duration;
    let mut next = start;
    loop {
        next += Duration::from_secs_f64(exponential(&mut rng, 1.0 / spec.rate_per_client));
        if next >= end {
            break;
        }
        tokio::time::sleep_until(next.into()).await;
        let arrived = Instant::now();
        let units = exponential(&mut rng, spec.mean_work_us) as u64;
        let call = match svc.ready().await {
            Ok(ready) => ready.call(units),
            Err(_) => {
                let _ = out.send(None);
                continue;
            }
        };
        let out = out.clone();
        let timeout = spec.request_timeout;
        tokio::spawn(async move {
            let ok = matches!(tokio::time::timeout(timeout, call).await, Ok(Ok(_)));
            if arrived >= measure_from {
                let _ = out.send(ok.then(|| arrived.elapsed().as_micros() as u64));
            }
        });
    }
    svc
}

fn spawn_client(
    policy: Policy,
    addrs: &[SocketAddr],
    spec: &Arc<LoadSpec>,
    seed: u64,
    out: mpsc::UnboundedSender<Outcome>,
) -> JoinHandle<ProbeCounts> {
    let http = reqwest::Client::new();
    let endpoints: Vec<_> = addrs.iter().map(|&a| HttpEndpoint::new(http.clone(), a)).collect();
    let spec = Arc::clone(spec);
    let rtt = Duration::from_millis(30);
    let decay_ns = Duration::from_secs(10).as_nanos() as f64;
    match policy {
        Policy::Prequal => {
            // Own connection pool so a timed-out probe never tears down a request connection.
            let prober = HttpProber::new(reqwest::Client::new(), addrs);
            let handle = PrequalHandle::new(spec.prequal.clone());
            let endpoints = if spec.piggyback {
                endpoints.into_iter().enumerate().map(|(i, e)| e.with_feedback(handle.clone(), i)).collect()
            } else {
                endpoints
            };
            let svc = PrequalBalance::with_handle(handle, StaticList::new(endpoints), prober)
                .with_probe_timeout(spec.probe_timeout)
                .with_max_in_flight_probes(spec.max_in_flight_probes);
            tokio::spawn(async move { drive(svc, spec, seed, out).await.probe_counts() })
        }
        Policy::P2cEwma => {
            let loaded = endpoints.into_iter().map(|e| PeakEwma::new(e, rtt, decay_ns, CompleteOnResponse::default()));
            let svc = p2c::Balance::new(ServiceList::new(loaded.collect::<Vec<_>>()));
            tokio::spawn(without_probes(drive(svc, spec, seed, out)))
        }
        Policy::P2cLr => {
            let loaded = endpoints.into_iter().map(|e| PendingRequests::new(e, CompleteOnResponse::default()));
            let svc = p2c::Balance::new(ServiceList::new(loaded.collect::<Vec<_>>()));
            tokio::spawn(without_probes(drive(svc, spec, seed, out)))
        }
        Policy::RoundRobin => {
            let next = seed as usize % endpoints.len();
            tokio::spawn(without_probes(drive(RoundRobin { endpoints, next }, spec, seed, out)))
        }
    }
}

async fn without_probes<S>(driving: impl Future<Output = S>) -> ProbeCounts {
    driving.await;
    ProbeCounts::default()
}

pub struct RunResult {
    pub outcomes: Vec<Outcome>,
    pub probes: ProbeCounts,
}

pub async fn run(policy: Policy, addrs: &[SocketAddr], clients: usize, spec: LoadSpec, seed: u64) -> RunResult {
    let spec = Arc::new(spec);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let handles: Vec<_> = (0..clients)
        .map(|c| spawn_client(policy, addrs, &spec, seed.wrapping_mul(1_000).wrapping_add(c as u64), tx.clone()))
        .collect();
    drop(tx);
    let mut outcomes = Vec::new();
    while let Some(outcome) = rx.recv().await {
        outcomes.push(outcome);
    }
    let mut probes = ProbeCounts::default();
    for handle in handles {
        probes = probes + handle.await.expect("client task panicked");
    }
    RunResult { outcomes, probes }
}
