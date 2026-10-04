use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use prequal_llm::{Prompt, Scheduler, Ticket};
use rand::seq::SliceRandom;

use crate::{board::Board, metadata::subset_allows, objectives::Objectives, responses::LlmdError};

/// Why no endpoint was picked, answered as llm-d's EPP (v0.11, flow control off) answers it.
#[derive(Debug, PartialEq, Eq)]
pub enum Reject {
    /// No ready endpoint, or none allowed by the subset hint (503).
    Unavailable,
    /// A sheddable request (priority < 0) while the candidates are saturated (429).
    Saturated,
}

impl Reject {
    pub fn error(&self) -> LlmdError {
        match self {
            Self::Unavailable => LlmdError {
                status: 503,
                code: "ServiceUnavailable",
                message: "failed to find endpoint candidates for serving the request",
                dropped_reason: Some("rejected-no-endpoints"),
            },
            Self::Saturated => LlmdError {
                status: 429,
                code: "ResourceExhausted",
                message: "system saturated, sheddable request dropped",
                dropped_reason: None,
            },
        }
    }
}

/// Routes each request with the pool's [`Scheduler`] (fed by engine-metrics scrapes).
pub struct Picker {
    scheduler: Scheduler,
    fallbacks: usize,
    synced: AtomicBool,
    board: Board,
    objectives: Objectives,
}

impl Picker {
    /// `scrape_interval` is how often the scheduler's replicas are scraped (it sets how stale a scrape may get).
    pub fn new(scheduler: Scheduler, fallbacks: usize, scrape_interval: Duration) -> Self {
        let board = Board::new(scrape_interval);
        Self { scheduler, fallbacks, synced: AtomicBool::new(false), board, objectives: Objectives::default() }
    }

    /// Replaces the endpoint set (address to llm-d endpoint name); the picker is ready from the first sync.
    pub fn sync(&self, endpoints: &BTreeMap<SocketAddr, String>) {
        self.scheduler.sync(endpoints.keys().copied());
        self.board.sync(endpoints);
        self.synced.store(true, Ordering::Release);
    }

    pub fn is_synced(&self) -> bool {
        self.synced.load(Ordering::Acquire)
    }

    /// Appends llm-d's pool gauges (see [`Board::render`]).
    pub fn render_pool_metrics(&self, pool: &str, namespace: &str, out: &mut String) {
        self.board.render(pool, namespace, &self.scheduler.replicas(), Instant::now(), out);
    }

    pub fn objectives(&self) -> &Objectives {
        &self.objectives
    }

    /// Routes a request. Returns its ticket and the ranked endpoints: the pick, then up to `fallbacks` random
    /// alternatives for gateway retries. `subset` restricts candidates (the gateway's subset hint or a conformance
    /// test header). Sheddable requests are refused first if the candidates are saturated, as llm-d's admission
    /// does; with an admission limit the others wait for a replica with room (late binding).
    pub async fn pick(
        &self,
        prompt: &Prompt,
        output_tokens: u64,
        subset: Option<&[String]>,
        priority: i32,
    ) -> Result<(Ticket, Vec<SocketAddr>), Reject> {
        let allowed = |addr: &SocketAddr| subset.is_none_or(|s| subset_allows(s, addr));
        if priority < 0 && self.board.saturation(&self.scheduler.replicas(), allowed, Instant::now()) >= 1.0 {
            return Err(Reject::Saturated);
        }
        let ticket = self.scheduler.acquire(prompt, output_tokens, allowed).await.ok_or(Reject::Unavailable)?;
        let mut others: Vec<SocketAddr> =
            self.scheduler.addrs().into_iter().filter(|a| *a != ticket.addr() && allowed(a)).collect();
        others.shuffle(&mut rand::rng());
        let ranked = std::iter::once(ticket.addr()).chain(others.into_iter().take(self.fallbacks)).collect();
        Ok((ticket, ranked))
    }
}

#[cfg(test)]
mod tests {
    use prequal_llm::{EngineStats, policy};

    use super::*;

    fn named(list: &[&str]) -> BTreeMap<SocketAddr, String> {
        list.iter().map(|a| (a.parse().unwrap(), (*a).to_owned())).collect()
    }

    fn picker(fallbacks: usize) -> (Picker, Scheduler) {
        let scheduler = Scheduler::new(policy::by_name("prequal").unwrap());
        (Picker::new(scheduler.clone(), fallbacks, Duration::from_millis(50)), scheduler)
    }

    #[tokio::test]
    async fn picks_within_subset_with_fallbacks() {
        let (picker, _) = picker(2);
        let prompt = Prompt::default();
        assert!(picker.pick(&prompt, 1, None, 0).await.is_err());
        let pool = named(&["10.0.0.1:8000", "10.0.0.2:8000", "10.0.0.3:8000", "10.0.0.4:8000"]);
        picker.sync(&pool);
        assert!(picker.is_synced());
        assert_eq!(picker.pick(&prompt, 1, None, 0).await.unwrap().1.len(), 3);
        let subset = vec!["10.0.0.2".to_owned()];
        let picked = picker.pick(&prompt, 1, Some(&subset), 0).await.unwrap().1;
        assert_eq!(picked, ["10.0.0.2:8000".parse::<SocketAddr>().unwrap()]);
        let none = vec!["10.9.9.9:1".to_owned()];
        assert_eq!(picker.pick(&prompt, 1, Some(&none), 0).await.err(), Some(Reject::Unavailable));
    }

    #[tokio::test]
    async fn sheds_only_sheddable_requests_and_only_when_saturated() {
        let (picker, scheduler) = picker(0);
        let prompt = Prompt::default();
        // As in llm-d, admission runs before the candidate check: no endpoints is saturation 1.
        assert_eq!(picker.pick(&prompt, 1, None, -1).await.err(), Some(Reject::Saturated));
        let pool = named(&["10.0.0.1:8000", "10.0.0.2:8000"]);
        picker.sync(&pool);
        assert_eq!(picker.pick(&prompt, 1, None, -1).await.err(), Some(Reject::Saturated), "never scraped");
        assert!(picker.pick(&prompt, 1, None, 0).await.is_ok(), "priority 0 is never shed");
        let load = |waiting| EngineStats::new(0.0, waiting, 0.0);
        let (busy, idle) = (*pool.keys().next().unwrap(), *pool.keys().last().unwrap());
        scheduler.observe(busy, load(9.0));
        scheduler.observe(idle, load(0.0));
        assert!(picker.pick(&prompt, 1, None, -1).await.is_ok(), "mean (1.8 + 0) / 2 is under 1");
        scheduler.observe(idle, load(2.0));
        assert_eq!(picker.pick(&prompt, 1, None, -5).await.err(), Some(Reject::Saturated), "(1.8 + 0.4) / 2");
        assert_eq!(Reject::Saturated.error().status, 429);
        assert_eq!(Reject::Unavailable.error().status, 503);
    }
}
