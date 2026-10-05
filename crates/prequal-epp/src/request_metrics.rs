//! llm-d's per-request series (`llm_d_epp_request_*`), labelled `model_name`, `target_model_name` (the same: no
//! model rewrite), `fairness_id` and `priority`, with llm-d's buckets and recording points.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Write as _,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

/// Distinct values kept per label before further ones read `other`, as in llm-d.
const LABEL_VALUE_LIMIT: usize = 1000;
/// Bytes kept of a label value: both labels are client-supplied, and every scrape repeats them per series.
const LABEL_VALUE_BYTES: usize = 256;
/// The flow a request belongs to (only a metric label here: prequal-epp has no flow control).
pub const FAIRNESS_HEADERS: [&str; 2] = ["x-llm-d-inference-fairness-id", "x-gateway-inference-fairness-id"];
pub const DEFAULT_FAIRNESS_ID: &str = "default-flow";

const LATENCY_BUCKETS: [f64; 35] = [
    0.005, 0.025, 0.05, 0.1, 0.2, 0.4, 0.6, 0.8, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0, 15.0, 20.0, 30.0,
    45.0, 60.0, 120.0, 180.0, 240.0, 300.0, 360.0, 480.0, 600.0, 900.0, 1200.0, 1800.0, 2700.0, 3600.0,
];
/// 64 B to 1 GiB in powers of two.
const REQUEST_SIZE_BUCKETS: [f64; 25] = {
    let mut buckets = [0.0; 25];
    let mut i = 0;
    while i < 25 {
        buckets[i] = (64u64 << i) as f64;
        i += 1;
    }
    buckets
};
const RESPONSE_SIZE_BUCKETS: [f64; 15] =
    [1.0, 8.0, 16.0, 32.0, 64.0, 128.0, 256.0, 512.0, 1024.0, 2048.0, 4096.0, 8192.0, 16384.0, 32768.0, 65536.0];

pub static REQUESTS: LazyLock<RequestMetrics> = LazyLock::new(RequestMetrics::default);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Labels {
    model: Arc<str>,
    fairness: Arc<str>,
    priority: i32,
}

#[derive(Default)]
struct Histogram {
    /// Per-bucket (not cumulative) counts; the last slot is +Inf.
    counts: Vec<u64>,
    sum: f64,
}

impl Histogram {
    fn observe(&mut self, bounds: &[f64], value: f64) {
        self.counts.resize(bounds.len() + 1, 0);
        self.counts[bounds.partition_point(|&b| b < value)] += 1;
        self.sum += value;
    }

    fn render(&self, out: &mut String, name: &str, labels: &str, bounds: &[f64]) {
        if self.counts.is_empty() {
            return;
        }
        let mut cumulative = 0;
        for (bound, count) in bounds.iter().map(ToString::to_string).chain(["+Inf".into()]).zip(&self.counts) {
            cumulative += count;
            let _ = writeln!(out, "{name}_bucket{{{labels},le=\"{bound}\"}} {cumulative}");
        }
        let _ = writeln!(out, "{name}_sum{{{labels}}} {}\n{name}_count{{{labels}}} {cumulative}", self.sum);
    }
}

#[derive(Default)]
struct Series {
    /// Has a model name: llm-d tracks `request_running` only for those.
    named: bool,
    total: u64,
    running: i64,
    errors: BTreeMap<&'static str, u64>,
    request_size: Histogram,
    duration: Histogram,
    response_size: Histogram,
}

/// Interned label values, capped at [`LABEL_VALUE_LIMIT`].
#[derive(Default)]
struct Values(Mutex<HashSet<Arc<str>>>);

impl Values {
    fn get(&self, value: &str) -> Arc<str> {
        let end = (0..=value.len().min(LABEL_VALUE_BYTES)).rev().find(|&i| value.is_char_boundary(i)).unwrap_or(0);
        let value = &value[..end];
        let mut values = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(known) = values.get(value) {
            return Arc::clone(known);
        }
        let value: Arc<str> = if values.len() < LABEL_VALUE_LIMIT { value.into() } else { "other".into() };
        values.insert(Arc::clone(&value));
        value
    }
}

#[derive(Default)]
pub struct RequestMetrics {
    series: Mutex<HashMap<Labels, Series>>,
    models: Values,
    fairness_ids: Values,
}

/// Holds one routed request in `llm_d_epp_request_running` until dropped.
pub struct Running(Labels);

impl Drop for Running {
    fn drop(&mut self) {
        REQUESTS.update(&self.0, |s| s.running -= 1);
    }
}

impl RequestMetrics {
    pub fn labels(&self, model: &str, fairness_id: &str, priority: i32) -> Labels {
        Labels { model: self.models.get(model), fairness: self.fairness_ids.get(fairness_id), priority }
    }

    fn update(&self, labels: &Labels, apply: impl FnOnce(&mut Series)) {
        let mut series = self.series.lock().unwrap_or_else(|p| p.into_inner());
        let named = !labels.model.is_empty();
        apply(series.entry(labels.clone()).or_insert_with(|| Series { named, ..Series::default() }));
    }

    /// A request was scheduled: `request_total`, `request_size_bytes` and (with a model) `request_running`.
    pub fn routed(&self, labels: &Labels, body_bytes: usize) -> Option<Running> {
        let mut named = false;
        self.update(labels, |s| {
            s.total += 1;
            s.request_size.observe(&REQUEST_SIZE_BUCKETS, body_bytes as f64);
            s.running += i64::from(s.named);
            named = s.named;
        });
        named.then(|| Running(labels.clone()))
    }

    /// `error_code` as llm-d names it: `ServiceUnavailable`, `ResourceExhausted`, `BadRequest`, `ModelServerError`.
    pub fn error(&self, labels: &Labels, error_code: &'static str) {
        self.update(labels, |s| *s.errors.entry(error_code).or_default() += 1);
    }

    /// The response completed `elapsed` after its request headers arrived.
    pub fn completed(&self, labels: &Labels, elapsed: Duration, response_bytes: usize) {
        self.update(labels, |s| {
            s.duration.observe(&LATENCY_BUCKETS, elapsed.as_secs_f64());
            s.response_size.observe(&RESPONSE_SIZE_BUCKETS, response_bytes as f64);
        });
    }

    pub fn render(&self, out: &mut String) {
        let series = self.series.lock().unwrap_or_else(|p| p.into_inner());
        let label_set = |l: &Labels| {
            let model = escape(&l.model);
            format!(
                "model_name=\"{model}\",target_model_name=\"{model}\",fairness_id=\"{}\",priority=\"{}\"",
                escape(&l.fairness),
                l.priority
            )
        };
        let rendered: Vec<(String, &Series)> = series.iter().map(|(l, s)| (label_set(l), s)).collect();
        let head = |out: &mut String, name: &str, kind: &str, help: &str| {
            let _ = writeln!(out, "# HELP llm_d_epp_{name} [ALPHA] {help}\n# TYPE llm_d_epp_{name} {kind}");
        };
        head(out, "request_total", "counter", "Total number of processed requests.");
        for (labels, s) in rendered.iter().filter(|(_, s)| s.total > 0) {
            let _ = writeln!(out, "llm_d_epp_request_total{{{labels}}} {}", s.total);
        }
        head(out, "request_error_total", "counter", "Total number of request errors.");
        for (labels, s) in &rendered {
            for (code, n) in &s.errors {
                let _ = writeln!(out, "llm_d_epp_request_error_total{{{labels},error_code=\"{code}\"}} {n}");
            }
        }
        head(out, "request_running", "gauge", "Current number of active running requests.");
        for (labels, s) in &rendered {
            if s.total > 0 && s.named {
                let _ = writeln!(out, "llm_d_epp_request_running{{{labels}}} {}", s.running);
            }
        }
        type Of = fn(&Series) -> &Histogram;
        let histograms: [(&str, &str, &[f64], Of); 3] = [
            (
                "request_duration_seconds",
                "End-to-end request latency distribution in seconds.",
                &LATENCY_BUCKETS,
                |s| &s.duration,
            ),
            ("request_size_bytes", "Request size distribution in bytes.", &REQUEST_SIZE_BUCKETS, |s| &s.request_size),
            ("response_size_bytes", "Response size distribution in bytes.", &RESPONSE_SIZE_BUCKETS, |s| {
                &s.response_size
            }),
        ];
        for (name, help, bounds, histogram) in histograms {
            head(out, name, "histogram", help);
            for (labels, s) in &rendered {
                histogram(s).render(out, &format!("llm_d_epp_{name}"), labels, bounds);
            }
        }
    }
}

/// A Prometheus label value: backslash, double quote and newline escaped.
pub fn escape(value: &str) -> String {
    value.replace('\\', r"\\").replace('"', "\\\"").replace('\n', r"\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_requests_errors_and_running_like_llm_d() {
        let metrics = RequestMetrics::default();
        let labels = metrics.labels("m\"x", DEFAULT_FAIRNESS_ID, -1);
        let running = metrics.routed(&labels, 100);
        metrics.completed(&labels, Duration::from_millis(30), 10);
        metrics.error(&labels, "ModelServerError");
        let unnamed = metrics.labels("", DEFAULT_FAIRNESS_ID, 0);
        assert!(metrics.routed(&unnamed, 1).is_none(), "no model, no running gauge");
        metrics.error(&unnamed, "ServiceUnavailable");
        let mut out = String::new();
        metrics.render(&mut out);
        let set = r#"model_name="m\"x",target_model_name="m\"x",fairness_id="default-flow",priority="-1""#;
        for line in [
            format!("llm_d_epp_request_total{{{set}}} 1"),
            format!("llm_d_epp_request_running{{{set}}} 1"),
            format!("llm_d_epp_request_error_total{{{set},error_code=\"ModelServerError\"}} 1"),
            format!("llm_d_epp_request_duration_seconds_bucket{{{set},le=\"0.025\"}} 0"),
            format!("llm_d_epp_request_duration_seconds_bucket{{{set},le=\"0.05\"}} 1"),
            format!("llm_d_epp_request_size_bytes_bucket{{{set},le=\"128\"}} 1"),
            format!("llm_d_epp_response_size_bytes_count{{{set}}} 1"),
            r#"llm_d_epp_request_error_total{model_name="",target_model_name="",fairness_id="default-flow",priority="0",error_code="ServiceUnavailable"} 1"#.into(),
        ] {
            assert!(out.contains(&line), "missing {line} in\n{out}");
        }
        assert!(!out.contains(r#"llm_d_epp_request_running{model_name="""#));
        drop(running);
        assert_eq!(REQUEST_SIZE_BUCKETS[24], 1_073_741_824.0);
    }

    #[test]
    fn caps_label_cardinality() {
        let values = Values::default();
        for i in 0..LABEL_VALUE_LIMIT {
            assert_eq!(&*values.get(&i.to_string()), i.to_string());
        }
        assert_eq!(&*values.get("one-too-many"), "other");
        assert_eq!(&*values.get("7"), "7");
    }

    #[test]
    fn truncates_long_label_values_on_a_char_boundary() {
        let values = Values::default();
        assert_eq!(values.get(&"a".repeat(10 * LABEL_VALUE_BYTES)).len(), LABEL_VALUE_BYTES);
        let kept = values.get(&format!("{}é", "a".repeat(LABEL_VALUE_BYTES - 1)));
        assert_eq!(kept.len(), LABEL_VALUE_BYTES - 1, "a split two-byte char is dropped whole");
    }
}
