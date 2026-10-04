//! `prequal-epp`: a Gateway API Inference Extension endpoint picker that routes requests to model
//! server Pods by estimated time to first token (load + prefix-cache affinity, `prequal_llm::Scheduler`).
//! Speaks Envoy ext_proc per the GIE endpoint-picker protocol
//! (<https://github.com/kubernetes-sigs/gateway-api-inference-extension>).
//! Drop-in for the llm-d-router charts and the GIE conformance manifests (see `compat`).
//!
//! ```sh
//! prequal-epp --pool-name vllm-pool --pool-namespace inference            # in-cluster
//! prequal-epp --endpoints 127.0.0.1:8001,127.0.0.1:8002 --secure-serving=false   # local
//! ```

mod args;
mod board;
mod compat;
mod engine_priority;
mod extproc;
mod health;
mod limits;
mod metadata;
mod objectives;
mod picker;
mod pool;
mod request_metrics;
mod responses;
mod service;
mod shards;
mod shutdown;
mod telemetry;

use std::{collections::BTreeMap, error::Error, net::SocketAddr, process::ExitCode, sync::Arc, time::Duration};

use clap::Parser;
use envoy_types::pb::envoy::service::ext_proc::v3::external_processor_server::ExternalProcessorServer;
use prequal_llm::{Engine, EngineProber, Scheduler, policy, scrape_forever};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Identity, Server, ServerTlsConfig};

use crate::{
    args::Args, engine_priority::EnginePriority, limits::Quota, picker::Picker, pool::PoolRef,
    request_metrics::REQUESTS, service::Epp, shutdown::Signals, telemetry::TELEMETRY,
};

/// Envoy keeps ext_proc connections open indefinitely; pinging them frees the streams of a peer that vanished.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);

type BoxError = Box<dyn Error + Send + Sync>;

fn self_signed() -> Result<Identity, rcgen::Error> {
    let certified = rcgen::generate_simple_self_signed(vec!["prequal-epp".to_owned()])?;
    Ok(Identity::from_pem(certified.cert.pem(), certified.signing_key.serialize_pem()))
}

async fn bind(what: &str, port: u16) -> Result<TcpListener, BoxError> {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    TcpListener::bind(addr).await.map_err(|e| format!("cannot listen for {what} on {addr}: {e}").into())
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Args::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("prequal-epp: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), BoxError> {
    for warning in args.ignored.warnings() {
        eprintln!("{warning}");
    }
    // SGLang runs the highest priority first; the stamp is ordered for vLLM, which runs the lowest.
    if args.engine_priority_handicap.is_some() && args.engine != Engine::Vllm {
        return Err("--engine-priority-handicap requires --engine vllm".into());
    }
    let mut signals = Signals::install()?;
    let policy = policy::by_name(&args.policy)
        .ok_or_else(|| format!("unknown --policy {:?}; expected one of {}", args.policy, policy::NAMES.join(", ")))?;
    let scheduler = Scheduler::new(policy).with_prefill_signal(args.prefill_signal);
    let scheduler = match args.admission_limit {
        0 => scheduler,
        limit => scheduler.with_admission_limit(limit),
    };
    let scrape_interval = Duration::from_millis(args.scrape_ms);
    let picker = Arc::new(Picker::new(scheduler.clone(), args.fallbacks, scrape_interval));
    let prober = EngineProber::new(args.engine).with_path(&args.metrics_path);
    tokio::spawn(scrape_forever(scheduler, prober, scrape_interval));
    let pool_name = args.pool_name.clone().unwrap_or_default();
    let namespace = if args.pool_name.is_some() { args.pool_namespace.clone() } else { String::new() };
    match &args.pool_name {
        Some(name) => {
            let pool = PoolRef { group: args.pool_group, namespace: args.pool_namespace, name: name.clone() };
            tokio::spawn(pool::watch(pool, Arc::clone(&picker), Duration::from_millis(args.refresh_ms)));
        }
        None => picker.sync(&args.endpoints.iter().map(|a| (*a, a.to_string())).collect::<BTreeMap<_, _>>()),
    }

    // Every port is bound before anything serves, so a taken port fails startup and names itself.
    let health_listener = bind("gRPC health", args.grpc_health_port).await?;
    let metrics_listener = bind("metrics", args.metrics_port).await?;
    let ext_proc_listener = bind("ext_proc", args.grpc_port).await?;
    let (addr, health_addr, metrics_addr) =
        (ext_proc_listener.local_addr()?, health_listener.local_addr()?, metrics_listener.local_addr()?);

    let (drain, draining) = shutdown::channel();
    let (reporter, health) = tonic_health::server::health_reporter();
    tokio::spawn(health::report_readiness(reporter, Arc::clone(&picker), draining.clone()));
    let health_service = health.clone();
    let mut health_server = tokio::spawn(async move {
        let incoming = TcpListenerStream::new(health_listener);
        Server::builder().add_service(health_service).serve_with_incoming(incoming).await
    });

    let streams = Quota::new(args.max_concurrent_streams as usize);
    let bodies = Quota::new(args.max_buffered_body_mib << 20);
    let render = {
        let (picker, streams) = (Arc::clone(&picker), Arc::clone(&streams));
        move || {
            let mut out = String::new();
            TELEMETRY.render(streams.used(), &mut out);
            REQUESTS.render(&mut out);
            picker.render_pool_metrics(&pool_name, &namespace, &mut out);
            out
        }
    };
    let mut metrics_server = tokio::spawn(telemetry::serve(metrics_listener, render));

    let tls = match args.secure_serving {
        true => Some(ServerTlsConfig::new().identity(self_signed()?)),
        false => None,
    };
    if let Some(tls) = &tls {
        Server::builder().tls_config(tls.clone())?;
    }
    let shards = args.ext_proc_threads.max(1);
    let test_hooks = args.conformance_test_hooks;
    let per_connection = (args.max_concurrent_streams > 0).then_some(args.max_concurrent_streams);
    let open_streams = Arc::clone(&streams);
    let engine_priority = args.engine_priority_handicap.map(|h| Arc::new(EnginePriority::new(h)));
    let router = move || {
        let mut server = Server::builder()
            .max_concurrent_streams(per_connection)
            .http2_keepalive_interval(Some(KEEPALIVE_INTERVAL))
            .http2_keepalive_timeout(Some(KEEPALIVE_TIMEOUT));
        if let Some(tls) = &tls {
            server = server.tls_config(tls.clone()).expect("validated at startup");
        }
        let epp = Epp::new(Arc::clone(&picker), test_hooks)
            .with_limits(Arc::clone(&streams), Arc::clone(&bodies))
            .with_engine_priority(engine_priority.clone());
        // Envoy health-checks the ext_proc cluster itself, so health is served on this port too.
        server.add_service(health.clone()).add_service(ExternalProcessorServer::new(epp))
    };
    let coalesce = Duration::from_micros(args.ext_proc_coalesce_us);
    let mut ext_proc = tokio::spawn(shards::serve(ext_proc_listener, shards, coalesce, router, draining));
    eprintln!(
        "prequal-epp: policy {} ext_proc on {addr} (tls: {}, {shards} threads), health on {health_addr} and {addr}, metrics on {metrics_addr}{}",
        args.policy,
        args.secure_serving,
        if test_hooks { "; CONFORMANCE TEST HOOKS ON" } else { "" }
    );

    let signal = tokio::select! {
        ended = &mut ext_proc => return Err(stopped("ext_proc server", ended.map(|r| r.map_err(Into::into)))),
        ended = &mut health_server => return Err(stopped("gRPC health server", ended.map(|r| r.map_err(Into::into)))),
        _ = &mut metrics_server => return Err("metrics server stopped".into()),
        signal = signals.recv() => signal,
    };
    eprintln!(
        "prequal-epp: {signal}: readiness NOT_SERVING, refusing new ext_proc streams, draining {} open for up to {:?}",
        open_streams.used(),
        args.drain_timeout
    );
    drain.send_replace(true);
    tokio::select! {
        drained = tokio::time::timeout(args.drain_timeout, ext_proc) => match drained {
            Ok(ended) => ended??,
            Err(_) => eprintln!("prequal-epp: drain timed out with {} ext_proc streams open", open_streams.used()),
        },
        signal = signals.recv() => eprintln!("prequal-epp: {signal} again: exiting without waiting"),
    }
    eprintln!("prequal-epp: stopped");
    Ok(())
}

/// The error for a server task that ended before shutdown, which only happens when it fails.
fn stopped(what: &str, ended: Result<Result<(), BoxError>, tokio::task::JoinError>) -> BoxError {
    match ended {
        Ok(Ok(())) => format!("{what} stopped").into(),
        Ok(Err(e)) => format!("{what} failed: {e}").into(),
        Err(e) => format!("{what} panicked: {e}").into(),
    }
}
