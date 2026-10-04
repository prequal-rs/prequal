//! The client side when a real Envoy sits in front of the EPP: one streaming completion per request over pooled
//! keep-alive HTTP/1.1 connections, as inference-perf (aiohttp) sends them.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Instant,
};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

use crate::driver::Outcome;

#[derive(Clone)]
pub struct Client {
    addr: SocketAddr,
    idle: Arc<Mutex<Vec<BufReader<TcpStream>>>>,
}

impl Client {
    pub fn new(addr: SocketAddr) -> Self {
        Self { addr, idle: Arc::default() }
    }

    /// Posts one completion and reads its SSE stream to the end. `pick` is the time to the first token.
    pub async fn run(self, body: Vec<u8>) -> Outcome {
        let mut outcome =
            Outcome { routed: false, pick: Default::default(), sent: 1, received: 0, echo_us: Vec::new() };
        let started = Instant::now();
        let pooled = self.idle.lock().unwrap().pop();
        let mut conn = match pooled {
            Some(conn) => conn,
            None => match TcpStream::connect(self.addr).await {
                Ok(s) => {
                    let _ = s.set_nodelay(true);
                    BufReader::with_capacity(64 << 10, s)
                }
                Err(_) => return outcome,
            },
        };
        let head = format!(
            "POST /v1/completions HTTP/1.1\r\nHost: router-epp\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let sent = async {
            conn.get_mut().write_all(head.as_bytes()).await?;
            conn.get_mut().write_all(&body).await
        };
        if sent.await.is_err() {
            return outcome;
        }
        if let Ok(true) = read_stream(&mut conn, &mut outcome, started).await {
            self.idle.lock().unwrap().push(conn);
        }
        outcome
    }
}

/// Reads a chunked `200` SSE response; returns whether the connection can be reused.
async fn read_stream(
    conn: &mut BufReader<TcpStream>,
    outcome: &mut Outcome,
    started: Instant,
) -> std::io::Result<bool> {
    let mut line = String::new();
    conn.read_line(&mut line).await?;
    let ok = line.starts_with("HTTP/1.1 200");
    loop {
        line.clear();
        if conn.read_line(&mut line).await? == 0 {
            return Ok(false);
        }
        if line == "\r\n" {
            break;
        }
    }
    if !ok {
        return Ok(false);
    }
    let mut data = Vec::new();
    loop {
        line.clear();
        if conn.read_line(&mut line).await? == 0 {
            return Ok(false);
        }
        let Ok(size) = usize::from_str_radix(line.trim_end(), 16) else { return Ok(false) };
        data.resize(size + 2, 0);
        conn.read_exact(&mut data).await?;
        if size == 0 {
            return Ok(true);
        }
        if !outcome.routed {
            outcome.routed = true;
            outcome.pick = started.elapsed();
        }
        // Envoy may merge or split the upstream's chunks; count SSE events instead.
        outcome.received += data.windows(6).filter(|w| w == b"data: ").count();
    }
}
