use std::{net::SocketAddr, time::Instant};

use super::*;
use crate::{
    engine::EngineStats,
    fleet::Replica,
    policy::{NAMES, by_name},
    prompt::Prompt,
};

fn addr(i: usize) -> SocketAddr {
    SocketAddr::from(([10, 0, 0, 1], 8000 + i as u16))
}

/// Three replicas: blocks of the prompt each holds, scraped queue, sustained load, own requests awaiting first token;
/// and where the prompt's key last went.
#[derive(Default)]
struct Fleet {
    matched: [usize; 3],
    waiting: [f64; 3],
    load: [f64; 3],
    prefilling: [u32; 3],
    home: Option<usize>,
}

impl Fleet {
    fn held_by_first() -> Self {
        Self { matched: [100, 0, 0], ..Self::default() }
    }

    fn pick(&self, policy: &Prequal, heat_share: f64) -> usize {
        let prompt = Prompt { blocks: (0..100).collect(), tokens: 6_400 };
        let replicas: Vec<Replica> = (0..3)
            .map(|i| {
                let mut r = Replica::new(addr(i));
                r.stats = Some(EngineStats { waiting: self.waiting[i], ..EngineStats::default() });
                r.smoothed_load = self.load[i];
                r.prefilling = self.prefilling[i];
                r
            })
            .collect();
        let candidates: Vec<Candidate> = replicas
            .iter()
            .zip(self.matched)
            .map(|(replica, matched_blocks)| Candidate { replica, matched_blocks })
            .collect();
        let request = Request {
            prompt: &prompt,
            output_tokens: 1_000,
            typical_demand_tokens: 2_000.0,
            heat_share,
            home: self.home.map(addr),
            key: 0,
            now: std::time::Instant::now(),
        };
        policy.pick(&request, &candidates, &mut PolicyRng::seed_from_u64(1))
    }
}

fn pick(waiting: [f64; 3], heat_share: f64) -> usize {
    Fleet { waiting, ..Fleet::held_by_first() }.pick(&Prequal::default(), heat_share)
}

#[test]
fn affinity_yields_only_to_queue_or_heat() {
    assert_eq!(pick([0.0, 0.0, 0.0], 0.01), 0, "the holder wins on an idle fleet");
    assert_eq!(pick([4.0, 0.0, 2.0], 0.01), 0, "a busier holder still wins: prefix outweighs queue");
    assert_eq!(pick([0.0, 0.0, 0.0], 0.9), 0, "even a hot prefix stays while its holder is idle");
    assert_ne!(pick([2.0, 0.0, 0.0], 0.9), 0, "a hot prefix spreads once its holder queues");
}

#[test]
fn a_holder_sheds_only_far_past_the_fleets_sustained_load() {
    let with_load = |load| Fleet { load, ..Fleet::held_by_first() }.pick(&Prequal::default(), 0.01);
    assert_eq!(with_load([5.0, 2.0, 2.0]), 0, "ordinary imbalance keeps affinity");
    assert_ne!(with_load([30.0, 2.0, 2.0]), 0, "an overloaded holder sheds");
}

#[test]
fn a_non_finite_gauge_on_one_replica_never_panics_any_policy() {
    type Poison = fn(&mut EngineStats, f64);
    let gauges: [(&str, Poison); 3] =
        [("running", |s, v| s.running = v), ("waiting", |s, v| s.waiting = v), ("kv_usage", |s, v| s.kv_usage = v)];
    let prompt = Prompt { blocks: (0..10).collect(), tokens: 640 };
    let request = Request {
        prompt: &prompt,
        output_tokens: 10,
        typical_demand_tokens: 100.0,
        heat_share: 0.5,
        home: Some(SocketAddr::from(([10, 0, 0, 1], 8000))),
        key: 0,
        now: Instant::now(),
    };
    for (gauge, poison) in gauges {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for matched in [[0, 0, 0], [10, 10, 0]] {
                let replicas: Vec<Replica> = (0..3u16)
                    .map(|i| {
                        let mut r = Replica::new(SocketAddr::from(([10, 0, 0, 1], 8000 + i)));
                        let mut stats = EngineStats { running: 1.0, waiting: 1.0, kv_usage: 0.5, ..Default::default() };
                        if i == 0 {
                            poison(&mut stats, bad);
                            r.smoothed_load = bad;
                        }
                        r.stats = Some(stats);
                        r
                    })
                    .collect();
                let candidates: Vec<Candidate> = replicas
                    .iter()
                    .zip(matched)
                    .map(|(replica, matched_blocks)| Candidate { replica, matched_blocks })
                    .collect();
                let variants = ["prequal-rif", "prequal-fresh", "prequal-scraped", "prequal-home"];
                for name in NAMES.iter().copied().chain(variants) {
                    let chosen = by_name(name).unwrap().pick(&request, &candidates, &mut PolicyRng::seed_from_u64(1));
                    assert!(chosen < 3, "{name} with {gauge} = {bad}");
                }
                let alone =
                    by_name("prequal").unwrap().pick(&request, &candidates[..1], &mut PolicyRng::seed_from_u64(1));
                assert_eq!(alone, 0, "a lone poisoned replica with {gauge} = {bad}");
            }
        }
    }
}

#[test]
fn own_unscraped_prefills_spread_warm_prompts_but_not_cold_ones() {
    let shared = |prefilling| Fleet { matched: [100, 100, 0], prefilling, ..Fleet::default() };
    let policy = Prequal::default();
    let (a, b) = (shared([3, 0, 0]).pick(&policy, 0.01), shared([0, 3, 0]).pick(&policy, 0.01));
    assert_eq!((a, b), (1, 0), "between two holders, the one without this router's pending prefills");
    let cold = Fleet { prefilling: [1, 0, 0], ..Fleet::default() };
    assert_eq!(cold.pick(&Prequal::default(), 0.01), 0, "cold placement follows the LRU, not own prefills");
    let always = Prequal { fresh: Fresh::Always, ..Prequal::default() };
    assert_ne!(cold.pick(&always, 0.01), 0, "fresh load on cold prompts overrides the LRU");
}

#[test]
fn among_several_routers_a_cold_prompt_keeps_its_hash_home_despite_a_queued_request() {
    let home = (0..3).max_by_key(|&i| rendezvous(0, &addr(i))).unwrap();
    let mut waiting = [0.0; 3];
    waiting[home] = 1.0;
    let busy_home = Fleet { waiting, ..Fleet::default() };
    assert_eq!(busy_home.pick(&Prequal::default(), 0.01), home, "other routers' load: placement goes by hash");
    let weak = Prequal { hash_weight: 2.0, ..Prequal::default() };
    assert_ne!(busy_home.pick(&weak, 0.01), home, "at the LRU's weight one queued request moves it");
}

#[test]
fn prequal_home_returns_a_forgotten_prompt_to_an_uncalibrated_replica() {
    let forgotten = Fleet { home: Some(2), ..Fleet::default() };
    assert_eq!(forgotten.pick(&Prequal::default(), 0.01), 0, "by default the cold LRU decides");
    assert_eq!(forgotten.pick(&Prequal { home: true, ..Prequal::default() }, 0.01), 2);
}
