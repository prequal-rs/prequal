//! Per-stage summaries (human line + CSV row) and engine-side prefix-cache totals scraped from `/metrics`.
//! All latencies and rates are in simulated time: wall-clock divided by `--time-scale`.

use std::{fs::OpenOptions, io, io::Write as _, net::SocketAddr, path::Path, time::Duration};

use crate::load::Outcome;

pub const CSV_HEADER: &str = "policy,routers,engines,load,rps,completed,errors,ttft_p50_ms,ttft_p99_ms,e2e_p50_ms,\
                              e2e_p99_ms,tokens_per_s,workload,stage,stage_rate,concurrency,ttft_p90_ms,prefix_hit_rate,\
                              ttft_mean_ms,e2e_mean_ms";

/// Run-wide columns shared by every row.
pub struct RunInfo {
    pub policy: String,
    pub routers: usize,
    pub engines: usize,
    pub workload: &'static str,
    /// Simulated requests/s the fleet can decode at full batch; `load = rps / capacity_rps`.
    pub capacity_rps: f64,
    pub concurrency: usize,
    pub time_scale: f64,
}

pub struct StageResult {
    pub index: usize,
    /// Offered simulated rate; 0 for closed loop.
    pub rate: f64,
    /// Simulated duration.
    pub duration: Duration,
    pub outcomes: Vec<Outcome>,
}

struct Summary {
    rps: f64,
    completed: usize,
    errors: usize,
    ttft_ms: [f64; 3],
    e2e_ms: [f64; 2],
    /// TTFT and e2e.
    mean_ms: [f64; 2],
    tokens_per_s: f64,
    hit_rate: Option<f64>,
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted.get(((sorted.len().max(1) - 1) as f64 * p) as usize).copied().unwrap_or(f64::NAN)
}

fn sorted_ms(values: impl Iterator<Item = u64>, time_scale: f64) -> Vec<f64> {
    let mut ms: Vec<f64> = values.map(|us| us as f64 / 1e3 / time_scale).collect();
    ms.sort_unstable_by(f64::total_cmp);
    ms
}

fn summarize(stage: &StageResult, time_scale: f64) -> Summary {
    let done: Vec<_> = stage.outcomes.iter().filter_map(|o| o.as_ref().ok()).collect();
    let ttft = sorted_ms(done.iter().map(|c| c.ttft_us), time_scale);
    let e2e = sorted_ms(done.iter().map(|c| c.e2e_us), time_scale);
    let secs = stage.duration.as_secs_f64();
    let (prompt, cached) =
        done.iter().filter_map(|c| c.prefix).fold((0, 0), |(p, h), (prompt, cached)| (p + prompt, h + cached));
    Summary {
        rps: if stage.rate > 0.0 { stage.rate } else { stage.outcomes.len() as f64 / secs },
        completed: done.len(),
        errors: stage.outcomes.len() - done.len(),
        ttft_ms: [0.5, 0.9, 0.99].map(|p| percentile(&ttft, p)),
        e2e_ms: [0.5, 0.99].map(|p| percentile(&e2e, p)),
        mean_ms: [&ttft, &e2e].map(|ms| ms.iter().sum::<f64>() / ms.len() as f64),
        tokens_per_s: done.iter().map(|c| c.tokens).sum::<u64>() as f64 / secs,
        hit_rate: (prompt > 0).then(|| cached as f64 / prompt as f64),
    }
}

/// Prints one human line per stage and returns the matching CSV rows.
pub fn stage_rows(run: &RunInfo, stages: &[StageResult]) -> Vec<String> {
    stages
        .iter()
        .map(|stage| {
            let s = summarize(stage, run.time_scale);
            let hit = s.hit_rate.map_or_else(|| "n/a".to_owned(), |h| format!("{:.1}%", h * 100.0));
            println!(
                "stage {} ({:.1} rps x {:.0}s): {} ok / {} err, ttft p50/p90/p99 {:.0}/{:.0}/{:.0} ms, e2e p50/p99 \
                 {:.0}/{:.0} ms, {:.0} out tok/s, prefix hit {hit}",
                stage.index,
                s.rps,
                stage.duration.as_secs_f64(),
                s.completed,
                s.errors,
                s.ttft_ms[0],
                s.ttft_ms[1],
                s.ttft_ms[2],
                s.e2e_ms[0],
                s.e2e_ms[1],
                s.tokens_per_s,
            );
            format!(
                "{},{},{},{:.3},{:.1},{},{},{:.0},{:.0},{:.0},{:.0},{:.0},{},{},{:.2},{},{:.0},{},{:.0},{:.0}",
                run.policy,
                run.routers,
                run.engines,
                s.rps / run.capacity_rps,
                s.rps,
                s.completed,
                s.errors,
                s.ttft_ms[0],
                s.ttft_ms[2],
                s.e2e_ms[0],
                s.e2e_ms[1],
                s.tokens_per_s,
                run.workload,
                stage.index,
                stage.rate,
                run.concurrency,
                s.ttft_ms[1],
                s.hit_rate.map_or_else(String::new, |h| format!("{h:.4}")),
                s.mean_ms[0],
                s.mean_ms[1],
            )
        })
        .collect()
}

pub fn append_csv(path: &Path, rows: &[String]) -> io::Result<()> {
    let is_new = !path.exists();
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    if is_new {
        writeln!(file, "{CSV_HEADER}")?;
    }
    rows.iter().try_for_each(|row| writeln!(file, "{row}"))
}

fn counter(metrics: &str, name: &str) -> u64 {
    metrics
        .lines()
        .find(|line| line.starts_with(name) && line[name.len()..].starts_with(['{', ' ']))
        .and_then(|line| line.rsplit(' ').next()?.parse::<f64>().ok())
        .map_or(0, |v| v as u64)
}

/// Prints each engine's lifetime prompt and prefix-hit tokens (warmup included) from its `/metrics`.
pub async fn print_engine_totals(engines: &[SocketAddr]) {
    let client = reqwest::Client::new();
    let mut totals = Vec::new();
    for addr in engines {
        let text = match client.get(format!("http://{addr}/metrics")).send().await {
            Ok(response) => response.text().await.unwrap_or_default(),
            Err(_) => String::new(),
        };
        totals
            .push((counter(&text, "vllm:prefix_cache_queries_total"), counter(&text, "vllm:prefix_cache_hits_total")));
    }
    print_totals(&totals);
}

/// Prints per-engine `(prompt tokens, cached tokens)` totals and the fleet hit rate.
pub fn print_totals(totals: &[(u64, u64)]) {
    let (mut prompt_total, mut hit_total, mut per_engine) = (0, 0, Vec::new());
    for &(prompt, hits) in totals {
        prompt_total += prompt;
        hit_total += hits;
        per_engine.push(format!("{}k/{:.0}%", prompt / 1000, hits as f64 * 100.0 / prompt.max(1) as f64));
    }
    println!(
        "engines: {prompt_total} prompt tokens, {hit_total} cached ({:.1}%); per engine prompt/hit: {}",
        hit_total as f64 * 100.0 / prompt_total.max(1) as f64,
        per_engine.join(" "),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_matches_exact_metric_name() {
        let text = "vllm:prefix_cache_hits_total{engine=\"0\"} 42\nvllm:prefix_cache_queries_total{engine=\"0\"} 100\n";
        assert_eq!(counter(text, "vllm:prefix_cache_queries_total"), 100);
        assert_eq!(counter(text, "vllm:prefix_cache_hits_total"), 42);
        assert_eq!(counter(text, "vllm:prefix_cache_hits"), 0);
    }
}
