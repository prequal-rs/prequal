//! `--queue-order`: the priority a router stamps on each request for engines running vLLM's
//! `--scheduling-policy priority` (see `prequal_llm::queue_order`), from a predicted output length. The cost is the
//! prediction, or with `+kv` its `kv_cost`; the priority is the cost alone (shortest job first) or, with
//! `@<seconds>`, `arrival + Deadline::delay(cost)`, bounding how long a long job can be overtaken.

use std::{fmt, str::FromStr, time::Duration};

use prequal_llm::{
    Prompt,
    queue_order::{Deadline, HistoryKeys, OutputHistory, kv_cost},
};
use rand::{SeedableRng, rngs::SmallRng};

use crate::workload::{LogNormal, derive};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Predictor {
    /// The true output length.
    Oracle,
    /// The true length times log-normal noise of this sigma.
    Noisy(f64),
    /// `OutputHistory`, learning from this router's completions.
    History,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QueueOrder {
    predictor: Predictor,
    kv: bool,
    handicap: Option<Duration>,
}

impl FromStr for QueueOrder {
    type Err = String;

    /// `oracle`, `noisy:<sigma>` or `history`, optionally followed by `+kv`, then `@<seconds>`.
    fn from_str(s: &str) -> Result<Self, String> {
        let invalid = |e: &dyn fmt::Display| format!("{s:?}: {e}");
        let (name, handicap) = match s.split_once('@') {
            Some((name, secs)) => (name, Some(Duration::from_secs_f64(secs.parse().map_err(|e| invalid(&e))?))),
            None => (s, None),
        };
        let (name, kv) = match name.strip_suffix("+kv") {
            Some(name) => (name, true),
            None => (name, false),
        };
        let predictor = match name.split_once(':') {
            None if name == "oracle" => Predictor::Oracle,
            None if name == "history" => Predictor::History,
            Some(("noisy", sigma)) => Predictor::Noisy(sigma.parse().map_err(|e| invalid(&e))?),
            _ => return Err(invalid(&"not oracle, noisy:<sigma> or history (then optionally +kv, @<seconds>)")),
        };
        Ok(Self { predictor, kv, handicap })
    }
}

impl fmt::Display for QueueOrder {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.predictor {
            Predictor::Oracle => write!(f, "oracle")?,
            Predictor::Noisy(sigma) => write!(f, "noisy:{sigma}")?,
            Predictor::History => write!(f, "history")?,
        }
        if self.kv {
            write!(f, "+kv")?;
        }
        match self.handicap {
            Some(handicap) => write!(f, "@{}", handicap.as_secs_f64()),
            None => Ok(()),
        }
    }
}

/// One router's predictor state.
pub struct Orderer {
    order: QueueOrder,
    rng: SmallRng,
    history: OutputHistory,
    deadline: Option<Deadline>,
}

impl Orderer {
    pub fn new(order: QueueOrder, seed: u64) -> Self {
        Self {
            order,
            rng: SmallRng::seed_from_u64(derive(seed, 7, 0)),
            history: OutputHistory::default(),
            deadline: order.handicap.map(Deadline::new),
        }
    }

    /// `prompt` as the router sees it (estimated tokens); `output` is the true length, for the oracles.
    pub fn priority(&mut self, arrival_us: u64, prompt: &Prompt, output: u64) -> u64 {
        let predicted = match self.order.predictor {
            Predictor::Oracle => Some(output as f64),
            Predictor::Noisy(sigma) => {
                Some(LogNormal { median: output as f64, sigma, max: f64::MAX }.sample(&mut self.rng) as f64)
            }
            Predictor::History => self.history.predict(&OutputHistory::keys(prompt)),
        };
        let cost = predicted.map(|p| if self.order.kv { kv_cost(prompt.tokens, p) } else { p });
        match &mut self.deadline {
            Some(deadline) => arrival_us + deadline.delay(cost).as_micros() as u64,
            None => cost.unwrap_or(0.0).round() as u64,
        }
    }

    pub fn observe(&mut self, keys: &HistoryKeys, output: u64) {
        self.history.observe(keys, output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_displays_round_trip() {
        for spec in ["oracle", "noisy:0.8", "history", "history@20", "oracle@0.5", "history+kv", "oracle+kv@60"] {
            assert_eq!(spec.parse::<QueueOrder>().unwrap().to_string(), spec);
        }
        assert!("fifo".parse::<QueueOrder>().is_err());
        assert!("noisy".parse::<QueueOrder>().is_err());
    }

    #[test]
    fn kv_cost_orders_by_context_too() {
        let mut orderer = Orderer::new("oracle+kv".parse().unwrap(), 1);
        let (short, long) = (Prompt::from_text(&[b'a'; 400]), Prompt::from_text(&[b'a'; 40_000]));
        assert!(orderer.priority(0, &short, 100) < orderer.priority(0, &long, 50));
    }

    #[test]
    fn handicap_bounds_the_delay() {
        let mut orderer = Orderer::new("oracle@2".parse().unwrap(), 1);
        let prompt = Prompt::from_text(b"q");
        assert_eq!(orderer.priority(5_000, &prompt, 30), 5_000 + 1_000_000, "a lone cost ranks median");
        for output in 1..100 {
            assert!(orderer.priority(5_000, &prompt, output) <= 5_000 + 2_000_000);
        }
    }
}
