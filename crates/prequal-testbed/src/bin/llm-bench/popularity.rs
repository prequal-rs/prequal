//! How often each shared-prefix group is drawn.

use std::str::FromStr;

use rand::{RngExt, rngs::SmallRng};

#[derive(Clone, Debug, PartialEq)]
pub enum Popularity {
    Uniform,
    Zipf(f64),
    /// Consecutive runs of groups `(count, share of requests)`, uniform within a run: `tools/kind-cache.sh`'s
    /// hot/warm/cold generators.
    Tiers(Vec<(usize, f64)>),
}

impl FromStr for Popularity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.split_once(':') {
            None if s == "uniform" => Ok(Self::Uniform),
            Some(("zipf", exponent)) => exponent.parse().map(Self::Zipf).map_err(|e| format!("zipf exponent: {e}")),
            Some(("tiers", tiers)) => tiers.split(',').map(parse_tier).collect::<Result<_, _>>().map(Self::Tiers),
            _ => Err(format!("popularity {s:?} is not `uniform`, `zipf:<s>` or `tiers:<groups>=<share>,...`")),
        }
    }
}

fn parse_tier(tier: &str) -> Result<(usize, f64), String> {
    let (groups, share) = tier.split_once('=').ok_or_else(|| format!("tier {tier:?} is not <groups>=<share>"))?;
    let groups: usize = groups.trim().parse().map_err(|e| format!("tier {tier:?} groups: {e}"))?;
    let share: f64 = share.trim().parse().map_err(|e| format!("tier {tier:?} share: {e}"))?;
    if groups == 0 || share.is_nan() || share <= 0.0 {
        return Err(format!("tier {tier:?} needs groups and a positive share"));
    }
    Ok((groups, share))
}

/// Draws a group index for the non-uniform popularities (uniform replays a fixed cycle instead).
pub struct GroupSampler {
    /// Cumulative request share per run, normalised to end at 1, and each run's `(first group, groups)`.
    cdf: Vec<f64>,
    runs: Vec<(usize, usize)>,
}

impl GroupSampler {
    /// `None` for [`Popularity::Uniform`].
    ///
    /// # Panics
    /// If tiers don't cover exactly `groups`.
    pub fn new(popularity: &Popularity, groups: usize) -> Option<Self> {
        let weighted: Vec<(usize, f64)> = match popularity {
            Popularity::Uniform => return None,
            Popularity::Zipf(s) => (1..=groups).map(|k| (1, (k as f64).powf(-s))).collect(),
            Popularity::Tiers(tiers) => tiers.clone(),
        };
        assert_eq!(weighted.iter().map(|t| t.0).sum::<usize>(), groups, "tiers must cover --groups exactly");
        let total: f64 = weighted.iter().map(|t| t.1).sum();
        let (mut acc, mut first) = (0.0, 0);
        let (cdf, runs) = weighted
            .iter()
            .map(|&(count, weight)| {
                acc += weight / total;
                first += count;
                (acc, (first - count, count))
            })
            .unzip();
        Some(Self { cdf, runs })
    }

    pub fn sample(&self, rng: &mut SmallRng) -> usize {
        let u = rng.random::<f64>();
        let (first, count) = self.runs[self.cdf.partition_point(|&c| c < u).min(self.runs.len() - 1)];
        if count == 1 { first } else { first + rng.random_range(0..count) }
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;

    use super::*;

    fn counts(popularity: &Popularity, groups: usize, draws: u32) -> Vec<u32> {
        let sampler = GroupSampler::new(popularity, groups).expect("non-uniform");
        let mut rng = SmallRng::seed_from_u64(3);
        let mut counts = vec![0u32; groups];
        (0..draws).for_each(|_| counts[sampler.sample(&mut rng)] += 1);
        counts
    }

    #[test]
    fn parses_and_rejects_garbage() {
        assert_eq!("zipf:1.1".parse(), Ok(Popularity::Zipf(1.1)));
        assert_eq!("uniform".parse(), Ok(Popularity::Uniform));
        assert_eq!("tiers:8=0.3,72=0.4".parse(), Ok(Popularity::Tiers(vec![(8, 0.3), (72, 0.4)])));
        for bad in ["zipf", "tiers:8", "tiers:0=1", "tiers:4=0", "tiers:4=x", "pareto:1"] {
            assert!(bad.parse::<Popularity>().is_err(), "{bad}");
        }
        assert!(GroupSampler::new(&Popularity::Uniform, 10).is_none());
    }

    #[test]
    fn zipf_skews_toward_low_ranks() {
        let counts = counts(&Popularity::Zipf(1.2), 100, 50_000);
        // Expected ratio between rank 1 and rank 51 is 51^1.2 ≈ 112.
        assert!(counts[0] > 20 * counts[50].max(1), "{} vs {}", counts[0], counts[50]);
        let (head, tail): (u32, u32) = (counts[..10].iter().sum(), counts[90..].iter().sum());
        assert!(head > 5 * tail, "{head} vs {tail}");
    }

    #[test]
    fn tiers_split_requests_by_share_and_spread_within() {
        let counts = counts(&Popularity::Tiers(vec![(2, 0.5), (8, 0.5)]), 10, 40_000);
        let hot: u32 = counts[..2].iter().sum();
        assert!((19_000..21_000).contains(&hot), "{hot}");
        assert!(counts[2..].iter().all(|&c| (2_000..3_000).contains(&c)), "{counts:?}");
    }
}
