//! Client connections served from dedicated single-threaded runtimes ("shards"), each with its own upstream
//! connection pool, so a streamed token's read, hand-off and write stay on one thread.
//!
//! A proxied token costs two syscalls (`recvfrom` upstream, `writev` downstream) and, in hyper, a hand-off between the
//! upstream connection task and the downstream one. On tokio's work-stealing runtime that hand-off often wakes
//! another worker (futex, park, steal); in a shard it is a local queue push. Measure with
//! `prequal-epp/examples/epp_load --router`.

use std::{io, net::TcpStream, time::Duration};

use hyper::service::service_fn;
use hyper_util::{
    rt::{TokioExecutor, TokioIo, TokioTimer},
    server::{conn::auto, graceful::GracefulShutdown},
};
use tokio::sync::{OwnedSemaphorePermit, mpsc};

use crate::forward::Forwarder;

/// An accepted client connection and its slot under `--max-connections`.
pub struct Conn {
    pub stream: TcpStream,
    pub permit: OwnedSemaphorePermit,
}

#[derive(Clone, Copy)]
pub struct ShardConfig {
    /// How long a shard lingers before parking, so tokens arriving meanwhile are handled in one pass.
    pub coalesce: Duration,
    pub header_timeout: Option<Duration>,
    pub max_streams: u32,
    /// How long in-flight requests may run on after shutdown starts.
    pub drain: Duration,
}

/// The running shards. Dropping it starts their graceful shutdown.
pub struct Shards {
    senders: Vec<mpsc::UnboundedSender<Conn>>,
    next: usize,
}

impl Shards {
    /// Hands `conn` to the next shard round-robin; `Err` (with that shard's index) if the shard has died.
    pub fn send(&mut self, conn: Conn) -> Result<(), usize> {
        let index = self.next % self.senders.len();
        self.next = self.next.wrapping_add(1);
        self.senders[index].send(conn).map_err(|_| index)
    }
}

/// Starts `count` shard threads serving the connections sent to them. `forwarder` builds each shard's forwarder
/// (and with it its upstream pool) on the shard's own runtime. Each shard sends its index on `exited` when its
/// thread ends, whether by graceful shutdown or by panic.
pub fn start<F>(
    count: usize,
    config: ShardConfig,
    forwarder: F,
    exited: mpsc::UnboundedSender<usize>,
) -> io::Result<Shards>
where
    F: Fn() -> Forwarder + Clone + Send + 'static,
{
    let senders = (0..count.max(1))
        .map(|i| {
            let (tx, rx) = mpsc::unbounded_channel::<Conn>();
            let mut builder = tokio::runtime::Builder::new_current_thread();
            if !config.coalesce.is_zero() {
                let coalesce = config.coalesce;
                builder.on_thread_park(move || std::thread::sleep(coalesce));
            }
            let runtime = builder.enable_all().build()?;
            let forwarder = forwarder.clone();
            let exit = ExitNotice { index: i, exited: exited.clone() };
            std::thread::Builder::new().name(format!("router-{i}")).spawn(move || {
                let _exit = exit;
                runtime.block_on(serve(rx, forwarder(), config));
            })?;
            Ok(tx)
        })
        .collect::<io::Result<_>>()?;
    Ok(Shards { senders, next: 0 })
}

/// Serves connections until the channel closes, then drains them for up to `config.drain`.
async fn serve(mut connections: mpsc::UnboundedReceiver<Conn>, forwarder: Forwarder, config: ShardConfig) {
    let mut builder = auto::Builder::new(TokioExecutor::new());
    builder.http1().timer(TokioTimer::new()).header_read_timeout(config.header_timeout);
    builder.http2().max_concurrent_streams(config.max_streams);
    let graceful = GracefulShutdown::new();
    while let Some(Conn { stream, permit }) = connections.recv().await {
        let Ok(stream) = tokio::net::TcpStream::from_std(stream) else { continue };
        let _ = stream.set_nodelay(true);
        let forwarder = forwarder.clone();
        let service = service_fn(move |request| forwarder.clone().handle(request));
        let connection = graceful.watch(builder.serve_connection(TokioIo::new(stream), service).into_owned());
        tokio::spawn(async move {
            let _ = connection.await;
            drop(permit);
        });
    }
    let _ = tokio::time::timeout(config.drain, graceful.shutdown()).await;
}

/// Reports a shard thread's end, including by unwinding.
struct ExitNotice {
    index: usize,
    exited: mpsc::UnboundedSender<usize>,
}

impl Drop for ExitNotice {
    fn drop(&mut self) {
        let _ = self.exited.send(self.index);
    }
}
