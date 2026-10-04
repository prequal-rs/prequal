//! LLM routing benchmark: simulated vLLM engines with a prefix cache (child process) behind N `prequal-router`
//! processes (or none, `--direct`), driven by streaming completions from a random or shared-prefix workload.
//! `--virtual` runs the same engines, workload and routing library in simulated time instead (see `vsim`).
//!
//! ```sh
//! llm-bench --router-bin target/release/prequal-router --policy prequal --load 0.8
//! llm-bench --virtual --workload shared-prefix --engine-preset h100-qwen32b --stages 15:50,3:20,10:20 \
//!     --warmup-stages 1 --policy prequal --routers 1
//! ```

mod batch;
mod cache;
mod cli;
mod diagnose;
mod engine;
mod load;
mod oracle;
mod order;
mod popularity;
mod preset;
mod report;
mod sim;
mod trace;
mod vsim;
mod workload;

use std::{io, net::SocketAddr, time::Duration};

use clap::Parser;
use cli::{Args, Schedule, WorkloadKind};
use load::{Arrivals, Outcome, Wire};
use prequal_llm::PrefillSignal;
use prequal_testbed::{
    blocking, fleet, fleet::FleetProcess, proxies::ProxyFleet, raise_fd_limit, raise_timer_resolution,
};
use preset::EngineSpec;
use report::{RunInfo, StageResult};
use workload::Source;

#[tokio::main]
async fn main() -> io::Result<()> {
    let args = Args::parse();
    raise_timer_resolution();
    raise_fd_limit();
    let specs = args.engine_specs();
    if args.serve {
        let addrs = sim::spawn_engines(&specs).await;
        return blocking(move || fleet::announce_and_wait(&addrs)).await;
    }
    let generator = args.generator()?;
    let capacity_rps = specs.iter().map(EngineSpec::decode_throughput).sum::<f64>() / generator.mean_output();
    let schedule = args.schedule(&generator, capacity_rps);
    let source = Source::new(generator, args.seed);
    let (outcomes, time_scale) = match args.virtual_time {
        true => (run_virtual(&args, &specs, &schedule, source), 1.0),
        false => (run_http(&args, &schedule, source).await?, args.time_scale),
    };

    let direct = args.direct && !args.virtual_time;
    let run = RunInfo {
        policy: match (direct, args.prefill_signal) {
            (true, _) => "direct".to_owned(),
            (false, PrefillSignal::FirstChunk) => args.policy.clone(),
            (false, signal) => format!("{}@{signal}", args.policy),
        } + if args.end_at_first_token { "+close" } else { "" }
            + &args.queue_order.map_or_else(String::new, |order| format!("+queue-{order}"))
            + &args.oracle_index.map_or_else(String::new, |mode| format!("+oracle-{mode:?}").to_lowercase())
            + &args.gossip_ms.map_or_else(String::new, |ms| format!("+gossip-{ms}ms"))
            + &if args.gossip_loss > 0.0 { format!("-loss-{}", args.gossip_loss) } else { String::new() },
        routers: if direct { 0 } else { args.routers },
        engines: if args.targets.is_empty() { args.engines } else { args.targets.len() },
        workload: match (&args.trace, args.workload) {
            (Some(_), _) => "trace",
            (None, WorkloadKind::Random) => "random",
            (None, WorkloadKind::SharedPrefix) => "shared-prefix",
        },
        capacity_rps,
        concurrency: schedule.closed_users.unwrap_or(0),
        time_scale,
    };
    let stages: Vec<StageResult> = schedule
        .stages
        .iter()
        .zip(outcomes)
        .enumerate()
        .skip(schedule.warmup_stages)
        .map(|(index, (stage, outcomes))| StageResult { index, rate: stage.rate, duration: stage.duration, outcomes })
        .collect();
    let rows = report::stage_rows(&run, &stages);
    println!("{}\n{}", report::CSV_HEADER, rows.join("\n"));
    if let Some(path) = &args.csv {
        report::append_csv(path, &rows)?;
    }
    Ok(())
}

fn run_virtual(args: &Args, specs: &[EngineSpec], schedule: &Schedule, source: Source) -> Vec<Vec<Outcome>> {
    let setup = vsim::Setup {
        specs,
        policy: &args.policy,
        routers: args.routers,
        stages: &schedule.stages,
        closed_users: schedule.closed_users,
        timeout: Duration::from_secs(args.timeout_s),
        seed: args.seed,
        slowdown: args.host_slowdown,
        signal: args.prefill_signal,
        end_at_first_token: args.end_at_first_token,
        diagnose: args.diagnose.then(|| args.tier_ends()),
        hidden: engine::Hidden { kv_capacity: args.hide_kv_capacity, prefix_counters: args.hide_prefix_counters },
        queue_order: args.queue_order,
        oracle: args.oracle_index,
        restart_router_at: args.router_restart_s.map(Duration::from_secs_f64),
        gossip_delay: args.gossip_ms.map(Duration::from_millis),
        gossip_loss: args.gossip_loss,
    };
    let (outcomes, totals) = vsim::run(&setup, source);
    report::print_totals(&totals);
    outcomes
}

async fn run_http(args: &Args, schedule: &Schedule, source: Source) -> io::Result<Vec<Vec<Outcome>>> {
    let fleet = match args.targets.is_empty() {
        true => {
            let fleet_args = args.fleet_args();
            Some(blocking(move || FleetProcess::spawn(&fleet_args)).await?)
        }
        false => None,
    };
    let engines = fleet.as_ref().map_or(&args.targets, |f| &f.addrs).clone();
    let routers = match args.direct {
        true => None,
        false => Some(spawn_routers(args, &engines).await?),
    };
    let targets = routers.as_ref().map_or(&engines, |r| &r.addrs).clone();
    let wall = |d: Duration| d.mul_f64(args.time_scale);
    let stages = schedule.stages.iter().map(|s| wall(s.duration)).collect();
    let arrivals = match schedule.closed_users {
        Some(users) => Arrivals::Closed { users, stages },
        None => {
            let at = source.arrivals_us(&schedule.stages, args.seed);
            Arrivals::Open { at: at.into_iter().map(|us| wall(Duration::from_micros(us))).collect(), stages }
        }
    };
    let timeout = wall(Duration::from_secs(args.timeout_s));
    let wire = Wire { model: args.model.clone(), token_vocab: args.token_vocab, queue_order: args.queue_order };
    let outcomes = load::run(&targets, source, arrivals, timeout, wire, args.seed).await;
    report::print_engine_totals(&engines).await;
    drop(routers);
    if let Some(fleet) = fleet {
        blocking(move || fleet.shutdown()).await?;
    }
    Ok(outcomes)
}

async fn spawn_routers(args: &Args, engines: &[SocketAddr]) -> io::Result<ProxyFleet> {
    let router_bin = args.router_bin.clone().expect("--router-bin is required unless --direct or --virtual");
    let (backends, policy, extra) = (engines.to_vec(), args.policy.clone(), args.router_arg.clone());
    let count = args.routers;
    blocking(move || {
        ProxyFleet::spawn(&router_bin, count, |cmd, listen| {
            cmd.args(["--listen", &listen.to_string(), "--policy", &policy, "--engine", "vllm"])
                .args(&extra)
                .args(backends.iter().map(ToString::to_string));
        })
    })
    .await
}
