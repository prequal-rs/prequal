//! Per-step token scheduling, vLLM V1 style: decoders take one budget token each, then partially prefilled sequences
//! consume the remaining `max_num_batched_tokens` in admission order.

/// Prefill tokens each sequence processes this step; `prefill_left[i] == 0` means sequence `i` is decoding.
/// `budget: None` disables chunking: every pending prefill runs to completion in one step.
pub fn prefill_chunks(prefill_left: &[u64], budget: Option<u64>) -> Vec<u64> {
    let Some(budget) = budget else { return prefill_left.to_vec() };
    let decoding = prefill_left.iter().filter(|&&left| left == 0).count() as u64;
    let mut remaining = budget.saturating_sub(decoding);
    prefill_left
        .iter()
        .map(|&left| {
            let chunk = left.min(remaining);
            remaining -= chunk;
            chunk
        })
        .collect()
}

/// Whether a step whose running sequences need `prefill_left` still has budget for a newly admitted one.
pub fn has_spare_budget(prefill_left: impl Iterator<Item = u64>, budget: Option<u64>) -> bool {
    budget.is_none_or(|budget| prefill_left.map(|left| left.max(1)).sum::<u64>() < budget)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs steps until every prefill finishes; returns per-step (prefill tokens, token-emitting sequences).
    fn simulate(mut left: Vec<u64>, budget: Option<u64>) -> Vec<(u64, usize)> {
        let mut steps = Vec::new();
        while left.iter().any(|&l| l > 0) {
            let chunks = prefill_chunks(&left, budget);
            let emitting = left.iter().zip(&chunks).filter(|&(&l, &c)| l == c).count();
            left.iter_mut().zip(&chunks).for_each(|(l, c)| *l -= c);
            steps.push((chunks.iter().sum(), emitting));
        }
        steps
    }

    #[test]
    fn long_prompt_chunks_while_decoders_keep_emitting() {
        let steps = simulate(vec![0, 0, 20_000], Some(8192));
        assert_eq!(steps, [(8190, 2), (8190, 2), (3620, 3)], "first token only in the completing step");
    }

    #[test]
    fn prefills_share_budget_fifo() {
        assert_eq!(prefill_chunks(&[0, 3000, 3000, 3000], Some(8192)), [0, 3000, 3000, 2191]);
        assert_eq!(prefill_chunks(&[5000, 0], Some(2048)), [2047, 0]);
    }

    #[test]
    fn unchunked_prefills_everything_at_once() {
        assert_eq!(simulate(vec![0, 20_000, 9_000], None), [(29_000, 3)]);
    }

    #[test]
    fn admission_stops_when_budget_is_spoken_for() {
        assert!(has_spare_budget([0, 0, 100].into_iter(), Some(8192)));
        assert!(!has_spare_budget([0, 20_000].into_iter(), Some(8192)));
        assert!(has_spare_budget([0, 20_000].into_iter(), None));
    }
}
