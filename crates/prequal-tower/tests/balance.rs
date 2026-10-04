//! `PrequalBalance` end to end over in-process services.

use std::{
    convert::Infallible,
    future::{Ready, pending, ready},
    task::{Context, Poll},
    time::Duration,
};

use futures_util::stream;
use prequal_tower::{Config, PrequalBalance, PrequalHandle, ProbeResponse, Prober, StaticList};
use tokio::sync::mpsc;
use tower::{BoxError, discover::Change};
use tower_service::Service;

/// Answers with its id, or fails instantly with low load (the sinkhole case) when `fail` is set.
#[derive(Clone)]
struct Svc {
    id: usize,
    fail: bool,
}

impl Service<()> for Svc {
    type Response = usize;
    type Error = &'static str;
    type Future = Ready<Result<usize, &'static str>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: ()) -> Self::Future {
        ready(if self.fail { Err("boom") } else { Ok(self.id) })
    }
}

fn svcs(n: usize) -> Vec<Svc> {
    (0..n).map(|id| Svc { id, fail: false }).collect()
}

struct StaticLoad(Vec<u32>);

impl Prober<usize> for StaticLoad {
    async fn probe(&self, key: &usize) -> Option<ProbeResponse> {
        Some(ProbeResponse { rif: self.0[*key], latency_us: 0 })
    }
}

struct Hangs;

impl<K: Sync> Prober<K> for Hangs {
    async fn probe(&self, _: &K) -> Option<ProbeResponse> {
        pending().await
    }
}

async fn send<S: Service<(), Response = usize, Error = BoxError>>(svc: &mut S) -> Result<usize, BoxError> {
    std::future::poll_fn(|cx| svc.poll_ready(cx)).await?;
    svc.call(()).await
}

#[tokio::test]
async fn converges_on_least_loaded_replica() {
    let load = StaticLoad(vec![30, 25, 40, 0, 35, 50]);
    let mut balancer = PrequalBalance::from_services(svcs(6), load, Config::default());
    let mut hits = [0usize; 6];
    for _ in 0..400 {
        hits[send(&mut balancer).await.unwrap()] += 1;
        tokio::task::yield_now().await;
    }
    assert!(hits[3] > 300, "{hits:?}");
    let counts = balancer.probe_counts();
    assert_eq!(counts.failed + counts.timed_out, 0);
    assert!(counts.answered > 1_000, "{counts:?}");
}

#[tokio::test]
async fn syncing_the_balancers_handle_never_misroutes() {
    let mut balancer = PrequalBalance::from_services(svcs(3), Hangs, Config::default());
    send(&mut balancer).await.unwrap();
    balancer.handle().sync(&[0, 7]);
    let mut hits = [0usize; 3];
    for _ in 0..60 {
        hits[send(&mut balancer).await.unwrap()] += 1;
    }
    assert!(hits.iter().all(|&h| h > 0), "membership is restored from discovery: {hits:?}");
    assert_eq!(balancer.handle().len(), 3);
}

#[tokio::test]
async fn fast_failing_replica_is_ejected() {
    let mut services = svcs(4);
    services[2].fail = true;
    let mut balancer = PrequalBalance::from_services(services, StaticLoad(vec![20, 20, 0, 20]), Config::default());
    let mut failures = 0;
    for _ in 0..300 {
        failures += usize::from(send(&mut balancer).await.is_err());
        tokio::task::yield_now().await;
    }
    assert!(balancer.counters().ejections >= 1);
    assert!(failures < 30, "sinkholed {failures} of 300 requests");
}

#[tokio::test]
async fn classifier_turns_ok_responses_into_failures() {
    let mut config = Config::default();
    config.eject_after_failures = 1;
    let mut balancer = PrequalBalance::from_services(svcs(4), Hangs, config).with_classifier(|id: &usize| *id == 1);
    for _ in 0..50 {
        send(&mut balancer).await.unwrap();
    }
    assert!(balancer.counters().ejections >= 1);
}

#[tokio::test]
async fn piggybacked_reports_alone_steer_traffic() {
    let mut config = Config::default();
    (config.probes_per_query, config.removes_per_query) = (0.0, 0.25);
    let handle = PrequalHandle::new(config);
    let mut balancer = PrequalBalance::with_handle(handle.clone(), StaticList::new(svcs(4)), Hangs);
    let rif = [20, 30, 0, 25];
    let mut hits = [0usize; 4];
    for _ in 0..200 {
        let replica = send(&mut balancer).await.unwrap();
        hits[replica] += 1;
        handle.record(&replica, ProbeResponse { rif: rif[replica], latency_us: 0 });
    }
    let counters = balancer.counters();
    assert_eq!(hits.iter().max(), Some(&hits[2]), "{hits:?} {counters:?}");
    assert!(counters.random_fallbacks < counters.selections / 2, "{counters:?}");
    assert_eq!(balancer.probe_counts().sent, 0);
}

#[tokio::test]
async fn follows_discovery_inserts_and_removes() {
    let (tx, mut rx) = mpsc::unbounded_channel::<Change<&'static str, Svc>>();
    let discover = stream::poll_fn(move |cx| rx.poll_recv(cx).map(|c| c.map(Ok::<_, Infallible>)));
    let mut balancer = PrequalBalance::new(discover, Hangs, Config::default());
    tx.send(Change::Insert("a", Svc { id: 0, fail: false })).unwrap();
    tx.send(Change::Insert("b", Svc { id: 1, fail: false })).unwrap();
    for _ in 0..20 {
        send(&mut balancer).await.unwrap();
    }
    assert_eq!(balancer.len(), 2);
    tx.send(Change::Remove("a")).unwrap();
    for _ in 0..50 {
        assert_eq!(send(&mut balancer).await.unwrap(), 1);
    }
    tx.send(Change::Insert("c", Svc { id: 2, fail: false })).unwrap();
    let mut seen_c = false;
    for _ in 0..100 {
        seen_c |= send(&mut balancer).await.unwrap() == 2;
    }
    assert!(seen_c && balancer.len() == 2);
}

#[tokio::test(start_paused = true)]
async fn caps_outstanding_probes_and_times_them_out() {
    let mut balancer = PrequalBalance::from_services(svcs(10), Hangs, Config::default())
        .with_probe_timeout(Duration::from_millis(5))
        .with_max_in_flight_probes(4);
    for _ in 0..20 {
        send(&mut balancer).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let counts = balancer.probe_counts();
    assert!(counts.skipped > 0, "{counts:?}");
    assert!(counts.timed_out > 0, "{counts:?}");
    assert_eq!(counts.sent + counts.skipped, 60, "{counts:?}");
}
