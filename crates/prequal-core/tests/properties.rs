//! Property tests: random operation sequences keep the balancer's state consistent.

use prequal_core::{Config, Prequal, ProbeResponse};
use proptest::prelude::*;
use rand::{SeedableRng, rngs::SmallRng};

#[derive(Clone, Debug)]
enum Op {
    Add,
    Remove(usize),
    Probe { replica: usize, rif: u32, latency_us: u64 },
    Fail(usize),
    Succeed(usize),
    Select,
    Advance(u64),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        1 => Just(Op::Add),
        1 => any::<usize>().prop_map(Op::Remove),
        6 => (any::<usize>(), 0u32..200, 0u64..1_000_000)
            .prop_map(|(replica, rif, latency_us)| Op::Probe { replica, rif, latency_us }),
        2 => any::<usize>().prop_map(Op::Fail),
        2 => any::<usize>().prop_map(Op::Succeed),
        6 => Just(Op::Select),
        2 => (0u64..3_000_000).prop_map(Op::Advance),
    ]
}

fn config() -> impl Strategy<Value = Config> {
    (0.0f64..4.0, 1usize..32, 0.0f64..2.0, 0.0f64..=1.0, 0u32..6).prop_map(
        |(probes_per_query, pool_capacity, removes_per_query, q_rif, eject_after_failures)| {
            let mut config = Config::default();
            config.probes_per_query = probes_per_query;
            config.pool_capacity = pool_capacity;
            config.removes_per_query = removes_per_query;
            config.q_rif = q_rif;
            config.eject_after_failures = eject_after_failures;
            config
        },
    )
}

proptest! {
    /// Any interleaving of membership changes, probes, outcomes and selections keeps the state
    /// consistent: no panics, selections in range, pool within capacity, ejection capped.
    #[test]
    fn invariants_hold(config in config(), start in 1usize..12, ops in prop::collection::vec(op(), 1..400), seed: u64) {
        let mut rng = SmallRng::seed_from_u64(seed);
        let mut p = Prequal::new(config.clone(), start);
        let mut now = 0u64;
        let mut targets = Vec::new();
        for op in ops {
            let n = p.replicas();
            match op {
                Op::Add => { p.add_replica(); }
                Op::Remove(r) if n > 1 => p.swap_remove_replica(r % n),
                Op::Remove(_) => {}
                Op::Probe { replica, rif, latency_us } => {
                    p.record_probe(replica % (n + 1), ProbeResponse { rif, latency_us }, now, &mut rng);
                }
                Op::Fail(r) => p.record_failure(r % n, now),
                Op::Succeed(r) => p.record_success(r % n),
                Op::Select => {
                    p.probe_targets_into(&mut rng, &mut targets);
                    prop_assert!(targets.iter().all(|&t| t < n));
                    let mut unique = targets.clone();
                    unique.sort_unstable();
                    unique.dedup();
                    prop_assert_eq!(unique.len(), targets.len());
                    let chosen = p.select(now, &mut rng);
                    prop_assert!(chosen < n);
                    let any_eligible = (0..n).any(|r| !p.is_ejected(r, now));
                    prop_assert!(!any_eligible || !p.is_ejected(chosen, now), "picked ejected {chosen} with healthy replicas left");
                }
                Op::Advance(dt) => now += dt,
            }
            prop_assert!(p.pool_len() <= config.pool_capacity.max(1));
        }
    }
}
