//! Streaming load against OpenAI-compatible endpoints: open-loop arrivals at given times, or closed-loop users.
//! Measures time to first token (first SSE event), end-to-end latency, and the engine-reported cached prefix.

use std::{
    fmt::Write,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use prequal_llm::queue_order::OutputHistory;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{
    cache::TOKEN_BYTES,
    order::{Orderer, QueueOrder},
    workload::{Prompt, Request, Source, derive},
};

/// `Ok` for a completed stream; `Err` for a transport error, non-2xx status, or timeout.
pub type Outcome = Result<Completed, ()>;

#[derive(Clone, Copy)]
pub struct Completed {
    pub ttft_us: u64,
    pub e2e_us: u64,
    pub tokens: u64,
    /// `(prompt_tokens, cached_tokens)` from the final usage chunk, when the server sent one.
    pub prefix: Option<(u64, u64)>,
}

/// Wall-clock load shape; requests are attributed to the stage they were issued in.
pub enum Arrivals {
    /// Send times from the start, ascending.
    Open { at: Vec<Duration>, stages: Vec<Duration> },
    /// `users` back-to-back senders.
    Closed { users: usize, stages: Vec<Duration> },
}

/// What a request body carries beyond the workload's prompt and length.
#[derive(Default)]
pub struct Wire {
    pub model: Option<String>,
    /// Send text prompts as token ids below this vocabulary size, so a real tokenizer sees the intended lengths.
    pub token_vocab: Option<u32>,
    /// Stamp `priority` as a router would (see `order`); unstamped requests are FCFS under vLLM's priority policy.
    pub queue_order: Option<QueueOrder>,
}

struct Ctx {
    client: reqwest::Client,
    urls: Vec<String>,
    source: Source,
    timeout: Duration,
    sent: AtomicUsize,
    started: Instant,
    model: Option<String>,
    token_vocab: Option<u32>,
    orderer: Option<Mutex<Orderer>>,
}

impl Ctx {
    async fn send(&self) -> Outcome {
        let request = self.source.next();
        let output = request.max_tokens;
        let mut body = self.body(request);
        let keys = self.orderer.as_ref().map(|orderer| {
            let prompt = prequal_llm::Prompt::from_body(body.as_bytes());
            let arrival_us = self.started.elapsed().as_micros() as u64;
            let priority = orderer.lock().unwrap().priority(arrival_us, &prompt, output);
            body.pop();
            write!(body, ",\"priority\":{priority}}}").expect("writing to a String");
            OutputHistory::keys(&prompt)
        });
        let url = &self.urls[self.sent.fetch_add(1, Ordering::Relaxed) % self.urls.len()];
        let outcome =
            tokio::time::timeout(self.timeout, stream_completion(&self.client, url, body)).await.unwrap_or(Err(()));
        if let (Some(orderer), Some(keys), Ok(completed)) = (&self.orderer, keys, &outcome) {
            orderer.lock().unwrap().observe(&keys, completed.tokens);
        }
        outcome
    }

    fn body(&self, request: Request) -> String {
        let mut body = serde_json::json!({
            "max_tokens": request.max_tokens,
            "stream": true,
            "stream_options": {"include_usage": true},
            "ignore_eos": true,
        });
        if let Some(model) = &self.model {
            body["model"] = model.as_str().into();
        }
        match (request.prompt, self.token_vocab) {
            (Prompt::Text(text), Some(vocab)) => body["prompt"] = token_ids(&text, vocab).into(),
            (Prompt::Text(text), None) => body["prompt"] = text.into(),
            (Prompt::Tokens(n), _) => {
                body["prompt"] = "".into();
                body["prompt_tokens"] = n.into();
            }
        }
        body.to_string()
    }
}

/// One id per `TOKEN_BYTES` of pseudo-text: prompts sharing leading text share leading tokens.
fn token_ids(text: &str, vocab: u32) -> Vec<u32> {
    let id = |chunk: &[u8]| {
        let word = chunk.iter().fold(0u64, |word, &b| word << 8 | u64::from(b));
        (derive(0, 6, word) % u64::from(vocab.max(1))) as u32
    };
    text.as_bytes().chunks(TOKEN_BYTES).map(id).collect()
}

fn usage(tail: &[u8]) -> Option<(u64, u64, u64)> {
    let text = String::from_utf8_lossy(tail);
    let usage =
        text.lines().rev().filter_map(|line| line.strip_prefix("data: ")).find_map(|json| {
            serde_json::from_str::<Value>(json).ok()?.get("usage").filter(|u| !u.is_null()).cloned()
        })?;
    let field = |path: &str| usage.pointer(path).and_then(Value::as_u64);
    Some((
        field("/prompt_tokens")?,
        field("/prompt_tokens_details/cached_tokens").unwrap_or(0),
        field("/completion_tokens")?,
    ))
}

async fn stream_completion(client: &reqwest::Client, url: &str, body: String) -> Outcome {
    const TAIL: usize = 2048;
    let start = Instant::now();
    let response = client
        .post(url)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(drop)?
        .error_for_status()
        .map_err(drop)?;
    let (mut ttft_us, mut events, mut tail) = (None, 0u64, Vec::new());
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(drop)?;
        ttft_us.get_or_insert_with(|| start.elapsed().as_micros() as u64);
        events += chunk.windows(6).filter(|w| w == b"data: ").count() as u64;
        tail.extend_from_slice(&chunk);
        if tail.len() > 2 * TAIL {
            tail.drain(..tail.len() - TAIL);
        }
    }
    let e2e_us = start.elapsed().as_micros() as u64;
    let (prefix, tokens) = match usage(&tail) {
        Some((prompt, cached, completion)) => (Some((prompt, cached)), completion),
        None => (None, events.saturating_sub(1)),
    };
    Ok(Completed { ttft_us: ttft_us.ok_or(())?, e2e_us, tokens, prefix })
}

/// Drives `arrivals` round-robin over `targets`; returns each stage's outcomes, indexed like the stages.
pub async fn run(
    targets: &[SocketAddr],
    source: Source,
    arrivals: Arrivals,
    timeout: Duration,
    wire: Wire,
    seed: u64,
) -> Vec<Vec<Outcome>> {
    let ctx = Arc::new(Ctx {
        client: reqwest::Client::new(),
        urls: targets.iter().map(|t| format!("http://{t}/v1/completions")).collect(),
        source,
        timeout,
        sent: AtomicUsize::new(0),
        started: Instant::now(),
        model: wire.model,
        token_vocab: wire.token_vocab,
        orderer: wire.queue_order.map(|order| Mutex::new(Orderer::new(order, seed))),
    });
    let (tx, mut rx) = mpsc::unbounded_channel::<(usize, Outcome)>();
    let stages = match arrivals {
        Arrivals::Open { at, stages } => {
            open_loop(&ctx, &at, &ends(ctx.started, &stages), &tx).await;
            stages.len()
        }
        Arrivals::Closed { users, stages } => {
            closed_loop(&ctx, users, ends(ctx.started, &stages), &tx).await;
            stages.len()
        }
    };
    drop(tx);
    let mut outcomes = vec![Vec::new(); stages];
    while let Some((stage, outcome)) = rx.recv().await {
        outcomes[stage].push(outcome);
    }
    outcomes
}

/// When each stage ends.
fn ends(start: Instant, stages: &[Duration]) -> Vec<Instant> {
    stages
        .iter()
        .scan(start, |at, d| {
            *at += *d;
            Some(*at)
        })
        .collect()
}

async fn open_loop(ctx: &Arc<Ctx>, at: &[Duration], ends: &[Instant], tx: &mpsc::UnboundedSender<(usize, Outcome)>) {
    for &at in at {
        let at = ctx.started + at;
        let Some(stage) = ends.iter().position(|&end| at < end) else { break };
        tokio::time::sleep_until(at.into()).await;
        let (ctx, tx) = (Arc::clone(ctx), tx.clone());
        tokio::spawn(async move {
            let _ = tx.send((stage, ctx.send().await));
        });
    }
}

async fn closed_loop(ctx: &Arc<Ctx>, users: usize, ends: Vec<Instant>, tx: &mpsc::UnboundedSender<(usize, Outcome)>) {
    let ends = Arc::new(ends);
    let users: Vec<_> = (0..users)
        .map(|_| {
            let (ctx, tx, ends) = (Arc::clone(ctx), tx.clone(), Arc::clone(&ends));
            tokio::spawn(async move {
                loop {
                    let now = Instant::now();
                    let Some(stage) = ends.iter().position(|&end| now < end) else { break };
                    let _ = tx.send((stage, ctx.send().await));
                }
            })
        })
        .collect();
    for user in users {
        let _ = user.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::ascii;

    #[test]
    fn token_ids_keep_length_and_shared_prefixes() {
        let (system, a, b) = (ascii(1, 40), ascii(2, 10), ascii(3, 10));
        let (a, b) = (token_ids(&format!("{system}{a}"), 1000), token_ids(&format!("{system}{b}"), 1000));
        assert_eq!((a.len(), b.len()), (50, 50));
        assert_eq!(a[..40], b[..40]);
        assert_ne!(a[40..], b[40..]);
        assert!(a.iter().all(|&id| id < 1000));
    }
}
