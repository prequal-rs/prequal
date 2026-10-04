//! Selection and probe-recording cost.

use divan::{Bencher, black_box};
use prequal_core::{Config, Prequal, ProbeResponse};
use rand::{RngExt, SeedableRng, rngs::SmallRng};

fn main() {
    divan::main();
}

fn warmed(replicas: usize, rng: &mut SmallRng) -> Prequal {
    let mut p = Prequal::new(Config::default(), replicas);
    for now in 0..1_000 {
        query(&mut p, rng, now);
    }
    p
}

fn query(p: &mut Prequal, rng: &mut SmallRng, now: u64) -> usize {
    let mut targets = Vec::with_capacity(4);
    p.probe_targets_into(rng, &mut targets);
    for r in targets {
        let response = ProbeResponse { rif: rng.random_range(0..40), latency_us: rng.random_range(0..50_000) };
        p.record_probe(r, response, now, rng);
    }
    p.select(now, rng)
}

/// One full query: probe-target sampling, three probe responses recorded, one selection.
#[divan::bench(args = [10, 100, 1000])]
fn full_query(bencher: Bencher, replicas: usize) {
    let mut rng = SmallRng::seed_from_u64(1);
    let mut p = warmed(replicas, &mut rng);
    let mut now = 1_000;
    bencher.bench_local(|| {
        now += 1;
        black_box(query(&mut p, &mut rng, now))
    });
}

#[divan::bench]
fn select_only(bencher: Bencher) {
    let mut rng = SmallRng::seed_from_u64(1);
    let mut p = warmed(100, &mut rng);
    bencher.bench_local(|| {
        p.record_probe(3, ProbeResponse { rif: 5, latency_us: 900 }, 2_000, &mut rng);
        black_box(p.select(2_000, &mut rng))
    });
}
