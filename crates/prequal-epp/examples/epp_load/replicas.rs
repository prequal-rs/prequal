//! Fake model servers on keep-alive HTTP/1.1: `GET` answers with a captured llm-d-inference-sim `/metrics`
//! exposition, so the EPP's scrape loop costs what it does against the real simulator; `POST` (only reached through
//! Envoy) streams an SSE completion the way the simulator does: a prefill pause, then one chunked-encoding chunk
//! per token.

use std::{net::SocketAddr, time::Duration};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

const SIM_METRICS: &str = include_str!("sim-metrics.txt");

/// Streamed completions: tokens per response and the gap between them.
#[derive(Clone, Copy)]
pub struct Stream {
    pub tokens: usize,
    pub itl: Duration,
}

pub async fn start(count: usize, stream: Stream) -> std::io::Result<Vec<SocketAddr>> {
    let metrics: &'static [u8] = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\n\r\n{SIM_METRICS}",
        SIM_METRICS.len()
    )
    .leak()
    .as_bytes();
    let mut addrs = Vec::with_capacity(count);
    for _ in 0..count {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        addrs.push(listener.local_addr()?);
        tokio::spawn(async move {
            while let Ok((stream_, _)) = listener.accept().await {
                let _ = stream_.set_nodelay(true);
                tokio::spawn(serve(stream_, metrics, stream));
            }
        });
    }
    Ok(addrs)
}

/// Reads one request; returns (is POST, body bytes). Envoy forwards ext_proc-streamed bodies chunked.
async fn read_request(conn: &mut BufReader<TcpStream>) -> std::io::Result<Option<(bool, usize)>> {
    let mut line = String::new();
    if conn.read_line(&mut line).await? == 0 {
        return Ok(None);
    }
    let post = line.starts_with("POST");
    let (mut length, mut chunked) = (0, false);
    loop {
        line.clear();
        if conn.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        if line == "\r\n" {
            break;
        }
        let Some((key, value)) = line.split_once(':') else { continue };
        if key.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().unwrap_or(0);
        } else if key.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value.trim().eq_ignore_ascii_case("chunked");
        }
    }
    let mut body = vec![0u8; length];
    conn.read_exact(&mut body).await?;
    while chunked {
        line.clear();
        conn.read_line(&mut line).await?;
        let size = usize::from_str_radix(line.trim_end(), 16).map_err(std::io::Error::other)?;
        body.resize(size + 2, 0);
        conn.read_exact(&mut body).await?;
        length += size;
        chunked = size > 0;
    }
    Ok(Some((post, length)))
}

async fn serve(conn: TcpStream, metrics: &'static [u8], stream: Stream) {
    let mut conn = BufReader::with_capacity(64 << 10, conn);
    while let Ok(Some((post, body_len))) = read_request(&mut conn).await {
        if !post {
            if conn.get_mut().write_all(metrics).await.is_err() {
                return;
            }
            continue;
        }
        let Ok(std) = conn.into_inner().into_std() else { return };
        let streamed = tokio::task::spawn_blocking(move || complete(std, body_len, stream).ok()).await;
        let Some(Ok(std)) = streamed.ok().flatten().map(|s| s.set_nonblocking(true).map(|()| s)) else { return };
        let Ok(tcp) = TcpStream::from_std(std) else { return };
        conn = BufReader::with_capacity(64 << 10, tcp);
    }
}

/// Simulator timing: 5 ms prefill overhead plus 4 µs per prompt token (~4 bytes), then `tokens` chunks. On its own
/// thread with OS sleeps: tokio's 1 ms timer wheel would fire every stream's tokens in the same tick, and those
/// synchronized bursts let Envoy and the EPP batch ~1.7x more messages per syscall than real, desynchronized
/// simulators do (measured in kind with tools/epp-syscalls.bt).
fn complete(
    mut conn: std::net::TcpStream,
    prompt_bytes: usize,
    stream: Stream,
) -> std::io::Result<std::net::TcpStream> {
    use std::io::Write;
    const HEAD: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
    conn.set_nonblocking(false)?;
    let start = std::time::Instant::now() + Duration::from_micros(5000 + prompt_bytes as u64);
    std::thread::sleep(start.saturating_duration_since(std::time::Instant::now()));
    conn.write_all(HEAD)?;
    for i in 0..=stream.tokens {
        std::thread::sleep((start + stream.itl * i as u32).saturating_duration_since(std::time::Instant::now()));
        let data = if i < stream.tokens {
            format!(
                "data: {{\"id\":\"cmpl-{i:016x}\",\"object\":\"text_completion\",\"created\":1727700000,\"model\":\"Qwen/Qwen3-8B\",\"choices\":[{{\"index\":0,\"text\":\" tok{i}\",\"logprobs\":null,\"finish_reason\":null}}]}}\n\n"
            )
        } else {
            "data: [DONE]\n\n".to_owned()
        };
        conn.write_all(format!("{:x}\r\n{data}\r\n", data.len()).as_bytes())?;
    }
    conn.write_all(b"0\r\n\r\n")?;
    Ok(conn)
}
