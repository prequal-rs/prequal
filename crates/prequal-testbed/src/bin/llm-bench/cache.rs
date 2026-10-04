//! vLLM-style prefix cache over 16-token blocks of 4-byte pseudo-tokens. Block `i` covers prompt bytes
//! `[64i, 64i + 64)` and is keyed by a chained hash, so a block hit implies its whole prefix matched.
//! Running sequences pin their blocks; unpinned cached blocks sit in an LRU and are evicted on demand.

use std::{
    collections::{BTreeMap, HashMap},
    hash::{DefaultHasher, Hash, Hasher},
};

/// Bytes per workload pseudo-token, and the default engine tokenizer's.
pub const TOKEN_BYTES: usize = 4;
pub const BLOCK_TOKENS: u64 = 16;
#[cfg(test)]
const BLOCK_BYTES: usize = TOKEN_BYTES * BLOCK_TOKENS as usize;

pub fn token_count(prompt: &[u8], token_bytes: usize) -> u64 {
    prompt.len().div_ceil(token_bytes) as u64
}

pub fn blocks_for(tokens: u64) -> u64 {
    tokens.div_ceil(BLOCK_TOKENS)
}

/// Chained hashes of the prompt's full blocks; a partial tail block is not cacheable.
pub fn block_hashes(prompt: &[u8], token_bytes: usize) -> Vec<u64> {
    let mut prev = 0u64;
    prompt
        .chunks_exact(token_bytes * BLOCK_TOKENS as usize)
        .map(|block| {
            let mut hasher = DefaultHasher::new();
            prev.hash(&mut hasher);
            block.hash(&mut hasher);
            prev = hasher.finish();
            prev
        })
        .collect()
}

/// A running sequence's claim on the cache: pinned hashed blocks plus private (uncacheable) blocks.
#[derive(Debug)]
pub struct Lease {
    hashes: Vec<u64>,
    pinned: usize,
    private: u64,
    pub hit_tokens: u64,
}

struct Entry {
    refs: u32,
    tick: u64,
}

pub struct BlockCache {
    capacity: u64,
    blocks: HashMap<u64, Entry>,
    lru: BTreeMap<u64, u64>,
    private: u64,
    tick: u64,
}

impl BlockCache {
    pub fn new(capacity_blocks: u64) -> Self {
        Self { capacity: capacity_blocks, blocks: HashMap::new(), lru: BTreeMap::new(), private: 0, tick: 0 }
    }

    fn used(&self) -> u64 {
        self.blocks.len() as u64 + self.private
    }

    /// Leading `hashes` cached, pinned or not.
    pub fn cached_prefix(&self, hashes: &[u64]) -> usize {
        hashes.iter().take_while(|h| self.blocks.contains_key(h)).count()
    }

    /// Hashes of every cached block, pinned or not.
    pub fn block_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.blocks.keys().copied()
    }

    /// Blocks held by running sequences (vLLM's `kv_cache_usage_perc` numerator).
    pub fn active_blocks(&self) -> u64 {
        (self.blocks.len() - self.lru.len()) as u64 + self.private
    }

    /// Reserves `total_blocks` (prompt + output) for a sequence, reusing its longest cached prefix. Hands `hashes` back
    /// when even evicting every unpinned block can't make room, unless `force` (an idle engine must make progress).
    pub fn admit(
        &mut self,
        hashes: Vec<u64>,
        prompt_tokens: u64,
        total_blocks: u64,
        force: bool,
    ) -> Result<Lease, Vec<u64>> {
        // vLLM always recomputes the last prompt token, so a fully cached prompt still misses its final block.
        let max_hit = ((prompt_tokens.saturating_sub(1)) / BLOCK_TOKENS) as usize;
        let hit = hashes.iter().take(max_hit).take_while(|h| self.blocks.contains_key(h)).count();
        let reclaimable = hashes[..hit].iter().filter(|h| self.blocks[h].refs == 0).count() as u64;
        let need = total_blocks - hit as u64;
        let free = self.capacity.saturating_sub(self.used()) + self.lru.len() as u64 - reclaimable;
        if need > free && !force {
            return Err(hashes);
        }
        for h in &hashes[..hit] {
            self.pin(*h);
        }
        self.evict(need);
        self.private += need;
        Ok(Lease { hashes, pinned: hit, private: need, hit_tokens: hit as u64 * BLOCK_TOKENS })
    }

    /// Publishes the prefilled prompt's blocks so later requests can hit them.
    pub fn commit(&mut self, lease: &mut Lease) {
        for &h in &lease.hashes[lease.pinned..] {
            match self.blocks.get(&h) {
                Some(_) => self.pin(h),
                None => {
                    self.blocks.insert(h, Entry { refs: 1, tick: 0 });
                }
            }
        }
        let published = (lease.hashes.len() - lease.pinned) as u64;
        self.private -= published;
        lease.private -= published;
        lease.pinned = lease.hashes.len();
    }

    /// Adds `blocks` private blocks to a running sequence's lease, evicting unpinned blocks; false if they don't fit.
    pub fn grow(&mut self, lease: &mut Lease, blocks: u64) -> bool {
        if self.capacity.saturating_sub(self.used()) + (self.lru.len() as u64) < blocks {
            return false;
        }
        self.evict(blocks);
        self.private += blocks;
        lease.private += blocks;
        true
    }

    /// Frees the lease; returns its prompt's block hashes.
    pub fn release(&mut self, lease: Lease) -> Vec<u64> {
        self.private -= lease.private;
        // Unpin tail-first so a prefix outlives its suffixes in the LRU, as vLLM's free queue does.
        for &h in lease.hashes[..lease.pinned].iter().rev() {
            let tick = self.tick;
            let entry = self.blocks.get_mut(&h).expect("pinned block is cached");
            entry.refs -= 1;
            if entry.refs == 0 {
                entry.tick = tick;
                self.lru.insert(tick, h);
                self.tick += 1;
            }
        }
        lease.hashes
    }

    fn pin(&mut self, h: u64) {
        let entry = self.blocks.get_mut(&h).expect("pinned block is cached");
        if entry.refs == 0 {
            self.lru.remove(&entry.tick);
        }
        entry.refs += 1;
    }

    fn evict(&mut self, need: u64) {
        while self.used() + need > self.capacity {
            let Some((_, h)) = self.lru.pop_first() else { break };
            self.blocks.remove(&h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_count(text: &[u8]) -> u64 {
        super::token_count(text, TOKEN_BYTES)
    }

    fn block_hashes(text: &[u8]) -> Vec<u64> {
        super::block_hashes(text, TOKEN_BYTES)
    }

    #[test]
    fn coarser_tokenizers_make_fewer_tokens_and_blocks() {
        let text = [b'x'; 192];
        assert_eq!((super::token_count(&text, 6), super::block_hashes(&text, 6).len()), (32, 2));
    }

    fn prompt(blocks: &[u8]) -> Vec<u8> {
        blocks.iter().flat_map(|&b| [b; BLOCK_BYTES]).collect()
    }

    fn admit(cache: &mut BlockCache, text: &[u8], output_blocks: u64) -> Option<Lease> {
        let tokens = token_count(text);
        cache.admit(block_hashes(text), tokens, blocks_for(tokens) + output_blocks, false).ok()
    }

    fn run_to_completion(cache: &mut BlockCache, text: &[u8]) -> u64 {
        let mut lease = admit(cache, text, 1).expect("fits");
        let hit = lease.hit_tokens;
        cache.commit(&mut lease);
        cache.release(lease);
        hit
    }

    #[test]
    fn tokenization_and_full_blocks_only() {
        assert_eq!(token_count(b"abcde"), 2);
        assert_eq!(block_hashes(&[b'x'; 127]).len(), 1);
        assert_eq!(block_hashes(&[b'x'; 128]).len(), 2);
    }

    #[test]
    fn chained_hash_encodes_prefix() {
        let (a, b) = (block_hashes(&prompt(b"ab")), block_hashes(&prompt(b"cb")));
        assert_ne!(a[1], b[1], "same block after different prefixes must differ");
        assert_eq!(a[0], block_hashes(&prompt(b"az"))[0]);
    }

    #[test]
    fn miss_then_hit_keeps_last_token_uncached() {
        let mut cache = BlockCache::new(100);
        let text = prompt(b"abc");
        assert_eq!(run_to_completion(&mut cache, &text), 0);
        assert_eq!(run_to_completion(&mut cache, &text), 2 * BLOCK_TOKENS, "block-aligned prompt recomputes its tail");
        let mut longer = text.clone();
        longer.extend_from_slice(b"tail");
        assert_eq!(run_to_completion(&mut cache, &longer), 3 * BLOCK_TOKENS);
        assert_eq!(run_to_completion(&mut cache, &prompt(b"xbc")), 0, "divergent first block misses everything");
    }

    #[test]
    fn uncommitted_prefill_is_not_a_hit() {
        let mut cache = BlockCache::new(100);
        let text = prompt(b"abcd");
        let first = admit(&mut cache, &text, 1).unwrap();
        assert_eq!(admit(&mut cache, &text, 1).unwrap().hit_tokens, 0);
        assert_eq!(first.hit_tokens, 0);
    }

    #[test]
    fn pinned_blocks_survive_pressure() {
        let mut cache = BlockCache::new(10);
        let mut running = admit(&mut cache, &prompt(b"abcd"), 1).unwrap();
        cache.commit(&mut running);
        assert_eq!(cache.active_blocks(), 5);
        assert!(admit(&mut cache, &prompt(b"efgh"), 2).is_none(), "5 pinned + 6 needed > 10");
        let shared = admit(&mut cache, &prompt(b"abcdz"), 1).expect("shares 4 pinned blocks");
        assert_eq!(shared.hit_tokens, 4 * BLOCK_TOKENS);
        assert_eq!(cache.active_blocks(), 7);
    }

    #[test]
    fn lru_evicts_oldest_unpinned_suffix_first() {
        let mut cache = BlockCache::new(5);
        let cached = |cache: &BlockCache, blocks: &[u8]| {
            block_hashes(&prompt(blocks)).iter().map(|h| cache.blocks.contains_key(h)).collect::<Vec<_>>()
        };
        for text in [b"abc", b"def"] {
            let mut lease = cache.admit(block_hashes(&prompt(text)), 3 * BLOCK_TOKENS, 3, false).unwrap();
            cache.commit(&mut lease);
            cache.release(lease);
        }
        assert_eq!(cached(&cache, b"abc"), [true, true, false], "tail of the older release goes first");
        assert_eq!(cache.active_blocks(), 0);
        cache.admit(block_hashes(&prompt(b"gh")), 2 * BLOCK_TOKENS, 2, false).unwrap();
        assert_eq!(cached(&cache, b"abc"), [false, false, false]);
        assert_eq!(cached(&cache, b"def"), [true, true, true]);
    }

    #[test]
    fn hit_blocks_are_not_evicted_to_admit_their_owner() {
        let mut cache = BlockCache::new(4);
        run_to_completion(&mut cache, &prompt(b"abc"));
        let lease = admit(&mut cache, &prompt(b"abz"), 1).expect("2 hit + 2 new fits after evicting c");
        assert_eq!(lease.hit_tokens, 2 * BLOCK_TOKENS);
        assert!(cache.used() <= cache.capacity);
    }

    #[test]
    fn force_admits_oversized_sequence() {
        let mut cache = BlockCache::new(2);
        assert!(admit(&mut cache, &prompt(b"abcd"), 0).is_none());
        let tokens = 4 * BLOCK_TOKENS;
        let mut lease = cache.admit(block_hashes(&prompt(b"abcd")), tokens, 4, true).unwrap();
        cache.commit(&mut lease);
        cache.release(lease);
        assert_eq!(cache.active_blocks(), 0);
    }
}
