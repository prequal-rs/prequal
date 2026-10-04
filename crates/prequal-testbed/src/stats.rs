use std::{fs::OpenOptions, io::Write, path::Path};

use prequal_tower::ProbeCounts;

use prequal_testbed::metrics::ProcessUsage;

use crate::client::Outcome;

pub struct Summary {
    pub completed: usize,
    pub errors: usize,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p90_ms: f64,
    pub p99_ms: f64,
    pub p999_ms: f64,
}

pub const CSV_HEADER: &str = "policy,via,estimator,q_rif,probes_per_query,piggyback,failing_servers,cpu_work,servers,clients,load,completed,errors,mean_ms,p50_ms,p90_ms,p99_ms,p999_ms,\
client_cpu_us_per_req,client_peak_mb,server_cpu_us_per_req,server_peak_mb,probes_sent,probes_answered,probes_timed_out,probes_failed,probes_skipped";

impl Summary {
    pub fn from_outcomes(outcomes: &[Outcome]) -> Self {
        let mut ok: Vec<u64> = outcomes.iter().flatten().copied().collect();
        ok.sort_unstable();
        let pct = |p: f64| {
            if ok.is_empty() { f64::NAN } else { ok[((ok.len() - 1) as f64 * p) as usize] as f64 / 1e3 }
        };
        Self {
            completed: ok.len(),
            errors: outcomes.len() - ok.len(),
            mean_ms: ok.iter().sum::<u64>() as f64 / ok.len().max(1) as f64 / 1e3,
            p50_ms: pct(0.50),
            p90_ms: pct(0.90),
            p99_ms: pct(0.99),
            p999_ms: pct(0.999),
        }
    }

    /// Latency columns, then per-request CPU (µs) and peak memory for both processes, then probe counts.
    pub fn csv_fields(&self, client: ProcessUsage, server: ProcessUsage, probes: ProbeCounts) -> String {
        let per_req = |u: ProcessUsage| u.cpu_s * 1e6 / (self.completed + self.errors).max(1) as f64;
        format!(
            "{},{},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1},{:.1},{},{},{},{},{}",
            self.completed,
            self.errors,
            self.mean_ms,
            self.p50_ms,
            self.p90_ms,
            self.p99_ms,
            self.p999_ms,
            per_req(client),
            client.peak_mb,
            per_req(server),
            server.peak_mb,
            probes.sent,
            probes.answered,
            probes.timed_out,
            probes.failed,
            probes.skipped,
        )
    }
}

pub fn append_csv(path: &Path, row: &str) -> std::io::Result<()> {
    let is_new = !path.exists();
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    if is_new {
        writeln!(file, "{CSV_HEADER}")?;
    }
    writeln!(file, "{row}")
}
