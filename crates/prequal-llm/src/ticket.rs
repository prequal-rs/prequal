use std::{fmt, net::SocketAddr, str::FromStr, sync::Arc};

use crate::scheduler::Shared;

/// What ends a request's prefill reservation (its share of the "awaiting first token" load the policy sees). The
/// first response body chunk is exact but makes the gateway stream every chunk through the router; the others let it
/// skip response bodies (lean mode; see the repository README).
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum PrefillSignal {
    /// [`Ticket::first_token`]: the first response body chunk.
    FirstChunk,
    /// [`Ticket::response_started`]: response headers, which vLLM sends before the request is even queued.
    Headers,
    /// An estimated prefill completion: the replica's estimated prefill backlog plus this request's uncached tokens,
    /// at this many prefill tokens per second.
    Estimate {
        /// Assumed prefill throughput.
        tokens_per_sec: f64,
    },
    /// Until the replica's next scrape, which then counts the request in its own queue.
    Scrape,
    /// Held until the response ends.
    End,
}

impl FromStr for PrefillSignal {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.split_once(':') {
            None if s == "first-chunk" => Ok(Self::FirstChunk),
            None if s == "headers" => Ok(Self::Headers),
            None if s == "end" => Ok(Self::End),
            None if s == "scrape" => Ok(Self::Scrape),
            Some(("estimate", rate)) => match rate.parse::<f64>() {
                Ok(tokens_per_sec) if tokens_per_sec > 0.0 => Ok(Self::Estimate { tokens_per_sec }),
                _ => Err(format!("{s:?}: estimate needs a positive tokens/s")),
            },
            _ => Err(format!("{s:?} is not first-chunk, headers, estimate:<tokens/s>, scrape or end")),
        }
    }
}

impl fmt::Display for PrefillSignal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FirstChunk => f.write_str("first-chunk"),
            Self::Headers => f.write_str("headers"),
            Self::Estimate { tokens_per_sec } => write!(f, "estimate:{tokens_per_sec}"),
            Self::Scrape => f.write_str("scrape"),
            Self::End => f.write_str("end"),
        }
    }
}

/// A routed request's reservation. Call [`Ticket::response_started`] at response headers and [`Ticket::first_token`]
/// at the first body chunk (the scheduler's [`PrefillSignal`] decides which one counts), and let it drop when the
/// response ends; dropping it early (error, cancellation) releases everything it still holds.
#[must_use = "dropping the ticket ends the request's reservation"]
pub struct Ticket {
    shared: Arc<Shared>,
    addr: SocketAddr,
    uncached: u64,
    id: u64,
    signal: PrefillSignal,
    prompt_tokens: u64,
    output: u64,
    prefilling: bool,
    finished: bool,
}

impl Ticket {
    pub(crate) fn new(
        shared: Arc<Shared>,
        addr: SocketAddr,
        id: u64,
        signal: PrefillSignal,
        uncached: u64,
        prompt_tokens: u64,
        output: u64,
    ) -> Self {
        Self { shared, addr, uncached, id, signal, prompt_tokens, output, prefilling: true, finished: false }
    }

    /// The replica the request was routed to.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Prompt tokens the replica was not believed to have cached when routed.
    pub fn uncached_tokens(&self) -> u64 {
        self.uncached
    }

    /// The response's headers arrived.
    pub fn response_started(&mut self) {
        if self.signal == PrefillSignal::Headers {
            self.end_prefill();
        }
    }

    /// The response's first body chunk arrived.
    pub fn first_token(&mut self) {
        if self.signal == PrefillSignal::FirstChunk {
            self.end_prefill();
        }
    }

    /// Prefill is done: release the prefill reservation and count the predicted cache hit.
    fn end_prefill(&mut self) {
        if !std::mem::replace(&mut self.prefilling, false) {
            return;
        }
        self.shared.update_replica(self.addr, |replica, _| {
            replica.end_prefill(self.uncached, self.output);
            replica.count_prefilled(self.prompt_tokens, self.prompt_tokens - self.uncached);
        });
    }

    /// The request failed at the replica (connect error, 5xx): mark it down until its next good scrape.
    pub fn failed(mut self) {
        self.release(true);
    }

    fn release(&mut self, failed: bool) {
        if std::mem::replace(&mut self.finished, true) {
            return;
        }
        let signal = self.signal;
        self.shared.update_replica(self.addr, |replica, _| {
            // An estimated reservation may already have expired.
            let prefilling = match signal {
                PrefillSignal::Estimate { .. } | PrefillSignal::Scrape => replica.forget_estimate(self.id),
                _ => self.prefilling,
            };
            if prefilling {
                replica.end_prefill(self.uncached, self.output);
                if signal == PrefillSignal::End && !failed {
                    replica.count_prefilled(self.prompt_tokens, self.prompt_tokens - self.uncached);
                }
            }
            replica.active = replica.active.saturating_sub(1);
            replica.active_tokens = replica.active_tokens.saturating_sub(self.prompt_tokens + self.output);
            replica.down |= failed;
        });
    }
}

impl fmt::Debug for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ticket")
            .field("addr", &self.addr)
            .field("uncached", &self.uncached)
            .field("signal", &self.signal)
            .field("prefilling", &self.prefilling)
            .finish_non_exhaustive()
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.release(false);
    }
}
