//! Prometheus `/metrics` on its own port. Series llm-d's dashboards, alerts and autoscalers read use llm-d's names,
//! labels and buckets (`llm_d_epp_*`); prequal's own are `prequal_epp_*`.

use std::{
    fmt::Write as _,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Semaphore,
};

/// `/metrics` connections served at once, and how long one may take from accept to the last byte.
const MAX_SCRAPES: usize = 32;
const SCRAPE_TIMEOUT: Duration = Duration::from_secs(10);
const SCHEDULER_LATENCY: &str = "llm_d_epp_scheduler_e2e_duration_seconds";
const BUCKETS: [f64; 10] = [0.0001, 0.0002, 0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1];

/// ext_proc message kinds as Envoy sends them, in `prequal_epp_ext_proc_messages_total` label order.
pub const MESSAGE_KINDS: [&str; 6] =
    ["request_headers", "request_body", "request_trailers", "response_headers", "response_body", "response_trailers"];
/// gRPC status names as Go spells them (llm-d's `llm_d_epp_extproc_streams_total` `code` label), by code number.
const GRPC_CODES: [&str; 17] = [
    "OK",
    "Canceled",
    "Unknown",
    "InvalidArgument",
    "DeadlineExceeded",
    "NotFound",
    "AlreadyExists",
    "PermissionDenied",
    "ResourceExhausted",
    "FailedPrecondition",
    "Aborted",
    "OutOfRange",
    "Unimplemented",
    "Internal",
    "Unavailable",
    "DataLoss",
    "Unauthenticated",
];

pub struct Telemetry {
    buckets: [AtomicU64; BUCKETS.len()],
    count: AtomicU64,
    sum_nanos: AtomicU64,
    routed: AtomicU64,
    rejected: AtomicU64,
    messages: [AtomicU64; MESSAGE_KINDS.len()],
    message_bytes: [AtomicU64; MESSAGE_KINDS.len()],
    streams: [AtomicU64; GRPC_CODES.len()],
}

pub static TELEMETRY: Telemetry = Telemetry::new();

impl Telemetry {
    const fn new() -> Self {
        Self {
            buckets: [const { AtomicU64::new(0) }; BUCKETS.len()],
            count: AtomicU64::new(0),
            sum_nanos: AtomicU64::new(0),
            routed: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            messages: [const { AtomicU64::new(0) }; MESSAGE_KINDS.len()],
            message_bytes: [const { AtomicU64::new(0) }; MESSAGE_KINDS.len()],
            streams: [const { AtomicU64::new(0) }; GRPC_CODES.len()],
        }
    }

    /// Counts one inbound ext_proc message of `MESSAGE_KINDS[kind]` carrying `bytes` of body.
    pub fn record_message(&self, kind: usize, bytes: usize) {
        self.messages[kind].fetch_add(1, Ordering::Relaxed);
        self.message_bytes[kind].fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// Counts one finished ext_proc stream by its gRPC status code.
    pub fn record_stream(&self, code: tonic::Code) {
        let index = usize::try_from(code as i32).unwrap_or(2).min(GRPC_CODES.len() - 1);
        self.streams[index].fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_pick(&self, elapsed: Duration, routed: bool) {
        let secs = elapsed.as_secs_f64();
        for (bucket, bound) in self.buckets.iter().zip(BUCKETS) {
            if secs <= bound {
                bucket.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_nanos.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
        let outcome = if routed { &self.routed } else { &self.rejected };
        outcome.fetch_add(1, Ordering::Relaxed);
    }

    /// This process's series; `open_streams` is the number of ext_proc streams now open.
    pub fn render(&self, open_streams: usize, out: &mut String) {
        let count = self.count.load(Ordering::Relaxed);
        let _ =
            writeln!(out, "# HELP {SCHEDULER_LATENCY} [ALPHA] End-to-end scheduling latency distribution in seconds.");
        let _ = writeln!(out, "# TYPE {SCHEDULER_LATENCY} histogram");
        for (bucket, bound) in self.buckets.iter().zip(BUCKETS) {
            let _ = writeln!(out, "{SCHEDULER_LATENCY}_bucket{{le=\"{bound}\"}} {}", bucket.load(Ordering::Relaxed));
        }
        let _ = writeln!(out, "{SCHEDULER_LATENCY}_bucket{{le=\"+Inf\"}} {count}");
        let sum = self.sum_nanos.load(Ordering::Relaxed) as f64 / 1e9;
        let _ = writeln!(out, "{SCHEDULER_LATENCY}_sum {sum}\n{SCHEDULER_LATENCY}_count {count}");
        let _ = writeln!(out, "# TYPE llm_d_epp_extproc_streams_inflight gauge");
        let _ = writeln!(out, "llm_d_epp_extproc_streams_inflight {open_streams}");
        let _ = writeln!(out, "# TYPE llm_d_epp_extproc_streams_total counter");
        for (code, n) in GRPC_CODES.iter().zip(&self.streams) {
            let n = n.load(Ordering::Relaxed);
            if n > 0 {
                let _ = writeln!(out, "llm_d_epp_extproc_streams_total{{code=\"{code}\"}} {n}");
            }
        }
        let _ = writeln!(out, "# TYPE llm_d_epp_info gauge");
        let version = concat!("prequal-epp-", env!("CARGO_PKG_VERSION"));
        let commit = option_env!("PREQUAL_COMMIT").unwrap_or("");
        let _ = writeln!(out, "llm_d_epp_info{{commit=\"{commit}\",build_ref=\"{version}\"}} 1");
        let _ = writeln!(out, "# TYPE prequal_epp_picks_total counter");
        let _ = writeln!(out, "prequal_epp_picks_total{{result=\"routed\"}} {}", self.routed.load(Ordering::Relaxed));
        let _ =
            writeln!(out, "prequal_epp_picks_total{{result=\"rejected\"}} {}", self.rejected.load(Ordering::Relaxed));
        for (name, counters) in [("messages", &self.messages), ("message_body_bytes", &self.message_bytes)] {
            let _ = writeln!(out, "# TYPE prequal_epp_ext_proc_{name}_total counter");
            for (kind, counter) in MESSAGE_KINDS.iter().zip(counters) {
                let n = counter.load(Ordering::Relaxed);
                let _ = writeln!(out, "prequal_epp_ext_proc_{name}_total{{type=\"{kind}\"}} {n}");
            }
        }
        out.push_str(&process_metrics());
    }
}

/// `process_cpu_seconds_total` and `process_resident_memory_bytes` from procfs (Linux only).
fn process_metrics() -> String {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    // Fields after the parenthesised command name: utime and stime are the 12th and 13th.
    let fields: Vec<&str> = stat.rsplit_once(')').map_or(Vec::new(), |(_, rest)| rest.split_whitespace().collect());
    let ticks: f64 = [11, 12].iter().filter_map(|&i| fields.get(i)?.parse::<f64>().ok()).sum();
    let rss_pages: f64 = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse().ok())
        .unwrap_or(0.0);
    if fields.is_empty() {
        return String::new();
    }
    // Linux reports in 100 Hz clock ticks and 4 KiB pages on every architecture prequal-epp ships for.
    format!(
        "# TYPE process_cpu_seconds_total counter\nprocess_cpu_seconds_total {}\n\
         # TYPE process_resident_memory_bytes gauge\nprocess_resident_memory_bytes {}\n",
        ticks / 100.0,
        rss_pages * 4096.0
    )
}

/// Serves `GET /metrics` (any path) over plain HTTP/1.1, one `render()` per connection. Runs until the task is
/// dropped; failed accepts (e.g. out of descriptors) back off rather than end it.
pub async fn serve(listener: TcpListener, render: impl Fn() -> String + Send + Sync + 'static) {
    let render = std::sync::Arc::new(render);
    let scrapes = std::sync::Arc::new(Semaphore::new(MAX_SCRAPES));
    loop {
        let mut stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(e) => {
                eprintln!("prequal-epp: metrics accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        // Over the cap the connection is closed unanswered: the port is unauthenticated.
        let Ok(permit) = std::sync::Arc::clone(&scrapes).try_acquire_owned() else { continue };
        let render = std::sync::Arc::clone(&render);
        tokio::spawn(async move {
            let _permit = permit;
            let _ = tokio::time::timeout(SCRAPE_TIMEOUT, async {
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request).await;
                let body = render();
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/plain; version=0.0.4\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(body.as_bytes()).await;
            })
            .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_is_cumulative_under_llm_d_names() {
        let telemetry = Telemetry::new();
        telemetry.record_pick(Duration::from_micros(150), true);
        telemetry.record_pick(Duration::from_millis(30), false);
        telemetry.record_message(4, 120);
        telemetry.record_stream(tonic::Code::Cancelled);
        let mut text = String::new();
        telemetry.render(3, &mut text);
        assert!(text.contains("llm_d_epp_scheduler_e2e_duration_seconds_bucket{le=\"0.0002\"} 1"));
        assert!(text.contains(&format!("{SCHEDULER_LATENCY}_bucket{{le=\"0.05\"}} 2")));
        assert!(text.contains(&format!("{SCHEDULER_LATENCY}_count 2")));
        assert!(text.contains("llm_d_epp_extproc_streams_inflight 3"));
        assert!(text.contains("llm_d_epp_extproc_streams_total{code=\"Canceled\"} 1"));
        assert!(text.contains("prequal_epp_picks_total{result=\"rejected\"} 1"));
        assert!(text.contains("prequal_epp_ext_proc_message_body_bytes_total{type=\"response_body\"} 120"));
    }
}
