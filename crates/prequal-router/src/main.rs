//! `prequal-router`: OpenAI-compatible reverse proxy for vLLM/SGLang replicas. It reads each request's prompt,
//! routes it with a load- and prefix-cache-aware policy fed by the engines' own Prometheus metrics, and streams
//! responses (including SSE) back unchanged.
//!
//! ```sh
//! prequal-router --listen 0.0.0.0:8000 --engine vllm 10.0.0.1:8000 10.0.0.2:8000
//! prequal-router --listen 0.0.0.0:8000 --engine vllm --k8s-service inference/vllm --k8s-port http
//! ```

mod admin;
mod args;
mod body;
mod discovery;
mod dns;
mod forward;
mod shards;
mod shutdown;

use std::{
    io,
    pin::pin,
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use clap::Parser;
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use prequal_llm::{EngineProber, Scheduler, policy, scrape_forever};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, mpsc},
};

use crate::{
    admin::Health,
    args::{Args, limit},
    discovery::EndpointSliceDiscovery,
    dns::DnsDiscovery,
    forward::{Forwarder, Limits},
    shards::{Conn, ShardConfig},
};

/// Discovery, scraping, health and accepting run here; the data path runs on the shards.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = Args::parse();
    let Some(policy) = policy::by_name(&args.policy) else {
        eprintln!("unknown --policy {:?}; expected one of {}", args.policy, policy::NAMES.join(", "));
        return ExitCode::from(2);
    };
    let scheduler = match args.admission_limit {
        0 => Scheduler::new(policy),
        limit => Scheduler::new(policy).with_admission_limit(limit),
    };
    match run(args, scheduler).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("prequal-router: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Serves until SIGTERM/SIGINT, then drains; `Err` on a startup failure or a dead shard.
async fn run(args: Args, scheduler: Scheduler) -> Result<(), String> {
    let signal = shutdown::install().map_err(|e| format!("installing signal handlers: {e}"))?;
    let bind = |addr| async move { TcpListener::bind(addr).await.map_err(|e| format!("bind {addr}: {e}")) };
    let listener = bind(args.listen).await?;
    let admin_listener = bind(args.admin_listen).await?;
    match &args.k8s_service {
        Some(qualified) => {
            let (namespace, service) = qualified.split_once('/').unwrap_or(("default", qualified));
            let discovery = EndpointSliceDiscovery::connect(namespace, service, args.k8s_port.clone())
                .await
                .map_err(|e| format!("connecting to Kubernetes: {e}"))?;
            tokio::spawn(discover_forever(discovery, scheduler.clone()));
        }
        None => match dns::literal_addrs(&args.backends) {
            Some(addrs) => scheduler.sync(addrs),
            None => {
                tokio::spawn(resolve_forever(DnsDiscovery::new(args.backends.clone()), scheduler.clone()));
            }
        },
    }
    let prober = EngineProber::new(args.engine).with_path(&args.metrics_path);
    tokio::spawn(scrape_forever(scheduler.clone(), prober, Duration::from_millis(args.scrape_ms)));
    let draining = Arc::new(AtomicBool::new(false));
    tokio::spawn(admin::serve(
        admin_listener,
        Health { scheduler: scheduler.clone(), draining: Arc::clone(&draining) },
    ));

    eprintln!(
        "prequal-router: policy {} on {} (health on {}, {} shards)",
        scheduler.policy_name(),
        args.listen,
        args.admin_listen,
        args.shards
    );
    let limits = Limits {
        max_body: args.max_body_mib << 20,
        buffered: Arc::new(Semaphore::new((args.max_buffered_mib << 20).min(Semaphore::MAX_PERMITS))),
        body_timeout: limit(Duration::from_secs(args.body_timeout_secs)),
        response_timeout: limit(Duration::from_secs(args.response_timeout_secs)),
    };
    let connect_timeout = limit(Duration::from_millis(args.connect_timeout_ms));
    let forwarder = move || {
        let mut connector = HttpConnector::new();
        connector.set_nodelay(true);
        connector.set_connect_timeout(connect_timeout);
        let client = Client::builder(TokioExecutor::new()).pool_idle_timeout(Duration::from_secs(90)).build(connector);
        Forwarder { scheduler: scheduler.clone(), client, limits: limits.clone() }
    };
    let drain = Duration::from_secs(args.drain_secs);
    let config = ShardConfig {
        coalesce: Duration::from_micros(args.coalesce_us),
        header_timeout: limit(Duration::from_secs(args.header_timeout_secs)),
        max_streams: args.max_streams,
        drain,
    };
    let (exited_tx, mut exited) = mpsc::unbounded_channel();
    let mut shards =
        shards::start(args.shards, config, forwarder, exited_tx).map_err(|e| format!("starting shards: {e}"))?;
    let slots = Arc::new(Semaphore::new(args.max_connections.clamp(1, Semaphore::MAX_PERMITS)));
    let mut signal = pin!(signal);
    let received = loop {
        tokio::select! {
            name = &mut signal => break name,
            Some(index) = exited.recv() => return Err(format!("shard {index} stopped unexpectedly")),
            accepted = accept(&listener, &slots) => match accepted {
                Ok(conn) => shards.send(conn).map_err(|index| format!("shard {index} stopped unexpectedly"))?,
                Err(e) => admin::accept_failed(&e).await,
            },
        }
    };

    eprintln!("prequal-router: {received}; draining in-flight requests for up to {}s", drain.as_secs());
    draining.store(true, Ordering::Relaxed);
    drop((listener, shards));
    let all_exited = async {
        for _ in 0..args.shards.max(1) {
            exited.recv().await;
        }
    };
    if tokio::time::timeout(drain + Duration::from_secs(1), all_exited).await.is_err() {
        eprintln!("prequal-router: drain timed out");
    }
    Ok(())
}

/// The next client connection, once one of `slots` (`--max-connections`) is free.
async fn accept(listener: &TcpListener, slots: &Arc<Semaphore>) -> io::Result<Conn> {
    let permit = Arc::clone(slots).acquire_owned().await.map_err(io::Error::other)?;
    let (stream, _) = listener.accept().await?;
    Ok(Conn { stream: stream.into_std()?, permit })
}

const DISCOVERY_INTERVAL: Duration = Duration::from_secs(2);

/// Re-reads the Service's ready endpoints; keeps the last set on errors.
async fn discover_forever(discovery: EndpointSliceDiscovery, scheduler: Scheduler) {
    let mut ticker = tokio::time::interval(DISCOVERY_INTERVAL);
    loop {
        ticker.tick().await;
        match discovery.discover().await {
            Ok(addrs) => scheduler.sync(addrs),
            Err(e) => eprintln!("prequal-router: discovery failed: {e}"),
        }
    }
}

/// Re-resolves replica names.
async fn resolve_forever(mut dns: DnsDiscovery, scheduler: Scheduler) {
    let mut ticker = tokio::time::interval(DISCOVERY_INTERVAL);
    loop {
        ticker.tick().await;
        scheduler.sync(dns.discover().await);
    }
}
