use std::{collections::HashSet, net::SocketAddr, time::Duration};

use futures_util::{StreamExt, stream::FuturesUnordered};

use crate::{EngineProber, Scheduler};

/// Slower scrapes count as failures. Busy engines answer `/metrics` late, which is load, not death.
const SCRAPE_TIMEOUT: Duration = Duration::from_millis(500);

/// Scrapes every replica's metrics each `interval` (llm-d uses 50 ms) and feeds them to the scheduler; a scrape
/// that fails or exceeds `max(500 ms, interval)` marks the replica down until the next good one. A slow replica
/// skips ticks while its scrape is outstanding without delaying the others'. Runs until dropped.
pub async fn scrape_forever(scheduler: Scheduler, prober: EngineProber, interval: Duration) {
    let timeout = SCRAPE_TIMEOUT.max(interval);
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut outstanding: HashSet<SocketAddr> = HashSet::new();
    let mut scrapes = FuturesUnordered::new();
    let scrape = |addr: SocketAddr| {
        let prober = &prober;
        async move { (addr, tokio::time::timeout(timeout, prober.stats(addr)).await.ok().flatten()) }
    };
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                for addr in scheduler.addrs() {
                    if outstanding.insert(addr) {
                        scrapes.push(scrape(addr));
                    }
                }
            }
            Some((addr, stats)) = scrapes.next(), if !scrapes.is_empty() => {
                outstanding.remove(&addr);
                match stats {
                    Some(stats) => scheduler.observe(addr, stats),
                    None => scheduler.mark_down(addr),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::{Engine, policy};

    #[tokio::test]
    async fn a_dead_replica_is_marked_down_without_stalling_the_loop() {
        let scheduler = Scheduler::new(policy::by_name("prequal").unwrap());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = listener.local_addr().unwrap();
        drop(listener);
        scheduler.sync([dead]);
        let started = Instant::now();
        let run = scrape_forever(scheduler.clone(), EngineProber::new(Engine::Vllm), Duration::from_millis(10));
        // Windows retries refused loopback connects for ~2 s, so this may take the 500 ms timeout path.
        let _ = tokio::time::timeout(Duration::from_millis(1_500), run).await;
        assert_eq!(scheduler.up_count(), 0, "a refused or timed-out scrape marks it down");
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
