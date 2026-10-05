use std::{
    collections::HashMap,
    io,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};

const MAX_IDLE_PER_ADDR: usize = 4;
const MAX_BODY: usize = 4 << 20;
/// Status line plus headers.
const MAX_HEAD: u64 = 64 << 10;

/// A parsed `GET` response: status, lower-cased header names, and the body.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct GetResponse {
    /// HTTP status code.
    pub status: u16,
    /// Headers in order, names lower-cased, values trimmed.
    pub headers: Vec<(String, String)>,
    /// The body (at most 4 MiB).
    pub body: Vec<u8>,
}

impl GetResponse {
    /// The first value of header `name` (lower-case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

/// Minimal HTTP/1.1 `GET` client for probing: keep-alive connections pooled per address,
/// `Content-Length` bodies only (chunked responses are read to close and not reused).
#[derive(Clone, Debug, Default)]
pub struct ProbeClient {
    idle: Arc<Mutex<HashMap<SocketAddr, Vec<BufReader<TcpStream>>>>>,
}

impl ProbeClient {
    /// `GET path` from `addr`, reusing an idle connection when one is pooled.
    pub async fn get(&self, addr: SocketAddr, path: &str) -> io::Result<GetResponse> {
        if let Some(mut conn) = self.checkout(addr) {
            // A pooled connection may have been closed by the server; retry once on a fresh one.
            if let Ok((response, reusable)) = exchange(&mut conn, addr, path).await {
                if reusable {
                    self.checkin(addr, conn);
                }
                return Ok(response);
            }
        }
        let mut conn = BufReader::new(TcpStream::connect(addr).await?);
        let (response, reusable) = exchange(&mut conn, addr, path).await?;
        if reusable {
            self.checkin(addr, conn);
        }
        Ok(response)
    }

    fn checkout(&self, addr: SocketAddr) -> Option<BufReader<TcpStream>> {
        self.idle.lock().unwrap_or_else(|p| p.into_inner()).get_mut(&addr)?.pop()
    }

    fn checkin(&self, addr: SocketAddr, conn: BufReader<TcpStream>) {
        let mut idle = self.idle.lock().unwrap_or_else(|p| p.into_inner());
        let conns = idle.entry(addr).or_default();
        if conns.len() < MAX_IDLE_PER_ADDR {
            conns.push(conn);
        }
    }
}

/// One request/response on `conn`. Returns the response and whether the connection can be reused.
async fn exchange(conn: &mut BufReader<TcpStream>, addr: SocketAddr, path: &str) -> io::Result<(GetResponse, bool)> {
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nUser-Agent: prequal\r\n\r\n");
    conn.get_mut().write_all(request.as_bytes()).await?;

    let too_large = || io::Error::new(io::ErrorKind::InvalidData, "probe response head too large");
    let mut head = (&mut *conn).take(MAX_HEAD);
    let mut line = String::new();
    head.read_line(&mut line).await?;
    let status = line
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad status line"))?;
    let (mut headers, mut body_len, mut reusable) = (Vec::new(), None, true);
    loop {
        line.clear();
        if head.read_line(&mut line).await? == 0 || !line.ends_with('\n') {
            return Err(if head.limit() == 0 { too_large() } else { io::ErrorKind::UnexpectedEof.into() });
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else { continue };
        let (name, value) = (name.trim().to_ascii_lowercase(), value.trim().to_owned());
        match name.as_str() {
            "content-length" => body_len = value.parse::<usize>().ok(),
            "connection" if value.eq_ignore_ascii_case("close") => reusable = false,
            _ => {}
        }
        headers.push((name, value));
    }
    let mut body = Vec::new();
    match body_len {
        Some(len) if len <= MAX_BODY => {
            body.resize(len, 0);
            conn.read_exact(&mut body).await?;
        }
        Some(_) => return Err(io::Error::new(io::ErrorKind::InvalidData, "probe body too large")),
        None => {
            reusable = false;
            conn.take(MAX_BODY as u64).read_to_end(&mut body).await?;
        }
    }
    Ok((GetResponse { status, headers, body }, reusable))
}
