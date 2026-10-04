use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
};

use crate::prompt::Prompt;

/// Outside knowledge of replicas' prefix caches (an engine's KV events, a simulator's ground truth), consulted at
/// each routing decision ([`Scheduler::with_exact_index`](crate::Scheduler::with_exact_index)). Experimental.
pub trait ExactIndex: Send + Sync {
    /// Leading blocks of `prompt` that `addr` holds; `approximate` is what the scheduler's own index believes.
    fn matched_blocks(&self, addr: SocketAddr, prompt: &Prompt, approximate: usize) -> usize;
}

/// One replica's approximate prefix cache: the blocks this router sent it, least recently used evicted first.
#[derive(Debug)]
pub struct BlockLru {
    capacity: usize,
    tick: u64,
    last_used: HashMap<u64, u64>,
    by_age: BTreeMap<u64, u64>,
}

impl BlockLru {
    pub fn new(capacity: usize) -> Self {
        Self { capacity: capacity.max(1), tick: 0, last_used: HashMap::new(), by_age: BTreeMap::new() }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.last_used.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.last_used.is_empty()
    }

    #[cfg(test)]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Leading blocks of `blocks` present, i.e. the cached prefix length in blocks.
    pub fn matched(&self, blocks: &[u64]) -> usize {
        blocks.iter().take_while(|b| self.last_used.contains_key(b)).count()
    }

    /// Records `blocks` as just used. Inserted tail-first so the head block is the most recent: under
    /// pressure the engine drops a prompt's tail before its shared head, and so does this model.
    pub fn touch(&mut self, blocks: &[u64]) {
        for &block in blocks.iter().rev() {
            self.tick += 1;
            if let Some(old) = self.last_used.insert(block, self.tick) {
                self.by_age.remove(&old);
            }
            self.by_age.insert(self.tick, block);
        }
        self.evict();
    }

    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity.max(1);
        self.evict();
    }

    pub fn clear(&mut self) {
        self.last_used.clear();
        self.by_age.clear();
    }

    fn evict(&mut self) {
        while self.last_used.len() > self.capacity {
            let (_, block) = self.by_age.pop_first().expect("by_age mirrors last_used");
            self.last_used.remove(&block);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_leading_run_only() {
        let mut lru = BlockLru::new(10);
        lru.touch(&[1, 2, 3]);
        assert_eq!(lru.matched(&[1, 2, 3, 4]), 3);
        assert_eq!(lru.matched(&[1, 9, 3]), 1);
        assert_eq!(lru.matched(&[9, 1]), 0);
    }

    #[test]
    fn evicts_least_recent_tail_first() {
        let mut lru = BlockLru::new(3);
        lru.touch(&[1, 2, 3]);
        lru.touch(&[1, 5]);
        assert_eq!(lru.len(), 3);
        assert_eq!(lru.matched(&[1, 2, 3]), 2, "3 was the oldest block");
        lru.set_capacity(2);
        assert_eq!(lru.matched(&[1, 5]), 2);
        assert_eq!(lru.matched(&[1, 2]), 1);
        lru.clear();
        assert!(lru.is_empty());
    }
}
