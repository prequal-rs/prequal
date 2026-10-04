//! ext_proc connections served from dedicated single-threaded runtimes ("shards"), each connection pinned to one.
//!
//! Envoy sends one gRPC message per streamed body chunk, i.e. per token, each needing microseconds of work. The cost
//! is in waking up for it: on tokio's work-stealing runtime each message woke tasks on other workers (futex, park,
//! steal), and every thread that goes idle between messages pays an epoll wakeup, a `recv` and a `writev` per
//! message. One thread owning every connection instead finds several connections' messages ready per wakeup, so its
//! cost per request *falls* as load rises. More shards only pay off once one thread saturates; measure with
//! `examples/epp_load`.
//!
//! Even one thread mostly wakes for a couple of messages per connection, paying a `recv`, a TLS `writev` and an
//! `epoll_wait` per wakeup. Lingering `coalesce` before parking lets each pass answer more, for at most that much
//! added latency per message.

use std::{io, sync::Arc, time::Duration};

use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_stream::{StreamExt, wrappers::UnboundedReceiverStream};
use tonic::transport::server::Router;

use crate::shutdown::Draining;

/// Serves `router()` (built once per shard) on `listener` from `shards` single-threaded runtimes until `draining`
/// starts, then stops accepting, sends HTTP/2 GOAWAY on every connection and returns once their in-flight streams
/// have finished (callers bound that wait). A shard that fails, or accepting that fails fatally, is an error.
/// A non-zero `coalesce` makes each shard wait that long before going idle, so messages arriving meanwhile are read,
/// handled and answered in one pass (fewer syscalls per message) at up to `coalesce` added latency per message.
pub async fn serve<F>(
    listener: TcpListener,
    shards: usize,
    coalesce: Duration,
    router: F,
    draining: Draining,
) -> io::Result<()>
where
    F: Fn() -> Router + Send + Sync + 'static,
{
    let router = Arc::new(router);
    let (ended_tx, mut ended) = mpsc::unbounded_channel::<io::Result<()>>();
    let mut shard_tx = Vec::with_capacity(shards);
    for i in 0..shards.max(1) {
        let (tx, rx) = mpsc::unbounded_channel::<std::net::TcpStream>();
        let (router, ended_tx, draining) = (Arc::clone(&router), ended_tx.clone(), draining.clone());
        std::thread::Builder::new().name(format!("ext-proc-{i}")).spawn(move || {
            let mut builder = tokio::runtime::Builder::new_current_thread();
            if !coalesce.is_zero() {
                builder.on_thread_park(move || std::thread::sleep(coalesce));
            }
            let served = builder.enable_all().build().and_then(|runtime| {
                runtime.block_on(async move {
                    // Re-registered with this shard's reactor; tonic's own listener would set TCP_NODELAY too.
                    let incoming = UnboundedReceiverStream::new(rx).map(|std| {
                        let stream = TcpStream::from_std(std)?;
                        stream.set_nodelay(true)?;
                        Ok::<_, io::Error>(stream)
                    });
                    router()
                        .serve_with_incoming_shutdown(incoming, draining.wait())
                        .await
                        .map_err(|e| io::Error::other(format!("ext_proc shard {i}: {e}")))
                })
            });
            let _ = ended_tx.send(served);
        })?;
        shard_tx.push(tx);
    }
    drop(ended_tx);
    let mut next = 0;
    loop {
        tokio::select! {
            // Drain first: shards that finish draining end `Ok`, which must not read as a failure.
            biased;
            () = draining.clone().wait() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    // A shard only stops on its own by failing, which `ended` reports.
                    let _ = shard_tx[next % shard_tx.len()].send(stream.into_std()?);
                    next += 1;
                }
                // Out of descriptors and the like: back off instead of spinning.
                Err(e) => {
                    eprintln!("prequal-epp: accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
            Some(result) = ended.recv() => {
                return Err(result.err().unwrap_or_else(|| io::Error::other("an ext_proc shard stopped")));
            }
        }
    }
    drop((listener, shard_tx));
    while let Some(result) = ended.recv().await {
        result?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tonic::transport::{Endpoint, Server};
    use tonic_health::pb::{HealthCheckRequest, health_check_response::ServingStatus, health_client::HealthClient};

    use super::*;
    use crate::shutdown;

    #[tokio::test]
    async fn every_shard_serves_grpc_until_drained() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_, health) = tonic_health::server::health_reporter();
        let (drain, draining) = shutdown::channel();
        let router = move || Server::builder().add_service(health.clone());
        let server = tokio::spawn(serve(listener, 2, Duration::from_micros(100), router, draining));
        let endpoint = Endpoint::from_shared(format!("http://{addr}")).unwrap();
        for _ in 0..3 {
            let channel = endpoint.connect().await.unwrap();
            let response = HealthClient::new(channel).check(HealthCheckRequest::default()).await.unwrap();
            assert_eq!(response.into_inner().status, ServingStatus::Serving as i32);
        }
        drain.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), server).await.unwrap().unwrap().unwrap();
        assert!(endpoint.connect().await.is_err(), "no longer accepting");
    }
}
