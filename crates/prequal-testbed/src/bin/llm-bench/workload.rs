//! Request generators: heavy-tailed random lengths (token hints only) and inference-perf's `shared_prefix` with real
//! prompt bytes, plus the load-stage schedule.

use std::{str::FromStr, sync::Mutex, time::Duration};

use rand::{RngExt, SeedableRng, rngs::SmallRng, seq::SliceRandom};

pub use crate::popularity::Popularity;
use crate::{cache::TOKEN_BYTES, popularity::GroupSampler, trace::Trace};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stage {
    pub rate: f64,
    pub duration: Duration,
}

/// `rate:seconds` pairs, e.g. `15:50,3:20,10:20`.
#[derive(Clone, Debug)]
pub struct Stages(pub Vec<Stage>);

impl FromStr for Stages {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let parse = |pair: &str| {
            let (rate, secs) = pair.split_once(':').ok_or_else(|| format!("stage {pair:?} is not rate:seconds"))?;
            let rate: f64 = rate.trim().parse().map_err(|e| format!("stage {pair:?} rate: {e}"))?;
            let secs: f64 = secs.trim().parse().map_err(|e| format!("stage {pair:?} duration: {e}"))?;
            if rate <= 0.0 || secs <= 0.0 {
                return Err(format!("stage {pair:?} needs a positive rate and duration"));
            }
            Ok(Stage { rate, duration: Duration::from_secs_f64(secs) })
        };
        s.split(',').filter(|p| !p.trim().is_empty()).map(parse).collect::<Result<_, _>>().map(Self)
    }
}

/// Token-count distribution: `median · e^(σ·z)`, clamped to `[1, max]`.
#[derive(Clone, Copy)]
pub struct LogNormal {
    pub median: f64,
    pub sigma: f64,
    pub max: f64,
}

impl LogNormal {
    pub fn mean(&self) -> f64 {
        self.median * (self.sigma * self.sigma / 2.0).exp()
    }

    pub fn sample(&self, rng: &mut SmallRng) -> u64 {
        let (u1, u2) = (1.0 - rng.random::<f64>(), rng.random::<f64>());
        let z = (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos();
        (self.median * (self.sigma * z).exp()).clamp(1.0, self.max) as u64
    }
}

pub enum Prompt {
    Text(String),
    Tokens(u64),
}

pub struct Request {
    pub prompt: Prompt,
    pub max_tokens: u64,
    /// The shared-prefix group drawn.
    pub group: Option<usize>,
}

pub struct SharedPrefixParams {
    pub groups: usize,
    pub prompts_per_group: usize,
    pub system_len: usize,
    pub question_len: usize,
    pub output_len: u64,
    pub popularity: Popularity,
    pub unique_questions: bool,
}

pub struct SharedPrefix {
    params: SharedPrefixParams,
    systems: Vec<String>,
    questions: Vec<String>,
    /// inference-perf replays one seeded shuffle of all `groups × prompts_per_group` prompts cyclically.
    order: Vec<usize>,
    groups: Option<GroupSampler>,
}

pub enum Generator {
    Random { prompt: LogNormal, output: LogNormal },
    SharedPrefix(Box<SharedPrefix>),
    Trace(Box<Trace>),
}

impl Generator {
    pub fn shared_prefix(params: SharedPrefixParams, seed: u64) -> Self {
        let systems = (0..params.groups).map(|g| ascii(derive(seed, 1, g as u64), params.system_len)).collect();
        let prompts = params.groups * params.prompts_per_group;
        let questions = (0..prompts).map(|i| ascii(derive(seed, 2, i as u64), params.question_len)).collect();
        let mut order: Vec<usize> = (0..prompts).collect();
        order.shuffle(&mut SmallRng::seed_from_u64(derive(seed, 3, 0)));
        let groups = GroupSampler::new(&params.popularity, params.groups);
        Self::SharedPrefix(Box::new(SharedPrefix { params, systems, questions, order, groups }))
    }

    pub fn mean_output(&self) -> f64 {
        match self {
            Self::Random { output, .. } => output.mean(),
            Self::SharedPrefix(sp) => sp.params.output_len as f64,
            Self::Trace(trace) => trace.mean_output(),
        }
    }

    /// The `index`-th request of the run.
    pub fn request(&self, index: u64, rng: &mut SmallRng) -> Request {
        match self {
            Self::Random { prompt, output } => {
                Request { prompt: Prompt::Tokens(prompt.sample(rng)), max_tokens: output.sample(rng), group: None }
            }
            Self::SharedPrefix(sp) => sp.request(index, rng),
            Self::Trace(trace) => {
                let (text, max_tokens) = trace.request(index);
                Request { prompt: Prompt::Text(text), max_tokens, group: None }
            }
        }
    }
}

impl SharedPrefix {
    fn request(&self, index: u64, rng: &mut SmallRng) -> Request {
        let ppg = self.params.prompts_per_group;
        let prompt = match &self.groups {
            None => self.order[index as usize % self.order.len()],
            Some(groups) => groups.sample(rng) * ppg + rng.random_range(0..ppg),
        };
        let mut text = self.systems[prompt / ppg].clone();
        match self.params.unique_questions {
            true => text.push_str(&ascii(rng.random(), self.params.question_len)),
            false => text.push_str(&self.questions[prompt]),
        }
        Request { prompt: Prompt::Text(text), max_tokens: self.params.output_len, group: Some(prompt / ppg) }
    }
}

/// A generator shared by concurrent senders, numbering requests in issue order.
pub struct Source {
    generator: Generator,
    state: Mutex<(SmallRng, u64)>,
}

impl Source {
    pub fn new(generator: Generator, seed: u64) -> Self {
        Self { generator, state: Mutex::new((SmallRng::seed_from_u64(derive(seed, 4, 0)), 0)) }
    }

    pub fn next(&self) -> Request {
        let mut state = self.state.lock().unwrap();
        let (rng, index) = &mut *state;
        *index += 1;
        self.generator.request(*index - 1, rng)
    }

    /// Arrival times in µs: the trace's when replaying one, else Poisson at each stage's rate.
    pub fn arrivals_us(&self, stages: &[Stage], seed: u64) -> Vec<u64> {
        match &self.generator {
            Generator::Trace(trace) => trace.arrivals_us(),
            _ => poisson_arrivals_us(stages, seed),
        }
    }
}

/// Arrival times in µs: memoryless arrivals redrawn at each stage's rate, the HTTP load generator's draw sequence.
pub fn poisson_arrivals_us(stages: &[Stage], seed: u64) -> Vec<u64> {
    let mut rng = SmallRng::seed_from_u64(seed);
    let (mut start, mut arrivals) = (0.0, Vec::new());
    for stage in stages {
        let (end, mut next) = (start + stage.duration.as_secs_f64(), start);
        loop {
            next += -(1.0 - rng.random::<f64>()).ln() / stage.rate;
            if next >= end {
                break;
            }
            arrivals.push((next * 1e6) as u64);
        }
        start = end;
    }
    arrivals
}

pub fn derive(seed: u64, stream: u64, index: u64) -> u64 {
    let mut z = seed ^ stream.rotate_left(48) ^ index.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// `tokens` pseudo-tokens of JSON-safe ASCII drawn deterministically from `seed`.
pub fn ascii(seed: u64, tokens: usize) -> String {
    const ALPHABET: &[u8; 64] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 .";
    let mut rng = SmallRng::seed_from_u64(seed);
    (0..tokens * TOKEN_BYTES).map(|_| ALPHABET[rng.random_range(0..64)] as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(popularity: Popularity, unique_questions: bool) -> SharedPrefixParams {
        SharedPrefixParams {
            groups: 20,
            prompts_per_group: 3,
            system_len: 32,
            question_len: 8,
            output_len: 10,
            popularity,
            unique_questions,
        }
    }

    fn texts(generator: &Generator, n: u64, seed: u64) -> Vec<String> {
        let mut rng = SmallRng::seed_from_u64(seed);
        (0..n)
            .map(|i| match generator.request(i, &mut rng).prompt {
                Prompt::Text(text) => text,
                Prompt::Tokens(_) => panic!("shared-prefix sends text"),
            })
            .collect()
    }

    #[test]
    fn stages_parse_and_reject_garbage() {
        let Stages(stages) = "15:50, 3:20.5,".parse().unwrap();
        assert_eq!(
            stages,
            [
                Stage { rate: 15.0, duration: Duration::from_secs(50) },
                Stage { rate: 3.0, duration: Duration::from_secs_f64(20.5) }
            ]
        );
        for bad in ["15", "a:1", "1:b", "0:5", "5:-1"] {
            assert!(bad.parse::<Stages>().is_err(), "{bad}");
        }
    }

    #[test]
    fn shared_prefix_is_deterministic_with_group_prefixes() {
        let a = texts(&Generator::shared_prefix(params(Popularity::Uniform, false), 7), 60, 0);
        assert_eq!(a, texts(&Generator::shared_prefix(params(Popularity::Uniform, false), 7), 60, 99));
        assert_ne!(a, texts(&Generator::shared_prefix(params(Popularity::Uniform, false), 8), 60, 0));
        assert!(a.iter().all(|t| t.len() == (32 + 8) * TOKEN_BYTES && t.is_ascii()));
        let system_bytes = 32 * TOKEN_BYTES;
        let mut systems: Vec<&str> = a.iter().map(|t| &t[..system_bytes]).collect();
        systems.sort_unstable();
        systems.dedup();
        assert_eq!(systems.len(), 20);
    }

    #[test]
    fn uniform_cycles_every_prompt_once_per_pass() {
        let generator = Generator::shared_prefix(params(Popularity::Uniform, false), 1);
        let mut pass = texts(&generator, 60, 0);
        assert_eq!(pass, texts(&generator, 120, 0)[60..]);
        pass.sort_unstable();
        pass.dedup();
        assert_eq!(pass.len(), 60);
    }

    #[test]
    fn unique_questions_never_repeat() {
        let generator = Generator::shared_prefix(params(Popularity::Uniform, true), 1);
        let mut all = texts(&generator, 120, 0);
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 120);
    }
}
