//! End-to-end: the router binary in front of a fake engine.

use std::{
    convert::Infallible,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use bytes::Bytes;
use http::{Request, Response};
use http_body_util::Full;
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;

const METRICS: &str = "vllm:num_requests_running 0\nvllm:num_requests_waiting 0\nvllm:kv_cache_usage_perc 0.1\n";

async fn engine(request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let body = if request.uri().path() == "/metrics" { METRICS } else { "data: hello\n\n" };
    Ok(Response::new(Full::new(Bytes::from_static(body.as_bytes()))))
}

/// A vLLM stand-in serving `/metrics` and a one-event SSE completion.
fn fake_engine() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(http1::Builder::new().serve_connection(TokioIo::new(stream), service_fn(engine)));
            }
        });
    });
    addr
}

fn free_port() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap()
}

struct Router {
    child: Child,
    listen: SocketAddr,
    admin: SocketAddr,
}

impl Router {
    fn start(extra: &[&str]) -> Self {
        let (listen, admin) = (free_port(), free_port());
        let child = Command::new(env!("CARGO_BIN_EXE_prequal-router"))
            .args(["--listen", &listen.to_string(), "--admin-listen", &admin.to_string()])
            .args(extra)
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Self { child, listen, admin }
    }

    /// Polls `/readyz` until it answers `want`.
    fn await_ready(&self, want: u16) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while status(&http(self.admin, "GET /readyz HTTP/1.1\r\nHost: r\r\nConnection: close\r\n\r\n")) != want {
            assert!(Instant::now() < deadline, "/readyz never answered {want}");
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Router {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn http(addr: SocketAddr, request: &str) -> String {
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(1)) else { return String::new() };
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut response = String::new();
    if stream.write_all(request.as_bytes()).is_ok() {
        let _ = stream.read_to_string(&mut response);
    }
    response
}

fn status(response: &str) -> u16 {
    response.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn post(router: &Router, body: &str) -> String {
    let request = format!(
        "POST /v1/completions HTTP/1.1\r\nHost: r\r\nConnection: close\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    http(router.listen, &request)
}

#[test]
fn proxies_streams_and_enforces_body_limits() {
    let engine = fake_engine();
    let router = Router::start(&["--max-body-mib", "1", "--scrape-ms", "20", &engine.to_string()]);
    router.await_ready(200);
    let response = post(&router, r#"{"model":"m","prompt":"hello","stream":true}"#);
    assert_eq!(status(&response), 200, "{response}");
    assert!(response.contains("data: hello"), "{response}");
    let oversized = "POST /v1/completions HTTP/1.1\r\nHost: r\r\nConnection: close\r\nContent-Length: 2097152\r\n\r\n";
    assert_eq!(status(&http(router.listen, oversized)), 413);
    let health = http(router.admin, "GET /healthz HTTP/1.1\r\nHost: r\r\nConnection: close\r\n\r\n");
    assert_eq!(status(&health), 200);
}

#[test]
fn not_ready_while_no_replica_is_up() {
    let dead = free_port();
    let router = Router::start(&["--scrape-ms", "20", "--connect-timeout-ms", "200", &dead.to_string()]);
    router.await_ready(503);
    assert_eq!(status(&post(&router, r#"{"prompt":"x"}"#)), 502, "the only replica is unreachable");
}

#[test]
fn startup_errors_exit_non_zero_with_a_message() {
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_prequal-router"))
        .args(["--listen", &taken.local_addr().unwrap().to_string(), "127.0.0.1:9"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("prequal-router: bind") && !stderr.contains("panicked"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn sigterm_drains_and_exits_zero() {
    let engine = fake_engine();
    let mut router = Router::start(&["--drain-secs", "5", &engine.to_string()]);
    router.await_ready(200);
    let killed = Command::new("kill").args(["-TERM", &router.child.id().to_string()]).status().unwrap();
    assert!(killed.success());
    let deadline = Instant::now() + Duration::from_secs(10);
    let exit = loop {
        if let Some(exit) = router.child.try_wait().unwrap() {
            break exit;
        }
        assert!(Instant::now() < deadline, "still running 10 s after SIGTERM");
        thread::sleep(Duration::from_millis(50));
    };
    assert!(exit.success(), "{exit}");
}
