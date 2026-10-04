use clap::{ArgAction, Args};

/// Flags the llm-d-router Helm charts and the GIE conformance manifests pass to every endpoint picker. Accepted —
/// repeatedly, as the chart repeats some (`--tracing=<nil> … --tracing=false`) — so prequal-epp is a drop-in image
/// swap; none of them change its behaviour, and [`IgnoredFlags::warnings`] says so for each one that asks for
/// something prequal-epp doesn't do.
#[derive(Args, Debug, Default)]
pub struct IgnoredFlags {
    #[arg(long, hide = true, action = ArgAction::Append)]
    zap_encoder: Vec<String>,
    #[arg(long, hide = true, action = ArgAction::Append)]
    zap_log_level: Vec<String>,
    #[arg(long, hide = true, action = ArgAction::Append)]
    config_file: Vec<String>,
    #[arg(long, hide = true, action = ArgAction::Append)]
    config_text: Vec<String>,
    #[arg(long, hide = true, action = ArgAction::Append)]
    tracing: Vec<String>,
    #[arg(long, hide = true, action = ArgAction::Append)]
    metrics_endpoint_auth: Vec<String>,
    #[arg(long, hide = true, action = ArgAction::Append)]
    v: Vec<String>,
    #[arg(long, hide = true, action = ArgAction::Append)]
    enable_pprof: Vec<String>,
    #[arg(long, hide = true, action = ArgAction::Append, num_args = 0..=1, default_missing_value = "true")]
    ha_enable_leader_election: Vec<String>,
}

/// The effective value of a repeatable Go flag: the last one, skipping the chart's unset `<nil>` renderings.
fn last(values: &[String]) -> Option<&str> {
    values.iter().rev().map(|v| v.trim()).find(|v| !v.is_empty() && *v != "<nil>")
}

/// Go's `strconv.ParseBool` truths.
fn enabled(values: &[String]) -> bool {
    last(values).is_some_and(|v| matches!(v, "1" | "t" | "T" | "true" | "TRUE" | "True"))
}

impl IgnoredFlags {
    /// One line per accepted flag whose value asks for behaviour prequal-epp doesn't have.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut warn = |flag: &str, value: &str, effect: &str| {
            out.push(format!(
                "prequal-epp: warning: --{flag}={value} is accepted for compatibility and ignored: {effect}"
            ));
        };
        for (flag, values) in [("config-file", &self.config_file), ("config-text", &self.config_text)] {
            if let Some(value) = last(values) {
                let shown = if flag == "config-text" { "<inline>" } else { value };
                warn(flag, shown, "its plugins and scheduling profiles are not applied; routing is set by --policy");
            }
        }
        if enabled(&self.ha_enable_leader_election) {
            warn("ha-enable-leader-election", "true", "every replica serves and reports ready (active-active)");
        }
        if enabled(&self.tracing) {
            warn("tracing", "true", "no OpenTelemetry spans are exported");
        }
        if enabled(&self.metrics_endpoint_auth) {
            warn("metrics-endpoint-auth", "true", "/metrics is served without authentication");
        }
        if enabled(&self.enable_pprof) {
            warn("enable-pprof", "true", "no pprof endpoints are served");
        }
        if let Some(encoder) = last(&self.zap_encoder).filter(|e| *e != "console") {
            warn("zap-encoder", encoder, "logs are plain text lines on stderr");
        }
        for (flag, values) in [("zap-log-level", &self.zap_log_level), ("v", &self.v)] {
            if let Some(level) = last(values) {
                warn(flag, level, "log verbosity is fixed");
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::IgnoredFlags;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        ignored: IgnoredFlags,
    }

    #[test]
    fn accepts_chart_and_conformance_flags() {
        let args = ["epp", "--zap-encoder", "json", "--v=4", "--enable-pprof=true", "--tracing=false"];
        assert!(Cli::try_parse_from(args).is_ok());
        assert!(Cli::try_parse_from(["epp", "--config-file", "/cfg/x.yaml", "--metrics-endpoint-auth=false"]).is_ok());
        assert!(Cli::try_parse_from(["epp", "--ha-enable-leader-election", "--zap-encoder", "json"]).is_ok());
    }

    #[test]
    fn accepts_the_llm_d_chart_args_verbatim() {
        // As rendered by llm-d-router-standalone v0.10.0 and v0.11.0 (identical) with the prequal arm's values.
        let args = [
            "epp",
            "--zap-encoder",
            "json",
            "--config-file",
            "/config/optimized-baseline-plugins.yaml",
            "--enable-pprof=<nil>",
            "--tracing=<nil>",
            "--v=<nil>",
            "--tracing=false",
            "--metrics-endpoint-auth=false",
        ];
        let cli = Cli::try_parse_from(args).unwrap();
        let warnings = cli.ignored.warnings();
        assert_eq!(warnings.len(), 2, "{warnings:#?}");
        assert!(warnings[0].contains("--config-file=/config/optimized-baseline-plugins.yaml"));
        assert!(warnings[1].contains("--zap-encoder=json"));
    }

    #[test]
    fn warns_once_per_flag_asking_for_missing_behaviour() {
        let cli = Cli::try_parse_from(["epp", "--ha-enable-leader-election", "--tracing=false", "--tracing=true"]);
        let warnings = cli.unwrap().ignored.warnings();
        assert_eq!(warnings.len(), 2, "{warnings:#?}");
        assert!(warnings[0].contains("active-active") && warnings[1].contains("--tracing=true"));
        let quiet = Cli::try_parse_from(["epp", "--ha-enable-leader-election=false", "--zap-encoder=console"]);
        assert!(quiet.unwrap().ignored.warnings().is_empty());
    }
}
