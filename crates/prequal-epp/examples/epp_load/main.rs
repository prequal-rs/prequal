//! Load harness for prequal-epp's CPU cost per request: spawns the real `prequal-epp` binary against fake model
//! servers (captured simulator `/metrics`), then drives llm-d's `shared_prefix` benchmark through it, either
//! - directly, playing Envoy's side of the ext_proc streams (`driver`; cheap, but its own batching couples to the
//!   EPP's and swings results run to run), or
//! - through a real Envoy with the llm-d chart's config (`--envoy <binary>`; the faithful mode, matches kind), or
//! - through `prequal-router` alone in the EPP's place (`--router <binary>`), the no-Envoy deployment.
//!
//! Reports the EPP's (or router's) gross CPU per request (what `kubectl top` sees) and net of its idle burn.
//!
//! ```sh
//! cargo build --release -p prequal-epp --bin prequal-epp --example epp_load
//! target/release/examples/epp_load --rate 25 --seconds 20 [--envoy path/to/envoy]   # Linux: CPU from /proc
//! ```

mod driver;
mod envoy;
mod http_driver;
mod procs;
mod replicas;
mod tls;

use std::{
    error::Error,
    net::SocketAddr,
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};

use clap::Parser;
use rand::{SeedableRng, rngs::SmallRng};

use crate::procs::{Reaped, cpu, cpu_and_peak, default_epp, free_port, host_busy, pinned};

#[derive(Parser)]
#[command(args_override_self = true)]
struct Args {
    /// prequal-epp binary (default: next to this example's directory).
    #[arg(long)]
    epp: Option<PathBuf>,
    /// Put a real Envoy (this binary, e.g. extracted from envoyproxy/envoy:distroless-v1.33.2) in front of the EPP.
    #[arg(long)]
    envoy: Option<PathBuf>,
    /// Pin Envoy to these CPUs (`taskset -c` list).
    #[arg(long)]
    envoy_cpus: Option<String>,
    /// Envoy's ext_proc response_body_mode (the chart's is FULL_DUPLEX_STREAMED).
    #[arg(long, default_value = "FULL_DUPLEX_STREAMED")]
    response_body_mode: String,
    /// Envoy worker threads (the chart's `--concurrency`).
    #[arg(long, default_value_t = 8)]
    envoy_concurrency: u32,
    /// Envoy config template replacing the embedded chart config (same *_PORT placeholders).
    #[arg(long)]
    envoy_config: Option<PathBuf>,
    /// Run this `prequal-router` binary as the whole data path (no Envoy, no ext_proc) in the EPP's place.
    #[arg(long, conflicts_with = "envoy")]
    router: Option<PathBuf>,
    #[arg(long, default_value_t = 10)]
    replicas: usize,
    /// Requests per second (open loop).
    #[arg(long, default_value_t = 25.0)]
    rate: f64,
    #[arg(long, default_value_t = 20.0)]
    seconds: f64,
    /// Idle seconds measured first, to net out the scrape loop.
    #[arg(long, default_value_t = 3.0)]
    idle_seconds: f64,
    #[arg(long, default_value_t = 150)]
    groups: usize,
    #[arg(long, default_value_t = 5)]
    questions: usize,
    #[arg(long, default_value_t = 9500)]
    system_tokens: usize,
    #[arg(long, default_value_t = 500)]
    question_tokens: usize,
    #[arg(long, default_value_t = 1000)]
    output_tokens: usize,
    /// Inter-token latency of the streamed response, in microseconds (kind's loaded sims at 25 req/s: ~1600).
    #[arg(long, default_value_t = 1000)]
    itl_us: u64,
    /// Direct mode: request-body bytes per ext_proc message (Envoy forwards what it read).
    #[arg(long, default_value_t = 16384)]
    chunk: usize,
    /// Direct mode: gRPC connections Envoy spreads streams over: one per Envoy worker (`--concurrency 8` in llm-d's
    /// optimized-baseline values).
    #[arg(long, default_value_t = 8)]
    connections: usize,
    /// TLS on the ext_proc port (the chart's). `false` with Envoy needs an `--envoy-config` without the TLS socket.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    tls: bool,
    /// Tokio worker threads for the EPP only (TOKIO_WORKER_THREADS). 4 = what tokio picks under the llm-d chart's
    /// 4-CPU limit; 0 = tokio's default here.
    #[arg(long, default_value_t = 4)]
    epp_workers: usize,
    /// Pin the EPP to these CPUs (`taskset -c` list, Linux), apart from the driver, for steadier numbers.
    #[arg(long)]
    epp_cpus: Option<String>,
    /// Extra flags passed through to prequal-epp.
    #[arg(last = true)]
    epp_args: Vec<String>,
}

enum Target {
    Direct(Vec<tonic::transport::Channel>),
    /// Inference-perf-like HTTP client in front of Envoy (`Some`) or prequal-router.
    Http(Option<Reaped>, http_driver::Client),
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let itl = Duration::from_micros(args.itl_us);
    let endpoints = replicas::start(args.replicas, replicas::Stream { tokens: args.output_tokens, itl }).await?;
    let port = free_port()?;
    let list = endpoints.iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
    let epp = args.epp.clone().unwrap_or_else(default_epp);
    let tls = args.tls;
    let mut command = pinned(args.router.as_ref().unwrap_or(&epp), args.epp_cpus.as_deref());
    if args.epp_workers > 0 {
        command.env("TOKIO_WORKER_THREADS", args.epp_workers.to_string());
    }
    if args.router.is_some() {
        command.args(["--listen", &format!("127.0.0.1:{port}")]).args(endpoints.iter().map(ToString::to_string));
    } else {
        command
            .args(["--endpoints", &list, "--secure-serving", &tls.to_string(), "--grpc-port", &port.to_string()])
            .args(["--grpc-health-port", &free_port()?.to_string(), "--metrics-port", &free_port()?.to_string()]);
    }
    let mut child = Reaped(command.args(&args.epp_args).stderr(Stdio::null()).spawn()?);
    let pid = child.0.id();
    let target = if args.router.is_some() {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        envoy::wait_listening(addr).await?;
        Target::Http(None, http_driver::Client::new(addr))
    } else {
        let channels = loop {
            match connect_all(port, tls, args.connections).await {
                Ok(channels) => break channels,
                Err(_) if child.0.try_wait()?.is_none() => tokio::time::sleep(Duration::from_millis(20)).await,
                Err(e) => return Err(e),
            }
        };
        match &args.envoy {
            Some(binary) => {
                drop(channels);
                let options = envoy::Options {
                    cpus: args.envoy_cpus.as_deref(),
                    response_body_mode: &args.response_body_mode,
                    concurrency: args.envoy_concurrency,
                    config: args.envoy_config.as_deref(),
                };
                let (envoy, addr) = envoy::start(binary, port, &options).await?;
                Target::Http(Some(envoy), http_driver::Client::new(addr))
            }
            None => Target::Direct(channels),
        }
    };
    let envoy_pid = match &target {
        Target::Http(envoy, _) => envoy.as_ref().map(|e| e.0.id()),
        Target::Direct(_) => None,
    };
    let mode = match (&target, envoy_pid) {
        (Target::Direct(_), _) => "direct",
        (_, Some(_)) => "envoy",
        _ => "router",
    };
    eprintln!(
        "epp_load: {mode}, router pid {pid} on :{port}, {} replicas, tls {tls}, envoy {envoy_pid:?}",
        args.replicas
    );

    let idle_start = cpu_and_peak(pid);
    tokio::time::sleep(Duration::from_secs_f64(args.idle_seconds)).await;
    let idle_end = cpu_and_peak(pid);

    let workload = driver::Workload::new(
        args.groups,
        args.questions,
        args.system_tokens,
        args.question_tokens,
        args.output_tokens,
    );
    let mut rng = SmallRng::seed_from_u64(11);
    let started = Instant::now();
    let (host0, self0, envoy0) = (host_busy(), cpu(std::process::id()), envoy_pid.and_then(cpu));
    let mut ticker = tokio::time::interval(Duration::from_secs_f64(1.0 / args.rate));
    let mut tasks = Vec::new();
    while started.elapsed().as_secs_f64() < args.seconds {
        ticker.tick().await;
        let body = workload.body(&mut rng);
        tasks.push(match &target {
            Target::Direct(channels) => {
                let channel = channels[tasks.len() % channels.len()].clone();
                tokio::spawn(driver::run(channel, body, args.chunk, args.output_tokens, itl))
            }
            Target::Http(_, client) => tokio::spawn(client.clone().run(body)),
        });
    }
    let mut outcomes = Vec::with_capacity(tasks.len());
    for task in tasks {
        outcomes.push(task.await?);
    }
    let elapsed = started.elapsed().as_secs_f64();
    let load_end = cpu_and_peak(pid);
    let (host1, self1, envoy1) = (host_busy(), cpu(std::process::id()), envoy_pid.and_then(cpu));
    drop(target);
    drop(child);

    report(&outcomes, elapsed, mode);
    if let (Some((i0, _)), Some((i1, _)), Some((l1, peak))) = (idle_start, idle_end, load_end) {
        let n = outcomes.len() as f64;
        let idle_cores = (i1 - i0) / args.idle_seconds;
        let load_cpu = l1 - i1;
        println!(
            "idle_cores={idle_cores:.3} load_cores={:.3} cpu_us/req={:.0} net_cpu_us/req={:.0} peak_rss_mib={:.1}",
            load_cpu / elapsed,
            load_cpu / n * 1e6,
            (load_cpu - idle_cores * elapsed) / n * 1e6,
            peak as f64 / 1024.0
        );
        let envoy_cpu = envoy0.zip(envoy1).map_or(0.0, |(a, b)| b - a);
        if envoy_pid.is_some() {
            println!("envoy_cores={:.3} envoy_cpu_us/req={:.0}", envoy_cpu / elapsed, envoy_cpu / n * 1e6);
        }
        if let (Some(h0), Some(h1), Some(s0), Some(s1)) = (host0, host1, self0, self1) {
            // Anything well above ~0.1 means other work shared the box during the run: distrust it.
            let driver = (s1 - s0) / elapsed;
            println!(
                "driver_cores={driver:.2} other_host_cores={:.2}",
                (h1 - h0 - load_cpu - envoy_cpu) / elapsed - driver
            );
        }
    }
    Ok(())
}

/// Request-level results. Direct mode: route = time to the pick, echo = token echo latency. HTTP modes (envoy,
/// router): route = time to first token, msgs_out = SSE events received.
fn report(outcomes: &[driver::Outcome], elapsed: f64, mode: &str) {
    let n = outcomes.len();
    let routed = outcomes.iter().filter(|o| o.routed).count();
    let sent: usize = outcomes.iter().map(|o| o.sent).sum();
    let received: usize = outcomes.iter().map(|o| o.received).sum();
    let mut picks: Vec<f64> = outcomes.iter().filter(|o| o.routed).map(|o| o.pick.as_secs_f64() * 1e3).collect();
    picks.sort_by(f64::total_cmp);
    let pct = |p: f64| picks.get(((picks.len() as f64 - 1.0) * p) as usize).copied().unwrap_or(f64::NAN);
    let mut echo: Vec<u32> = outcomes.iter().flat_map(|o| o.echo_us.iter().copied()).collect();
    echo.sort_unstable();
    let echo_pct = |p: f64| echo.get(((echo.len() as f64 - 1.0) * p) as usize).copied().unwrap_or(0);
    let route = if mode == "direct" { "route" } else { "ttft" };
    println!(
        "mode={mode} requests={n} routed={routed} msgs_in/req={:.0} msgs_out/req={:.0} {route}_ms_p50={:.2} \
         {route}_ms_p90={:.2} {route}_ms_p99={:.2} echo_us_p50={} echo_us_p99={} wall_s={elapsed:.1}",
        sent as f64 / n as f64,
        received as f64 / n as f64,
        pct(0.5),
        pct(0.9),
        pct(0.99),
        echo_pct(0.5),
        echo_pct(0.99)
    );
}

async fn connect_all(
    port: u16,
    tls: bool,
    connections: usize,
) -> Result<Vec<tonic::transport::Channel>, Box<dyn Error>> {
    let mut channels = Vec::with_capacity(connections);
    for _ in 0..connections {
        channels.push(tls::channel(port, tls).await?);
    }
    Ok(channels)
}
