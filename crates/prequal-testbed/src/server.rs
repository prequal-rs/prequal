use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use prequal_server::{HEADER_LATENCY_US, HEADER_RIF, LatencyEstimator, ProbeLayer, ProbeResponse, ProbeState};
use rand::{RngExt, SeedableRng, rngs::SmallRng};
use serde::Deserialize;
use tokio::{net::TcpListener, sync::Semaphore};

use crate::work;

pub type Estimator = Box<dyn LatencyEstimator>;

/// Heterogeneous fleet with noisy neighbours.
pub struct FleetSpec {
    pub servers: usize,
    pub cores: usize,
    pub slow_fraction: f64,
    pub noisy_fraction: f64,
    /// The last (fast) servers answer 500 instantly while reporting no load: the sinkhole case.
    pub failing: usize,
    /// Burn real CPU instead of sleeping, so servers contend for the host's cores.
    pub cpu_work: bool,
    pub seed: u64,
}

impl FleetSpec {
    fn base_speed(&self, index: usize) -> f64 {
        if (index as f64) < self.servers as f64 * self.slow_fraction { 0.5 } else { 1.0 }
    }

    /// Requests/s the fleet serves at its nominal allocation (before noise), for a mean work size.
    pub fn allocation_capacity(&self, mean_work_us: f64) -> f64 {
        let core_speed: f64 = (0..self.servers).map(|i| self.base_speed(i)).sum();
        core_speed * self.cores as f64 * 1e6 / mean_work_us
    }
}

#[derive(Clone)]
struct Host {
    cores: Arc<Semaphore>,
    base_speed: f64,
    noise: Arc<AtomicU64>,
    probe: ProbeState<Estimator>,
    failing: bool,
    /// `Some(iterations per µs)` burns CPU; `None` sleeps.
    cpu_rate: Option<f64>,
}

#[derive(Deserialize)]
struct Work {
    units_us: u64,
}

/// Piggybacks a load report on every response; RIF excludes the request being answered.
async fn work(State(host): State<Host>, Query(w): Query<Work>) -> Response {
    if host.failing {
        let idle = ProbeResponse { rif: 0, latency_us: 0 };
        return (StatusCode::INTERNAL_SERVER_ERROR, load_headers(idle)).into_response();
    }
    {
        let _core = host.cores.acquire().await.expect("semaphore never closes");
        let speed = host.base_speed * f64::from_bits(host.noise.load(Ordering::Relaxed));
        let micros = w.units_us as f64 / speed;
        match host.cpu_rate {
            Some(rate) => {
                let iterations = (micros * rate) as u64;
                tokio::task::spawn_blocking(move || work::burn(iterations)).await.expect("burn panicked");
            }
            None => tokio::time::sleep(Duration::from_micros(micros as u64)).await,
        }
    }
    let mut report = host.probe.probe();
    report.rif = report.rif.saturating_sub(1);
    (load_headers(report), "ok").into_response()
}

fn load_headers(report: ProbeResponse) -> HeaderMap {
    HeaderMap::from_iter([
        (HeaderName::from_static(HEADER_RIF), HeaderValue::from(report.rif)),
        (HeaderName::from_static(HEADER_LATENCY_US), HeaderValue::from(report.latency_us)),
    ])
}

async fn wander(noise: Arc<AtomicU64>, noisy: bool, mut rng: SmallRng) {
    loop {
        let factor = match (noisy, rng.random_bool(0.5)) {
            (true, true) => rng.random_range(0.3..0.7),
            (true, false) => rng.random_range(0.9..1.3),
            (false, _) => rng.random_range(1.2..2.0),
        };
        noise.store(f64::to_bits(factor), Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(rng.random_range(700..1300))).await;
    }
}

pub async fn spawn_fleet(spec: &FleetSpec, estimator: impl Fn() -> Estimator) -> Vec<SocketAddr> {
    let mut rng = SmallRng::seed_from_u64(spec.seed);
    let mut addrs = Vec::with_capacity(spec.servers);
    let cpu_rate = spec.cpu_work.then(work::calibrate);
    for index in 0..spec.servers {
        let probe_state = ProbeState::new(estimator());
        let host = Host {
            cores: Arc::new(Semaphore::new(spec.cores)),
            base_speed: spec.base_speed(index),
            noise: Arc::new(AtomicU64::new(f64::to_bits(1.0))),
            probe: probe_state.clone(),
            failing: index >= spec.servers.saturating_sub(spec.failing),
            cpu_rate,
        };
        let noisy = rng.random_bool(spec.noisy_fraction);
        tokio::spawn(wander(Arc::clone(&host.noise), noisy, SmallRng::seed_from_u64(rng.random())));

        let probe_handler = {
            let state = probe_state.clone();
            move || std::future::ready(load_headers(state.probe()))
        };
        let app = Router::new()
            .route("/work", post(work).layer(ProbeLayer::new(probe_state)))
            .with_state(host)
            .route("/prequal/probe", get(probe_handler));

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind localhost");
        addrs.push(listener.local_addr().expect("bound address"));
        tokio::spawn(async move { axum::serve(listener, app).await.expect("server crashed") });
    }
    addrs
}
