//! `--oracle-index` (virtual time only): routers are told what each engine's prefix cache really holds, with no delay,
//! instead of relying on their own approximate index. It bounds what engine KV events could gain before anything is
//! built on them (docs/roadmap.md, project 4): an integration can't beat the oracle.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use clap::ValueEnum;
use prequal_llm::{BLOCK_BYTES, ExactIndex, Prompt};

use crate::{cache::BLOCK_TOKENS, engine::EngineCore, preset::EngineSpec};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum OracleIndex {
    /// Cached blocks plus the prompts of every request still in the engine, from any router.
    Perfect,
    /// Cached blocks only, which is all KV events report; a prompt still queued or in prefill is invisible.
    Events,
    /// The larger of `events` and the router's approximate index, as the roadmap's hybrid index would.
    Hybrid,
}

/// What each engine holds of the request being routed, in router blocks.
struct View {
    mode: OracleIndex,
    held: Mutex<HashMap<SocketAddr, usize>>,
}

impl ExactIndex for View {
    fn matched_blocks(&self, addr: SocketAddr, _: &Prompt, approximate: usize) -> usize {
        let held = self.held.lock().unwrap().get(&addr).copied().unwrap_or(0);
        match self.mode {
            OracleIndex::Hybrid => held.max(approximate),
            OracleIndex::Perfect | OracleIndex::Events => held,
        }
    }
}

pub struct Oracle {
    view: Arc<View>,
    /// Prompt bytes per engine cache block, per engine.
    block_bytes: Vec<usize>,
    /// Engine and prompt block hashes of each unfinished request.
    in_flight: HashMap<usize, (usize, Vec<u64>)>,
}

impl Oracle {
    pub fn new(mode: OracleIndex, specs: &[EngineSpec]) -> Self {
        Self {
            view: Arc::new(View { mode, held: Mutex::default() }),
            block_bytes: specs.iter().map(|s| s.token_bytes * BLOCK_TOKENS as usize).collect(),
            in_flight: HashMap::new(),
        }
    }

    pub fn index(&self) -> Arc<dyn ExactIndex> {
        Arc::clone(&self.view) as Arc<dyn ExactIndex>
    }

    /// Publishes what each engine holds of `text` to the routers, for the routing decision that follows.
    pub fn look<T>(&self, text: &[u8], prompt: &Prompt, engines: &[EngineCore<T>], addrs: &[SocketAddr]) {
        let mut held = self.view.held.lock().unwrap();
        // Engines sharing a tokenizer share the hashes.
        let mut tokenized: Option<(usize, Vec<u64>)> = None;
        for (i, (engine, addr)) in engines.iter().zip(addrs).enumerate() {
            if tokenized.as_ref().is_none_or(|(bytes, _)| *bytes != self.block_bytes[i]) {
                tokenized = Some((self.block_bytes[i], engine.tokenize(text).0));
            }
            let hashes = &tokenized.as_ref().expect("just set").1;
            let mut blocks = engine.cached_prefix(hashes);
            if self.view.mode == OracleIndex::Perfect {
                let queued = self.in_flight.values().filter(|(at, _)| *at == i);
                let shared = queued.map(|(_, other)| hashes.iter().zip(other).take_while(|(a, b)| a == b).count());
                blocks = blocks.max(shared.max().unwrap_or(0));
            }
            let router_blocks = match blocks {
                0 => 0,
                all if all == hashes.len() => prompt.blocks.len(),
                some => (some * self.block_bytes[i] / BLOCK_BYTES).min(prompt.blocks.len()),
            };
            held.insert(*addr, router_blocks);
        }
    }

    pub fn sent(&mut self, id: usize, engine: usize, hashes: &[u64]) {
        self.in_flight.insert(id, (engine, hashes.to_vec()));
    }

    pub fn finished(&mut self, id: usize) {
        self.in_flight.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{engine::Job, preset::Preset};

    #[test]
    fn modes_differ_in_queued_prompts_and_the_approximate_index() {
        let spec = Preset::Default.spec();
        let addrs = [1, 2].map(|host| SocketAddr::from(([10, 0, 0, host], 8000)));
        let mut engines = [EngineCore::<usize>::new(spec), EngineCore::new(spec)];
        let text = vec![b'x'; BLOCK_BYTES * 8];
        let prompt = Prompt::from_text(&text);
        let (hashes, tokens) = engines[0].tokenize(&text);
        engines[0].submit(Job::new(hashes.clone(), tokens, 1, 0, 0));
        while engines[0].start_step(|_, _| {}).is_some() {
            engines[0].finish_step(|_, _| {}, |_| false);
        }
        let held = |mode, queued_on: Option<usize>| {
            let mut oracle = Oracle::new(mode, &[spec, spec]);
            if let Some(engine) = queued_on {
                oracle.sent(7, engine, &hashes);
            }
            oracle.look(&text, &prompt, &engines, &addrs);
            addrs.map(|addr| oracle.index().matched_blocks(addr, &prompt, 3))
        };
        assert_eq!(held(OracleIndex::Events, Some(1)), [8, 0]);
        assert_eq!(held(OracleIndex::Perfect, Some(1)), [8, 8]);
        assert_eq!(held(OracleIndex::Hybrid, None), [8, 3]);
    }
}
