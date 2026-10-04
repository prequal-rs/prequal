//! The engines of a virtual-time run: their steps, and how a request ends (completed, or timed out).

use super::{Event, Sim};
use crate::{load::Completed, preset::Slowdown};

impl Sim {
    pub(super) fn start_step(&mut self, engine: usize) {
        let reqs = &mut self.reqs;
        let step = self.engines[engine].start_step(|&id, cached| reqs[id].cached = cached);
        self.stepping[engine] = step.is_some();
        if let Some(us) = step {
            let us = us * Slowdown::at(self.slowdown, self.now());
            let at = self.now() + (us as u64).max(1);
            self.push(at, Event::StepEnd(engine));
        }
    }

    pub(super) fn step_end(&mut self, engine: usize) {
        let now = self.now();
        let (reqs, cancelled, end_at_first) = (&mut self.reqs, &self.cancelled, self.end_at_first_token);
        let done = self.engines[engine].finish_step(
            |&id, first| {
                let req = &mut reqs[id];
                if first && let Some(ticket) = req.ticket.as_mut() {
                    ticket.first_token();
                    req.first = Some(now);
                    if end_at_first {
                        req.ticket = None;
                    }
                }
            },
            |id| cancelled.contains(id),
        );
        for job in done {
            if !self.cancelled.contains(job.tag()) {
                self.finish(*job.tag(), true);
            }
        }
        self.start_step(engine);
    }

    pub(super) fn expire(&mut self, id: usize) {
        if self.reqs[id].outcome.is_none() {
            self.cancelled.insert(id);
            self.finish(id, false);
        }
    }

    /// Records the request's outcome, releases its routing ticket, and lets a closed-loop user send again.
    fn finish(&mut self, id: usize, ok: bool) {
        let now = self.now();
        if let Some(oracle) = &mut self.oracle {
            oracle.finished(id);
        }
        let req = &mut self.reqs[id];
        let user = req.user;
        req.ticket = None;
        if ok && let Some(orderer) = self.orderers.get_mut(req.router) {
            orderer.observe(&req.keys, req.output);
        }
        req.outcome = Some(match (ok, req.first) {
            (true, Some(first)) => Ok(Completed {
                ttft_us: first - req.arrived,
                e2e_us: now - req.arrived,
                tokens: req.output,
                prefix: Some((req.prompt_tokens, req.cached)),
            }),
            _ => Err(()),
        });
        if let Some(user) = user {
            self.push(now, Event::Arrive { user: Some(user) });
        }
    }
}
