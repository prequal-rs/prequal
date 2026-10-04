//! Prompt identity for prefix-cache affinity: the request body from its prompt field onward, cut into fixed-size
//! blocks whose chained hashes identify shared prefixes without tokenizing.

/// Bytes per block: 64 pseudo-tokens of 4 bytes, the granularity of llm-d's approximate index.
pub const BLOCK_BYTES: usize = 256;
/// Bytes per estimated token (OpenAI-style text averages about four).
pub const BYTES_PER_TOKEN: usize = 4;
pub const BLOCK_TOKENS: u64 = (BLOCK_BYTES / BYTES_PER_TOKEN) as u64;

const SEED: u64 = 0x9E37_79B9_7F4A_7C15;
const PROMPT_KEYS: [&[u8]; 2] = [b"\"prompt\"", b"\"messages\""];
/// Body fields that partition the engine's prefix cache, so they seed the block hashes (as llm-d's prefix index
/// seeds with the target model and `cache_salt`): LoRA adapters, models and salted tenants never share blocks.
const CACHE_KEYS: [&[u8]; 2] = [b"\"model\"", b"\"cache_salt\""];

/// A request's prompt as routing sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prompt {
    /// Chained hashes of the full blocks: equal hash at position i means equal first i+1 blocks.
    pub blocks: Vec<u64>,
    /// Estimated prompt tokens ([`BYTES_PER_TOKEN`] bytes each).
    pub tokens: u64,
}

impl Prompt {
    /// Reads the prompt region of an OpenAI-style JSON body (`/v1/completions` or `/v1/chat/completions`), with
    /// its hashes seeded by the body's `model` and `cache_salt`.
    pub fn from_body(body: &[u8]) -> Self {
        let start = prompt_start(body);
        let seed = CACHE_KEYS.iter().fold(SEED, |seed, key| match string_field(body, key, start) {
            Some(value) => hash_block(hash_block(seed, key), value),
            None => seed,
        });
        Self::seeded(seed, start.map_or(body, |s| &body[s..]))
    }

    /// The body's `model` as raw (still JSON-escaped) bytes, as [`Prompt::from_body`] seeds with it.
    pub fn model(body: &[u8]) -> Option<&[u8]> {
        string_field(body, CACHE_KEYS[0], prompt_start(body))
    }

    /// Hashes `text` as a whole prompt, unseeded (no model or salt).
    pub fn from_text(text: &[u8]) -> Self {
        Self::seeded(SEED, text)
    }

    fn seeded(mut prev: u64, text: &[u8]) -> Self {
        let blocks = text
            .chunks_exact(BLOCK_BYTES)
            .map(|block| {
                prev = hash_block(prev, block);
                prev
            })
            .collect();
        Self { blocks, tokens: text.len().div_ceil(BYTES_PER_TOKEN) as u64 }
    }
}

/// The body from the first `"prompt"` or `"messages"` key onward, so fields that precede it (model, sampling
/// options) can't split affinity, and bodies need no JSON parsing. Falls back to the whole body.
pub fn prompt_region(body: &[u8]) -> &[u8] {
    prompt_start(body).map_or(body, |start| &body[start..])
}

fn prompt_start(body: &[u8]) -> Option<usize> {
    find_any(body, &PROMPT_KEYS)
}

/// Output tokens assumed when a request doesn't set `max_tokens`.
pub const DEFAULT_MAX_TOKENS: u64 = 256;

/// `max_tokens` from a JSON body without parsing it all (prompts can be megabytes).
pub fn max_tokens(body: &[u8]) -> Option<u64> {
    uint_field(body, b"\"max_tokens\"")
}

/// `usage.completion_tokens` from a response body or streamed chunk, as [`max_tokens`] reads requests.
pub fn completion_tokens(body: &[u8]) -> Option<u64> {
    uint_field(body, b"\"completion_tokens\"")
}

/// The first unsigned integer value of quoted JSON `key`.
fn uint_field(body: &[u8], key: &[u8]) -> Option<u64> {
    let start = find(body, key)? + key.len();
    let digits: Vec<u8> = body[start..]
        .iter()
        .skip_while(|b| b.is_ascii_whitespace() || **b == b':')
        .take_while(|b| b.is_ascii_digit())
        .copied()
        .collect();
    std::str::from_utf8(&digits).ok()?.parse().ok()
}

fn find(body: &[u8], key: &[u8]) -> Option<usize> {
    find_any(body, &[key])
}

/// First position of any of `keys` (quoted JSON keys such as `"model"`), jumping quote to quote: one scan, not a
/// window compare at every offset per key.
fn find_any(body: &[u8], keys: &[&[u8]]) -> Option<usize> {
    let mut from = 0;
    while let Some(at) = next_quote(body, from) {
        if keys.iter().any(|key| body[at..].starts_with(key)) {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

fn rfind(body: &[u8], key: &[u8]) -> Option<usize> {
    let mut to = body.len();
    while let Some(at) = prev_quote(body, to) {
        if body[at..].starts_with(key) {
            return Some(at);
        }
        to = at;
    }
    None
}

const WORD: usize = 8;

/// One high bit per `"` byte in an 8-byte word (exact, unlike the borrow-based zero-byte test).
fn quote_bits(word: [u8; WORD]) -> u64 {
    const LOW7: u64 = 0x7F7F_7F7F_7F7F_7F7F;
    let x = u64::from_le_bytes(word) ^ u64::from_le_bytes([b'"'; WORD]);
    !(((x & LOW7) + LOW7) | x | LOW7)
}

/// Index of the first `"` at or after `from`, a word at a time.
fn next_quote(body: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while let Some(word) = body.get(i..i + WORD) {
        let bits = quote_bits(word.try_into().expect("a word"));
        if bits != 0 {
            return Some(i + bits.trailing_zeros() as usize / WORD);
        }
        i += WORD;
    }
    body[i.min(body.len())..].iter().position(|&b| b == b'"').map(|p| i + p)
}

/// Index of the last `"` before `to`, a word at a time.
fn prev_quote(body: &[u8], to: usize) -> Option<usize> {
    let mut end = to;
    while end >= WORD {
        let bits = quote_bits(body[end - WORD..end].try_into().expect("a word"));
        if bits != 0 {
            return Some(end - WORD + (63 - bits.leading_zeros() as usize) / WORD);
        }
        end -= WORD;
    }
    body[..end].iter().rposition(|&b| b == b'"')
}

/// The raw (still escaped) string value of a top-level-looking `key`. Searched before the prompt first, then from
/// the end backwards, since clients put such fields either before or after the (possibly huge) prompt.
fn string_field<'a>(body: &'a [u8], key: &[u8], prompt_start: Option<usize>) -> Option<&'a [u8]> {
    let head = prompt_start.unwrap_or(body.len());
    let at = find(&body[..head], key).or_else(|| Some(head + rfind(&body[head..], key)?))?;
    let rest = body[at + key.len()..].trim_ascii_start().strip_prefix(b":")?.trim_ascii_start().strip_prefix(b"\"")?;
    let mut escaped = false;
    let end = rest.iter().position(|&b| {
        let closes = b == b'"' && !escaped;
        escaped = b == b'\\' && !escaped;
        closes
    })?;
    Some(&rest[..end])
}

/// FxHash-style fold over 8-byte words, finalized with a SplitMix64 mix so near-identical blocks diverge.
fn hash_block(prev: u64, block: &[u8]) -> u64 {
    let mut h = prev;
    for word in block.chunks(8) {
        let mut bytes = [0u8; 8];
        bytes[..word.len()].copy_from_slice(word);
        h = (h.rotate_left(5) ^ u64::from_le_bytes(bytes)).wrapping_mul(0x51_7C_C1_B7_27_22_0A_95);
    }
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^ (h >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(seed: u8, len: usize) -> Vec<u8> {
        (0..len).map(|i| b'a' + ((i * 7 + usize::from(seed) * 13) % 26) as u8).collect()
    }

    fn long_prompt() -> String {
        String::from_utf8(text(4, BLOCK_BYTES * 2)).unwrap()
    }

    fn blocks(body: &str) -> Vec<u64> {
        Prompt::from_body(body.as_bytes()).blocks
    }

    #[test]
    fn shared_prefixes_share_leading_hashes() {
        let system = text(1, BLOCK_BYTES * 3);
        let a = [system.as_slice(), &text(2, BLOCK_BYTES * 2)].concat();
        let b = [system.as_slice(), &text(3, BLOCK_BYTES * 2)].concat();
        let (a, b) = (Prompt::from_text(&a), Prompt::from_text(&b));
        assert_eq!(a.blocks.len(), 5);
        assert_eq!(a.blocks[..3], b.blocks[..3]);
        assert!(a.blocks[3..].iter().zip(&b.blocks[3..]).all(|(x, y)| x != y));
        assert_eq!(a.tokens, (BLOCK_BYTES * 5 / BYTES_PER_TOKEN) as u64);
    }

    #[test]
    fn a_later_block_depends_on_everything_before_it() {
        let a = [text(1, BLOCK_BYTES), text(9, BLOCK_BYTES)].concat();
        let b = [text(2, BLOCK_BYTES), text(9, BLOCK_BYTES)].concat();
        assert_ne!(Prompt::from_text(&a).blocks[1], Prompt::from_text(&b).blocks[1]);
    }

    #[test]
    fn reads_max_tokens() {
        assert_eq!(max_tokens(br#"{"prompt":"x","max_tokens": 128,"stream":true}"#), Some(128));
        assert_eq!(max_tokens(br#"{"max_tokens":7}"#), Some(7));
        assert_eq!(max_tokens(br#"{"prompt":"x"}"#), None);
    }

    #[test]
    fn region_skips_fields_before_the_prompt() {
        let prompt = long_prompt();
        let a = format!(r#"{{"model":"m","max_tokens":10,"prompt":"{prompt}"}}"#);
        let b = format!(r#"{{"model":"m","max_tokens":999,"temperature":0.2,"prompt":"{prompt}"}}"#);
        assert_eq!(blocks(&a), blocks(&b));
        let chat = r#"{"model":"m","messages":[{"role":"user","content":"hi"}]}"#;
        assert!(prompt_region(chat.as_bytes()).starts_with(b"\"messages\""));
        assert_eq!(prompt_region(b"no keys"), b"no keys");
    }

    #[test]
    fn models_and_adapters_never_share_blocks() {
        let prompt = long_prompt();
        let base = blocks(&format!(r#"{{"model":"llama","prompt":"{prompt}"}}"#));
        let lora = blocks(&format!(r#"{{"model":"llama-lora-a","prompt":"{prompt}"}}"#));
        assert_eq!(base.len(), 2);
        assert!(base.iter().zip(&lora).all(|(a, b)| a != b));
        // Without a model the seed is the plain one, as for raw text.
        let bare = format!(r#"{{"prompt":"{prompt}"}}"#);
        assert_eq!(blocks(&bare), Prompt::from_text(prompt_region(bare.as_bytes())).blocks);
    }

    #[test]
    fn model_is_found_after_the_messages_too() {
        // openai-python serializes `messages` before `model`.
        let prompt = long_prompt();
        let chat = |head: &str, tail: &str| {
            blocks(&format!(r#"{{{head}"messages":[{{"role":"user","content":"{prompt}"}}]{tail}}}"#))
        };
        let last = chat("", r#","model":"m","stream":true"#);
        assert_eq!(chat(r#""model":"m","#, r#","stream":true"#), last, "same model, same region");
        assert_ne!(chat("", r#","model":"n""#), last);
    }

    #[test]
    fn word_scans_find_every_quote_like_a_byte_scan() {
        let body: Vec<u8> =
            (0..300u32).map(|i| if (i * 37 + i / 7) % 11 == 0 { b'"' } else { b'a' + (i % 26) as u8 }).collect();
        for at in 0..=body.len() {
            assert_eq!(next_quote(&body, at), body[at..].iter().position(|&b| b == b'"').map(|p| at + p), "{at}");
            assert_eq!(prev_quote(&body, at), body[..at].iter().rposition(|&b| b == b'"'), "{at}");
        }
        assert_eq!(quote_bits(*b"\"\x00\xa2\"!#\x22\xff"), 0x0080_0000_8000_0080);
    }

    #[test]
    fn cache_salt_partitions_and_values_parse_escapes() {
        let prompt = long_prompt();
        let salted = |salt: &str| blocks(&format!(r#"{{"model":"m","cache_salt":"{salt}","prompt":"{prompt}"}}"#));
        assert_ne!(salted("tenant-a"), salted("tenant-b"));
        assert_ne!(salted("tenant-a"), blocks(&format!(r#"{{"model":"m","prompt":"{prompt}"}}"#)));
        let model = |body: &'static [u8]| string_field(body, b"\"model\"", None);
        assert_eq!(model(br#"{"model" : "a\"b", "x":1}"#), Some(&br#"a\"b"#[..]));
        assert_eq!(model(br#"{"role":"model"}"#), None, "a value, not a key");
        assert_eq!(model(br#"{"model":"unterminated"#), None);
    }
}
