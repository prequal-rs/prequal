//! [`Scheduler::replicas`]: a read-only snapshot of per-replica state, for metrics, health and shedding.

use std::{net::SocketAddr, time::Instant};

use super::Scheduler;
use crate::engine::EngineStats;

/// One replica as the scheduler currently sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ReplicaStatus {
    /// The replica's address.
    pub addr: SocketAddr,
    /// Its latest good scrape and when it was applied (scheduler clock); `None` until the first.
    pub scrape: Option<(EngineStats, Instant)>,
    /// Marked down by a failed scrape or request: routed to only if nothing else is left, until its next good scrape.
    pub down: bool,
    /// Requests this scheduler routed here whose [`Ticket`](crate::Ticket)s have not ended.
    pub in_flight: u32,
}

impl Scheduler {
    /// Every replica's current status, in [`Scheduler::addrs`] order.
    #[must_use]
    pub fn replicas(&self) -> Vec<ReplicaStatus> {
        self.state()
            .replicas
            .iter()
            .map(|r| ReplicaStatus {
                addr: r.addr,
                scrape: r.stats.zip(r.scraped_at),
                down: r.down,
                in_flight: r.active,
            })
            .collect()
    }
}
