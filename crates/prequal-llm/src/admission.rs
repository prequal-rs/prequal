//! Late binding: [`Scheduler::acquire`] and its router-wide FIFO admission queue.

use std::net::SocketAddr;

use super::{Placement, Scheduler, Shared};
use crate::{prompt::Prompt, ticket::Ticket};

impl Scheduler {
    /// Like [`Scheduler::route`], but with an admission limit waits (FIFO) until a replica has room. `None` only if
    /// no replica is eligible at all.
    pub async fn acquire(
        &self,
        prompt: &Prompt,
        output_tokens: u64,
        allowed: impl Fn(&SocketAddr) -> bool,
    ) -> Option<Ticket> {
        let Some(limit) = self.state().admission_limit else {
            return self.route(prompt, output_tokens, allowed);
        };
        let mut heat = None;
        let mut place = QueuePlace { shared: &self.shared, queued: false };
        loop {
            let mut room = std::pin::pin!(self.shared.room.notified());
            room.as_mut().enable();
            {
                let mut state = self.state();
                if place.queued || state.queued == 0 {
                    match self.place(&mut state, prompt, output_tokens, &allowed, Some(limit), &mut heat) {
                        Placement::Routed(ticket) => {
                            drop(state);
                            drop(place);
                            // The next in line may fit too; it re-checks and passes the baton on.
                            self.shared.room.notify_one();
                            return Some(ticket);
                        }
                        Placement::Unavailable => return None,
                        Placement::Full => {}
                    }
                }
                if !place.queued {
                    state.queued += 1;
                    place.queued = true;
                }
            }
            room.await;
        }
    }
}

/// A request's place in the admission queue; leaving (routed, or cancelled while waiting) frees it.
struct QueuePlace<'a> {
    shared: &'a Shared,
    queued: bool,
}

impl Drop for QueuePlace<'_> {
    fn drop(&mut self) {
        if self.queued {
            self.shared.state().queued -= 1;
        }
    }
}
