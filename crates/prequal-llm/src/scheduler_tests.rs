use std::sync::atomic::Ordering;

use super::*;
use crate::{
    policy::by_name,
    prompt::{BLOCK_BYTES, Prompt},
};

fn addrs(n: u16) -> Vec<SocketAddr> {
    (1..=n).map(|p| SocketAddr::from(([127, 0, 0, 1], p))).collect()
}

fn prompt(group: u8, question: u8) -> Prompt {
    let mut text = vec![b'a' + group; BLOCK_BYTES * 20];
    text.extend(vec![b'A' + question; BLOCK_BYTES * 4]);
    Prompt::from_text(&text)
}

fn idle(scheduler: &Scheduler) {
    for addr in scheduler.addrs() {
        scheduler.observe(addr, EngineStats::default());
    }
}

#[test]
fn reservations_are_released_by_first_token_and_drop() {
    let scheduler = Scheduler::new(by_name("prequal").unwrap());
    scheduler.sync(addrs(2));
    idle(&scheduler);
    let mut ticket = scheduler.route(&prompt(0, 0), 100, |_| true).unwrap();
    let pending =
        |s: &Scheduler| s.state().replicas.iter().map(|r| (r.pending_prefill_tokens, r.active)).collect::<Vec<_>>();
    assert!(pending(&scheduler).iter().any(|&(tokens, active)| tokens > 0 && active == 1));
    ticket.first_token();
    assert!(pending(&scheduler).iter().all(|&(tokens, _)| tokens == 0));
    drop(ticket);
    assert!(pending(&scheduler).iter().all(|&p| p == (0, 0)));
}

#[test]
fn replicas_reports_scrapes_down_marks_and_in_flight() {
    let scheduler = Scheduler::new(by_name("least-request").unwrap());
    let all = addrs(2);
    scheduler.sync(all.clone());
    scheduler.observe(all[0], EngineStats { waiting: 3.0, ..EngineStats::default() });
    scheduler.mark_down(all[1]);
    let ticket = scheduler.route(&prompt(0, 0), 10, |_| true).unwrap();
    let status = scheduler.replicas();
    assert_eq!(status.iter().map(|r| r.addr).collect::<Vec<_>>(), all);
    assert_eq!(status[0].scrape.map(|(stats, _)| stats.waiting), Some(3.0));
    assert_eq!((status[0].down, status[0].in_flight), (false, 1));
    assert_eq!((status[1].scrape, status[1].down, status[1].in_flight), (None, true, 0));
    drop(ticket);
    assert_eq!(scheduler.replicas()[0].in_flight, 0);
}

fn prefilling(scheduler: &Scheduler) -> u32 {
    scheduler.state().replicas.iter().map(|r| r.prefilling).sum()
}

#[test]
fn prefill_signal_decides_what_ends_the_reservation() {
    use PrefillSignal::*;
    for (signal, after_headers, after_chunk) in [(FirstChunk, 1, 0), (Headers, 0, 0), (End, 1, 1)] {
        let scheduler = Scheduler::new(by_name("prequal").unwrap()).with_prefill_signal(signal);
        scheduler.sync(addrs(2));
        idle(&scheduler);
        let mut ticket = scheduler.route(&prompt(0, 0), 10, |_| true).unwrap();
        ticket.response_started();
        assert_eq!(prefilling(&scheduler), after_headers, "{signal}");
        ticket.first_token();
        assert_eq!(prefilling(&scheduler), after_chunk, "{signal}");
        drop(ticket);
        assert_eq!(prefilling(&scheduler), 0, "{signal}");
    }
    let scheduler = Scheduler::new(by_name("prequal").unwrap()).with_prefill_signal(Scrape);
    scheduler.sync(addrs(1));
    let ticket = scheduler.route(&prompt(0, 0), 10, |_| true).unwrap();
    assert_eq!(prefilling(&scheduler), 1);
    idle(&scheduler);
    assert_eq!(prefilling(&scheduler), 0, "the next scrape sees the request itself");
    drop(ticket);
    assert_eq!("estimate:2000".parse(), Ok(Estimate { tokens_per_sec: 2000.0 }));
    assert!("estimate:0".parse::<PrefillSignal>().is_err());
}

#[test]
fn estimated_prefills_queue_and_expire() {
    let micros = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let start = Instant::now();
    let clock = Arc::clone(&micros);
    let scheduler = Scheduler::new(by_name("prequal").unwrap())
        .with_clock(Arc::new(move || start + std::time::Duration::from_micros(clock.load(Ordering::Relaxed))))
        .with_prefill_signal(PrefillSignal::Estimate { tokens_per_sec: 1e6 });
    scheduler.sync(addrs(1));
    idle(&scheduler);
    let first = scheduler.route(&prompt(0, 0), 10, |_| true).unwrap();
    let second = scheduler.route(&prompt(0, 1), 10, |_| true).unwrap();
    micros.store(first.uncached_tokens(), Ordering::Relaxed);
    let third = scheduler.route(&prompt(0, 2), 10, |_| true).unwrap();
    assert_eq!(prefilling(&scheduler), 2, "the first prefill is estimated done; the second waits behind it");
    drop(second);
    assert_eq!(prefilling(&scheduler), 1);
    drop((first, third));
    assert_eq!(prefilling(&scheduler), 0);
}

#[test]
fn prequal_keeps_a_group_on_its_replica_until_it_queues() {
    let scheduler = Scheduler::new(by_name("prequal").unwrap());
    scheduler.sync(addrs(4));
    idle(&scheduler);
    let first = scheduler.route(&prompt(1, 0), 10, |_| true).unwrap();
    let home = first.addr();
    drop(first);
    let again = scheduler.route(&prompt(1, 1), 10, |_| true).unwrap();
    assert_eq!(again.addr(), home, "shared 20-block prefix is cheaper than recomputing it");

    let mut held = vec![again];
    held[0].first_token();
    for q in 2..120 {
        let mut ticket = scheduler.route(&prompt(1, q), 10, |_| true).unwrap();
        ticket.first_token();
        held.push(ticket);
    }
    assert!(held.iter().all(|t| t.addr() == home), "unqueued, even a hot prefix stays home");
    let pending = scheduler.route(&prompt(1, 120), 10, |_| true).unwrap();
    assert_eq!(pending.addr(), home);
    let next = scheduler.route(&prompt(1, 121), 10, |_| true).unwrap();
    assert_ne!(next.addr(), home, "a hot prefix spills while this router's last request there awaits its first token");
    drop((pending, next));
    scheduler.observe(home, EngineStats { waiting: 8.0, ..EngineStats::default() });
    let next = scheduler.route(&prompt(1, 120), 10, |_| true).unwrap();
    assert_ne!(next.addr(), home, "a hot prefix spills once its home queues");
}

#[test]
fn independent_routers_agree_on_a_new_prefix_home() {
    let routers: Vec<Scheduler> = (0..2).map(|_| Scheduler::new(by_name("prequal").unwrap())).collect();
    for router in &routers {
        router.sync(addrs(8));
        idle(router);
    }
    let homes: HashSet<_> = (0..20u8)
        .map(|group| {
            let picks: Vec<SocketAddr> =
                routers.iter().map(|r| r.route(&prompt(group, 0), 10, |_| true).unwrap().addr()).collect();
            assert_eq!(picks[0], picks[1], "group {group}");
            picks[0]
        })
        .collect();
    assert!(homes.len() >= 5, "different prefixes spread: {homes:?}");
}

#[test]
fn a_system_prompt_every_replica_holds_neither_heats_nor_identifies_prompts() {
    let scheduler = Scheduler::new(by_name("prequal").unwrap());
    let all = addrs(4);
    scheduler.sync(all.clone());
    idle(&scheduler);
    let system = vec![b's'; BLOCK_BYTES * 2];
    let held = Prompt::from_text(&system).blocks;
    scheduler.state().replicas.iter_mut().for_each(|r| r.cache.touch(&held));
    let turn = |conversation: u8, turns: usize| {
        let mut text = system.clone();
        text.extend(vec![b'a' + conversation; BLOCK_BYTES * 30]);
        (0..turns).for_each(|t| text.extend(vec![b'0' + t as u8; BLOCK_BYTES]));
        Prompt::from_text(&text)
    };
    let homes: Vec<SocketAddr> = (0..20).map(|c| scheduler.route(&turn(c, 1), 10, |_| true).unwrap().addr()).collect();
    assert!(homes.iter().collect::<HashSet<_>>().len() >= 3, "new conversations spread: {homes:?}");
    for (c, &home) in homes.iter().enumerate() {
        all.iter()
            .for_each(|&a| scheduler.observe(a, EngineStats { waiting: f64::from(a == home), ..Default::default() }));
        let next = scheduler.route(&turn(c as u8, 2), 10, |_| true).unwrap();
        assert_eq!(next.addr(), home, "conversation {c} keeps its replica: the shared system prompt isn't hot");
    }
}

#[tokio::test]
async fn acquire_waits_for_room_in_fifo_order() {
    let scheduler = Scheduler::new(by_name("prequal").unwrap()).with_admission_limit(1);
    scheduler.sync(addrs(1));
    idle(&scheduler);
    let mut first = scheduler.acquire(&prompt(0, 0), 10, |_| true).await.unwrap();
    let waiting = tokio::spawn({
        let scheduler = scheduler.clone();
        async move { scheduler.acquire(&prompt(0, 1), 10, |_| true).await.map(|t| t.addr()) }
    });
    let cancelled = tokio::spawn({
        let scheduler = scheduler.clone();
        async move { scheduler.acquire(&prompt(0, 2), 10, |_| true).await.is_some() }
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished(), "no room until the first request reaches its first token");
    cancelled.abort();
    first.first_token();
    assert_eq!(waiting.await.unwrap(), Some(addrs(1)[0]));
    assert_eq!(scheduler.state().queued, 0, "routed and cancelled waiters both left the queue");
    assert!(scheduler.acquire(&prompt(0, 3), 10, |_| false).await.is_none(), "nothing eligible never waits");
}

#[test]
fn routes_only_to_allowed_and_prefers_up_replicas() {
    let scheduler = Scheduler::new(by_name("least-request").unwrap());
    let all = addrs(3);
    scheduler.sync(all.clone());
    scheduler.mark_down(all[0]);
    for _ in 0..10 {
        let ticket = scheduler.route(&prompt(0, 0), 1, |a| *a != all[2]).unwrap();
        assert_eq!(ticket.addr(), all[1]);
    }
    assert!(scheduler.route(&prompt(0, 0), 1, |_| false).is_none());
    let only_down = scheduler.route(&prompt(0, 0), 1, |a| *a == all[0]).unwrap();
    assert_eq!(only_down.addr(), all[0], "a down replica still beats rejecting the request");
}

#[test]
fn llmd_optimized_rotates_cold_prefixes_and_keeps_warm_ones() {
    let scheduler = Scheduler::new(by_name("llmd-optimized").unwrap());
    scheduler.sync(addrs(3));
    idle(&scheduler);
    let homes: Vec<SocketAddr> = (0..3).map(|g| scheduler.route(&prompt(g, 0), 10, |_| true).unwrap().addr()).collect();
    assert_eq!(homes.iter().collect::<HashSet<_>>().len(), 3, "cold groups go to the least recently used replica");
    assert_eq!(scheduler.route(&prompt(1, 1), 10, |_| true).unwrap().addr(), homes[1], "a warm group stays home");
}

#[test]
fn settings_after_cloning_apply_to_every_clone() {
    let scheduler = Scheduler::new(by_name("prequal").unwrap());
    let clone = scheduler.clone();
    let scheduler = scheduler.with_prefill_signal(PrefillSignal::Headers).with_seed(7).with_admission_limit(2);
    assert_eq!(clone.state().signal, PrefillSignal::Headers);
    assert_eq!(clone.state().admission_limit, Some(2.0));
    scheduler.sync(addrs(1));
    let mut ticket = clone.route(&prompt(0, 0), 10, |_| true).unwrap();
    ticket.response_started();
    assert_eq!(prefilling(&scheduler), 0, "the clone routed under the new signal");
}

#[test]
fn non_finite_scrapes_are_sanitized_before_routing() {
    let scheduler = Scheduler::new(by_name("prequal").unwrap());
    let all = addrs(2);
    scheduler.sync(all.clone());
    let poisoned = EngineStats { running: f64::INFINITY, waiting: f64::NAN, kv_usage: f64::NAN, ..Default::default() };
    scheduler.observe(all[0], poisoned);
    scheduler.observe(all[1], EngineStats::default());
    assert_eq!(scheduler.state().replicas[0].stats, Some(EngineStats::default()));
    for q in 0..10 {
        drop(scheduler.route(&prompt(q, q), 10, |_| true).unwrap());
    }
    assert!(scheduler.state().replicas.iter().all(|r| r.smoothed_load.is_finite()));
    assert_eq!(scheduler.up_count(), 2);
    scheduler.mark_down(all[1]);
    assert_eq!(scheduler.up_count(), 1);
}

struct HeldBy(SocketAddr);

impl ExactIndex for HeldBy {
    fn matched_blocks(&self, addr: SocketAddr, prompt: &Prompt, _: usize) -> usize {
        if addr == self.0 { prompt.blocks.len() } else { 0 }
    }
}

#[test]
fn exact_index_and_peer_routes_say_where_a_prompt_is_cached() {
    for holder in addrs(4) {
        let exact = Scheduler::new(by_name("prequal").unwrap()).with_exact_index(Arc::new(HeldBy(holder)));
        let told = Scheduler::new(by_name("prequal").unwrap());
        for scheduler in [&exact, &told] {
            scheduler.sync(addrs(4));
            idle(scheduler);
        }
        told.observe_peer_route(holder, &prompt(0, 0));
        assert_eq!(exact.route(&prompt(0, 0), 10, |_| true).unwrap().addr(), holder);
        assert_eq!(told.route(&prompt(0, 0), 10, |_| true).unwrap().addr(), holder);
    }
}

#[test]
fn every_named_policy_routes() {
    for name in crate::policy::NAMES {
        let scheduler = Scheduler::new(by_name(name).unwrap());
        scheduler.sync(addrs(3));
        idle(&scheduler);
        let tickets: Vec<_> = (0..20).map(|q| scheduler.route(&prompt(q % 3, q), 50, |_| true).unwrap()).collect();
        assert_eq!(tickets.len(), 20, "{name}");
    }
    assert!(by_name("nope").is_none());
}
