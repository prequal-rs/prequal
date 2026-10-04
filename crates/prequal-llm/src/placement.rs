//! One routing decision: the policy's view of the fleet, its pick, and the reservation on the chosen replica.

use std::{net::SocketAddr, sync::Arc};

use super::{Placement, Scheduler, State};
use crate::{
    fleet::Estimate,
    heat::HeadStats,
    policy::{Candidate, Request},
    prompt::Prompt,
    ticket::{PrefillSignal, Ticket},
};

impl Scheduler {
    /// Places a request among the `allowed` replicas with fewer than `limit` requests awaiting first token. The first
    /// attempt counts it toward its prompt key's heat into `heat`; retries (admission) reuse that.
    pub(super) fn place(
        &self,
        state: &mut State,
        prompt: &Prompt,
        output_tokens: u64,
        allowed: &dyn Fn(&SocketAddr) -> bool,
        limit: Option<f64>,
        heat: &mut Option<HeadStats>,
    ) -> Placement {
        let eligible: Vec<usize> = (0..state.replicas.len()).filter(|&i| allowed(&state.replicas[i].addr)).collect();
        let up: Vec<usize> = eligible.iter().copied().filter(|&i| !state.replicas[i].down).collect();
        let slots = if up.is_empty() { eligible } else { up };
        if slots.is_empty() {
            return Placement::Unavailable;
        }
        let slots: Vec<usize> = match limit {
            Some(limit) => slots.into_iter().filter(|&i| state.replicas[i].admission_backlog() < limit).collect(),
            None => slots,
        };
        if slots.is_empty() {
            return Placement::Full;
        }
        let now = state.now();
        let signal = state.signal;
        let estimating = matches!(signal, PrefillSignal::Estimate { .. });
        slots.iter().for_each(|&i| {
            if estimating {
                state.replicas[i].expire_estimates(now);
            }
            state.replicas[i].sample_load(now);
        });
        let matched_blocks = |i: usize| {
            let replica = &state.replicas[i];
            let approximate = replica.matched_blocks(prompt);
            let Some(exact) = &state.exact else { return approximate };
            exact.matched_blocks(replica.addr, prompt, approximate).min(prompt.blocks.len())
        };
        let matched: Vec<usize> = slots.iter().copied().map(matched_blocks).collect();
        let key = prompt_key(prompt, matched.iter().copied().min().unwrap_or(0));
        let head = *heat.get_or_insert_with(|| key.map_or_else(HeadStats::default, |key| state.heat.record(key, now)));
        let candidates: Vec<Candidate> = slots
            .iter()
            .zip(&matched)
            .map(|(&i, &matched_blocks)| Candidate { replica: &state.replicas[i], matched_blocks })
            .collect();
        let request = Request {
            prompt,
            output_tokens,
            typical_demand_tokens: state.typical_demand_tokens,
            heat_share: head.share,
            home: head.home,
            key: key.unwrap_or(0),
            now,
        };
        let picked = self.shared.policy.pick(&request, &candidates, &mut state.rng);
        debug_assert!(picked < candidates.len(), "{} picked out of range", self.shared.policy.name());
        // A third-party policy's bad index must not panic while the lock is held.
        let chosen = picked.min(candidates.len() - 1);
        let uncached = candidates[chosen].uncached_tokens(&request);
        drop(candidates);

        if let Some(key) = key {
            state.heat.set_home(key, state.replicas[slots[chosen]].addr);
        }
        let demand = uncached + output_tokens;
        state.typical_demand_tokens = 0.95 * state.typical_demand_tokens + 0.05 * demand as f64;
        let id = state.next_ticket;
        state.next_ticket += 1;
        let replica = &mut state.replicas[slots[chosen]];
        replica.record_route(prompt, uncached, output_tokens);
        let estimate =
            Estimate { id, done_at: now, uncached, output: output_tokens, hit_tokens: prompt.tokens - uncached };
        replica.schedule_prefill_end(signal, estimate);
        Placement::Routed(Ticket::new(
            Arc::clone(&self.shared),
            replica.addr,
            id,
            signal,
            uncached,
            prompt.tokens,
            output_tokens,
        ))
    }
}

/// The block that identifies a prompt for routing: the first past `shared` blocks, the prefix every candidate holds
/// (a system prompt common to all traffic says nothing about where to send a request), or the last if all hold it
/// whole. `None` for prompts under one block.
fn prompt_key(prompt: &Prompt, shared: usize) -> Option<u64> {
    prompt.blocks.get(shared).or(prompt.blocks.last()).copied()
}
