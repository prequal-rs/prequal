//! Balancer behaviour through the public API.

use prequal_core::{Config, Prequal, ProbeResponse};
use rand::{SeedableRng, rngs::SmallRng};

fn rng() -> SmallRng {
    SmallRng::seed_from_u64(7)
}

fn config(set: impl FnOnce(&mut Config)) -> Config {
    let mut config = Config::default();
    set(&mut config);
    config
}

#[test]
fn probe_targets_are_distinct_and_rate_limited() {
    let mut rng = rng();
    let mut p = Prequal::new(config(|c| c.probes_per_query = 2.5), 10);
    let counts: Vec<_> = (0..4).map(|_| p.probe_targets(&mut rng)).collect();
    assert_eq!(counts.iter().map(Vec::len).collect::<Vec<_>>(), [2, 3, 2, 3]);
    for targets in &counts {
        let mut sorted = targets.clone();
        sorted.dedup();
        assert_eq!(sorted.len(), targets.len());
    }
    let mut tiny = Prequal::new(Config::default(), 2);
    assert_eq!(tiny.probe_targets(&mut rng).len(), 2);
}

#[test]
fn falls_back_to_random_with_sparse_pool() {
    let mut rng = rng();
    let mut p = Prequal::new(Config::default(), 5);
    p.record_probe(3, ProbeResponse { rif: 0, latency_us: 0 }, 0, &mut rng);
    for _ in 0..50 {
        assert!(p.select(0, &mut rng) < 5);
    }
}

#[test]
fn routes_away_from_loaded_replicas() {
    let mut rng = rng();
    let config = config(|c| c.removes_per_query = 0.0);
    let mut p = Prequal::new(config, 4);
    let rif = [40, 1, 35, 50];
    let mut hits = [0usize; 4];
    for now in 0..200u64 {
        for r in p.probe_targets(&mut rng) {
            let response = ProbeResponse { rif: rif[r], latency_us: u64::from(rif[r]) * 100 };
            p.record_probe(r, response, now, &mut rng);
        }
        hits[p.select(now, &mut rng)] += 1;
    }
    assert!(hits[1] > 150, "{hits:?}");
}

#[test]
fn stale_probes_expire() {
    let mut rng = rng();
    let mut p = Prequal::new(config(|c| c.max_age_us = 10), 4);
    for r in 0..4 {
        p.record_probe(r, ProbeResponse::default(), 0, &mut rng);
    }
    p.select(100, &mut rng);
    assert_eq!(p.pool_len(), 0);
}

#[test]
fn swap_remove_keeps_probes_for_moved_replica() {
    let mut rng = rng();
    let config = config(|c| c.removes_per_query = 0.0);
    let mut p = Prequal::new(config, 4);
    for (r, rif) in [(0, 9), (1, 9), (3, 0)] {
        p.record_probe(r, ProbeResponse { rif, latency_us: 0 }, 0, &mut rng);
    }
    p.swap_remove_replica(0);
    assert_eq!(p.replicas(), 3);
    assert_eq!(p.pool_len(), 2);
    assert_eq!(p.select(0, &mut rng), 0, "old replica 3 now lives at index 0");
    assert_eq!(p.add_replica(), 3);
}

#[test]
fn fast_failing_replica_is_ejected_and_avoided() {
    let mut rng = rng();
    let config = config(|c| c.removes_per_query = 0.0);
    let mut p = Prequal::new(config.clone(), 4);
    for _ in 0..config.eject_after_failures {
        p.record_failure(2, 0);
    }
    assert!(p.is_ejected(2, 0));
    assert_eq!(p.counters().ejections, 1);
    for now in 0..500 {
        p.record_probe(2, ProbeResponse { rif: 0, latency_us: 0 }, now, &mut rng);
        p.record_probe(now as usize % 2, ProbeResponse { rif: 5, latency_us: 0 }, now, &mut rng);
        assert_ne!(p.select(now, &mut rng), 2);
    }
    assert!(!p.is_ejected(2, config.base_ejection_us));
}

#[test]
fn routes_even_when_every_remaining_replica_is_ejected() {
    let mut rng = rng();
    let config = config(|c| c.eject_after_failures = 1);
    let mut p = Prequal::new(config, 2);
    p.record_failure(0, 0);
    p.swap_remove_replica(1);
    assert!(p.is_ejected(0, 0));
    assert_eq!(p.select(0, &mut rng), 0);
}

#[test]
fn select_where_respects_the_allowed_subset() {
    let mut rng = rng();
    let config = config(|c| c.removes_per_query = 0.0);
    let mut p = Prequal::new(config, 5);
    for (r, rif) in [(0, 0), (1, 9), (2, 7), (3, 8), (4, 1)] {
        p.record_probe(r, ProbeResponse { rif, latency_us: 0 }, 0, &mut rng);
    }
    for _ in 0..50 {
        let chosen = p.select_where(0, &mut rng, |r| r == 2 || r == 3).unwrap();
        assert!(chosen == 2 || chosen == 3);
    }
    assert_eq!(p.select_where(0, &mut rng, |_| false), None);
}

#[test]
fn counts_random_fallbacks() {
    let mut rng = rng();
    let mut p = Prequal::new(Config::default(), 3);
    p.select(0, &mut rng);
    let c = p.counters();
    assert_eq!((c.selections, c.random_fallbacks), (1, 1));
}
