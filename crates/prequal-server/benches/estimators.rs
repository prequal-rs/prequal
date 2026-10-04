//! Per-request cost of each latency estimator.

use divan::{Bencher, black_box};
use prequal_server::{LatencyEstimator, ProbeState, RecentMedian, RifOnly, ServiceTimeModel};

fn main() {
    divan::main();
}

fn filled<E: LatencyEstimator>(estimator: E) -> E {
    for i in 0..2_000u64 {
        estimator.record((i % 24) as u32, 1_000 + (i * 7919) % 50_000, i * 500);
    }
    estimator
}

/// Cost of answering one probe with a populated estimator.
#[divan::bench(types = [RifOnly, RecentMedian, ServiceTimeModel])]
fn probe<E: LatencyEstimator + Default>(bencher: Bencher) {
    let state = ProbeState::new(filled(E::default()));
    let _in_flight: Vec<_> = (0..12).map(|_| state.start()).collect();
    bencher.bench_local(|| black_box(state.probe()));
}

/// Cost the request path pays: start + finish of one request.
#[divan::bench(types = [RifOnly, RecentMedian, ServiceTimeModel])]
fn request<E: LatencyEstimator + Default>(bencher: Bencher) {
    let state = ProbeState::new(filled(E::default()));
    bencher.bench_local(|| drop(black_box(state.start())));
}
