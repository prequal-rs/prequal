//! Real-socket testbed: an HTTP fleet with heterogeneous speeds and noisy neighbours on localhost
//! (in a child process), driven by many independent clients, comparing Prequal with tower's P2C.

mod client;
mod http;
mod server;
mod stats;
mod work;

use std::{io, path::PathBuf, time::Duration};

use clap::Parser;
use client::{LoadSpec, Policy};
use prequal_server::{RecentMedian, RifOnly, ServiceTimeModel};
use prequal_testbed::{
    blocking, fleet, fleet::FleetProcess, metrics, proxies::ProxyFleet, raise_fd_limit, raise_timer_resolution,
};
use prequal_tower::Config;
use server::{Estimator, FleetSpec};
use stats::Summary;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum EstimatorKind {
    Rif,
    Median,
    Model,
}

impl EstimatorKind {
    fn arg(self) -> &'static str {
        match self {
            Self::Rif => "rif",
            Self::Median => "median",
            Self::Model => "model",
        }
    }
}

#[derive(Parser)]
struct Args {
    #[arg(long, value_enum, default_value_t = Policy::Prequal)]
    policy: Policy,
    #[arg(long, value_enum, default_value_t = EstimatorKind::Rif)]
    estimator: EstimatorKind,
    #[arg(long, default_value_t = 0.6)]
    q_rif: f64,
    #[arg(long, default_value_t = 20)]
    servers: usize,
    #[arg(long, default_value_t = 4)]
    cores: usize,
    #[arg(long, default_value_t = 20)]
    clients: usize,
    /// Offered load as a multiple of the fleet's nominal allocation.
    #[arg(long, default_value_t = 0.8)]
    load: f64,
    #[arg(long, default_value_t = 40.0)]
    mean_work_ms: f64,
    #[arg(long, default_value_t = 20)]
    duration_s: u64,
    #[arg(long, default_value_t = 3)]
    warmup_s: u64,
    #[arg(long, default_value_t = 50)]
    probe_timeout_ms: u64,
    #[arg(long, default_value_t = 64)]
    max_in_flight_probes: usize,
    #[arg(long, default_value_t = 3.0)]
    probes_per_query: f64,
    /// Use load reports piggybacked on responses as free probes.
    #[arg(long)]
    piggyback: bool,
    /// Defaults to a third of the pool's inflow (probes plus piggybacked reports per query).
    #[arg(long)]
    removes_per_query: Option<f64>,
    /// Consecutive request failures that eject a server; 0 disables ejection.
    #[arg(long, default_value_t = 5)]
    eject_after_failures: u32,
    /// Servers that fail every request instantly while reporting zero load.
    #[arg(long, default_value_t = 0)]
    failing_servers: usize,
    /// Burn real CPU for each request instead of sleeping.
    #[arg(long)]
    cpu_work: bool,
    #[arg(long, default_value_t = 1)]
    seed: u64,
    #[arg(long)]
    csv: Option<PathBuf>,
    /// Route through this many prequal-pingora example proxies (path to the built `proxy` binary).
    #[arg(long)]
    pingora_bin: Option<PathBuf>,
    #[arg(long, default_value_t = 4)]
    proxies: usize,
    /// `prequal` or `round-robin`, passed to the proxies.
    #[arg(long, default_value = "prequal")]
    proxy_policy: String,
    /// Internal: run only the server fleet (spawned by the parent process).
    #[arg(long, hide = true)]
    serve: bool,
}

impl Args {
    fn fleet_spec(&self) -> FleetSpec {
        FleetSpec {
            servers: self.servers,
            cores: self.cores,
            slow_fraction: 0.5,
            noisy_fraction: 0.3,
            failing: self.failing_servers,
            cpu_work: self.cpu_work,
            seed: self.seed,
        }
    }

    fn prequal_config(&self) -> Config {
        let inflow = self.probes_per_query + f64::from(u8::from(self.piggyback));
        let mut config = Config::default();
        config.q_rif = self.q_rif;
        config.probes_per_query = self.probes_per_query;
        config.removes_per_query = self.removes_per_query.unwrap_or(inflow / 3.0);
        config.eject_after_failures = self.eject_after_failures;
        config
    }

    fn fleet_args(&self) -> Vec<String> {
        let mut args: Vec<String> = [
            ("--servers", self.servers.to_string()),
            ("--cores", self.cores.to_string()),
            ("--estimator", self.estimator.arg().to_owned()),
            ("--failing-servers", self.failing_servers.to_string()),
            ("--seed", self.seed.to_string()),
        ]
        .into_iter()
        .flat_map(|(flag, value)| [flag.to_owned(), value])
        .collect();
        if self.cpu_work {
            args.push("--cpu-work".to_owned());
        }
        args
    }
}

fn estimator_factory(kind: EstimatorKind, cores: usize) -> impl Fn() -> Estimator {
    move || -> Estimator {
        match kind {
            EstimatorKind::Rif => Box::new(RifOnly),
            EstimatorKind::Median => Box::new(RecentMedian::default()),
            EstimatorKind::Model => Box::new(ServiceTimeModel::new(cores as f64, 0.2)),
        }
    }
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let args = Args::parse();
    raise_timer_resolution();
    raise_fd_limit();
    let fleet = args.fleet_spec();

    if args.serve {
        let addrs = server::spawn_fleet(&fleet, estimator_factory(args.estimator, args.cores)).await;
        return blocking(move || fleet::announce_and_wait(&addrs)).await;
    }

    let fleet_args = args.fleet_args();
    let fleet_process = blocking(move || FleetProcess::spawn(&fleet_args)).await?;
    let mean_work_us = args.mean_work_ms * 1e3;
    let total_rate = args.load * fleet.allocation_capacity(mean_work_us);
    let spec = LoadSpec {
        rate_per_client: total_rate / args.clients as f64,
        mean_work_us,
        warmup: Duration::from_secs(args.warmup_s),
        duration: Duration::from_secs(args.duration_s),
        request_timeout: Duration::from_secs(10),
        prequal: args.prequal_config(),
        probe_timeout: Duration::from_millis(args.probe_timeout_ms),
        max_in_flight_probes: args.max_in_flight_probes,
        piggyback: args.piggyback,
    };
    // Through a Pingora tier, clients spread requests over the proxies; the proxies do the balancing.
    let (policy, targets, via, _proxies) = match &args.pingora_bin {
        Some(bin) => {
            let (bin, backends, policy) = (bin.clone(), fleet_process.addrs.clone(), args.proxy_policy.clone());
            let count = args.proxies;
            let proxies = blocking(move || {
                ProxyFleet::spawn(&bin, count, |cmd, listen| {
                    cmd.args(backends.iter().map(ToString::to_string))
                        .env("PREQUAL_LISTEN", listen.to_string())
                        .env("PREQUAL_POLICY", &policy);
                })
            })
            .await?;
            (Policy::RoundRobin, proxies.addrs.clone(), format!("pingora-{}", args.proxy_policy), Some(proxies))
        }
        None => (args.policy, fleet_process.addrs.clone(), "direct".to_owned(), None),
    };
    let result = client::run(policy, &targets, args.clients, spec, args.seed).await;
    let client_usage = metrics::current();
    let server_usage = blocking(move || fleet_process.shutdown()).await?;

    let summary = Summary::from_outcomes(&result.outcomes);
    let row = format!(
        "{:?},{via},{:?},{},{},{},{},{},{},{},{},{}",
        policy,
        args.estimator,
        args.q_rif,
        args.probes_per_query,
        args.piggyback,
        args.failing_servers,
        args.cpu_work,
        args.servers,
        args.clients,
        args.load,
        summary.csv_fields(client_usage, server_usage, result.probes)
    );
    println!("{}\n{row}  ({total_rate:.0} req/s offered)", stats::CSV_HEADER);
    if let Some(path) = &args.csv {
        stats::append_csv(path, &row)?;
    }
    Ok(())
}
