//! Minimal Prometheus text-exposition reading: just the samples routing needs, without a parser dependency.

/// How samples of one metric are combined across label sets (e.g. several data-parallel engines).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Aggregate {
    /// Add them (counts).
    Sum,
    /// Take the largest (utilizations).
    Max,
}

/// Reads one metric from Prometheus text exposition, combining every sample whose name matches
/// exactly (labels ignored). Returns `None` if the metric is absent.
pub fn read_metric(exposition: &str, name: &str, aggregate: Aggregate) -> Option<f64> {
    let [value] = read_metrics(exposition, [(Some(name), aggregate)]);
    value
}

/// [`read_metric`] for several metrics in one pass over the exposition (scrapes run every 50 ms per replica, and
/// engines' expositions run to tens of KB). A `None` name reads nothing.
pub fn read_metrics<const N: usize>(exposition: &str, wanted: [(Option<&str>, Aggregate); N]) -> [Option<f64>; N] {
    let mut results: [Option<f64>; N] = [None; N];
    for line in exposition.lines() {
        let line = line.trim_start();
        if line.starts_with('#') {
            continue;
        }
        for ((name, aggregate), result) in wanted.iter().zip(&mut results) {
            let Some(value) = name.and_then(|name| sample_value(line, name)) else { continue };
            *result = Some(match (*result, aggregate) {
                (None, _) => value,
                (Some(acc), Aggregate::Sum) => acc + value,
                (Some(acc), Aggregate::Max) => acc.max(value),
            });
        }
    }
    results
}

/// The value of a sample line if it belongs to metric `name` exactly. Non-finite samples (`NaN`, `±Inf`, all valid
/// Prometheus values) are skipped: no routing score can use them.
fn sample_value(line: &str, name: &str) -> Option<f64> {
    let rest = line.strip_prefix(name)?;
    let after_labels = if let Some(labels) = rest.strip_prefix('{') {
        labels.split_once('}').map(|(_, after)| after)
    } else if rest.starts_with(char::is_whitespace) {
        Some(rest)
    } else {
        None // a longer metric name sharing this prefix
    };
    after_labels?.split_whitespace().next()?.parse().ok().filter(|v: &f64| v.is_finite())
}

/// The value of `label` on the first sample of metric `name` (info-style gauges such as `vllm:cache_config_info`).
pub fn read_label<'a>(exposition: &'a str, name: &str, label: &str) -> Option<&'a str> {
    exposition.lines().find_map(|line| {
        let labels = line.trim_start().strip_prefix(name)?.strip_prefix('{')?.split_once('}')?.0;
        labels.split(',').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key.trim() == label).then(|| value.trim().trim_matches('"'))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VLLM: &str = r#"
# HELP vllm:num_requests_running Number of requests in model execution batches.
# TYPE vllm:num_requests_running gauge
vllm:num_requests_running{engine="0",model_name="meta-llama/Llama-3.1-8B"} 3.0
vllm:num_requests_running{engine="1",model_name="meta-llama/Llama-3.1-8B"} 4.0
vllm:num_requests_waiting{engine="0",model_name="meta-llama/Llama-3.1-8B"} 2.0
vllm:num_requests_waiting_by_reason{engine="0",reason="capacity"} 99.0
vllm:kv_cache_usage_perc{engine="0",model_name="m"} 0.25
vllm:kv_cache_usage_perc{engine="1",model_name="m"} 0.5 1727200000000
plain_metric 7
"#;

    #[test]
    fn sums_and_maxes_across_label_sets() {
        assert_eq!(read_metric(VLLM, "vllm:num_requests_running", Aggregate::Sum), Some(7.0));
        assert_eq!(read_metric(VLLM, "vllm:num_requests_waiting", Aggregate::Sum), Some(2.0));
        assert_eq!(read_metric(VLLM, "vllm:kv_cache_usage_perc", Aggregate::Max), Some(0.5));
        assert_eq!(read_metric(VLLM, "plain_metric", Aggregate::Sum), Some(7.0));
    }

    #[test]
    fn ignores_prefix_collisions_and_missing_metrics() {
        assert_eq!(read_metric(VLLM, "vllm:num_requests", Aggregate::Sum), None);
        assert_eq!(read_metric(VLLM, "sglang:num_running_reqs", Aggregate::Sum), None);
    }

    #[test]
    fn skips_non_finite_samples() {
        let exposition = "m{e=\"0\"} NaN\nm{e=\"1\"} 2\nm{e=\"2\"} +Inf\nm{e=\"3\"} -Inf\nonly_nan NaN\n";
        assert_eq!(read_metric(exposition, "m", Aggregate::Sum), Some(2.0));
        assert_eq!(read_metric(exposition, "m", Aggregate::Max), Some(2.0));
        assert_eq!(read_metric(exposition, "only_nan", Aggregate::Sum), None);
    }

    #[test]
    fn reads_info_labels() {
        let info = "vllm:cache_config_info{block_size=\"16\",num_gpu_blocks=\"19187\"} 1.0\n";
        assert_eq!(read_label(info, "vllm:cache_config_info", "num_gpu_blocks"), Some("19187"));
        assert_eq!(read_label(info, "vllm:cache_config_info", "block_size"), Some("16"));
        assert_eq!(read_label(info, "vllm:cache_config_info", "missing"), None);
        assert_eq!(read_label(VLLM, "vllm:cache_config_info", "block_size"), None);
    }
}
