use std::{net::SocketAddr, sync::Mutex};

use super::{Candidate, rendezvous};

/// llm-d's no-hit-lru scorer: a request no replica holds any prefix of ("cold") prefers the replica that least
/// recently took a cold request, so new prefixes spread evenly instead of wherever a hash or a momentary load puts
/// them.
#[derive(Debug, Default)]
pub struct ColdLru {
    /// Replicas in order of their last cold request, oldest first.
    order: Mutex<Vec<SocketAddr>>,
}

impl ColdLru {
    pub fn is_cold(candidates: &[Candidate]) -> bool {
        candidates.iter().all(|c| c.matched_blocks == 0)
    }

    /// Per candidate, 1 for the next in line (never-used replicas first, in candidate order) falling linearly to 0.
    pub fn scores(&self, candidates: &[Candidate]) -> Vec<f64> {
        let order = self.order.lock().unwrap_or_else(|p| p.into_inner());
        let never_used: Vec<usize> =
            (0..candidates.len()).filter(|&i| !order.contains(&candidates[i].replica.addr)).collect();
        let span = (candidates.len() - 1).max(1) as f64;
        (0..candidates.len())
            .map(|i| {
                let rank = match order.iter().position(|a| *a == candidates[i].replica.addr) {
                    Some(pos) => never_used.len() + pos,
                    None => never_used.iter().position(|&j| j == i).expect("never used"),
                };
                (1.0 - rank as f64 / span).max(0.0)
            })
            .collect()
    }

    /// Per candidate, 1 for `head`'s first rendezvous choice falling linearly to 0 for its last: a cold placement
    /// every router agrees on without shared state.
    pub fn hash_scores(head: u64, candidates: &[Candidate]) -> Vec<f64> {
        let keys: Vec<u64> = candidates.iter().map(|c| rendezvous(head, &c.replica.addr)).collect();
        let span = (candidates.len() - 1).max(1) as f64;
        keys.iter().map(|k| 1.0 - keys.iter().filter(|&o| o > k).count() as f64 / span).collect()
    }

    pub fn record(&self, addr: SocketAddr) {
        let mut order = self.order.lock().unwrap_or_else(|p| p.into_inner());
        order.retain(|a| *a != addr);
        order.push(addr);
    }
}
