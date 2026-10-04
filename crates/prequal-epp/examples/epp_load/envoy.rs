//! A real Envoy in front of the EPP, configured exactly like llm-d's standalone chart (`envoy.yaml`), so the EPP
//! sees Envoy's actual ext_proc message pattern and batching rather than the direct driver's.

use std::{net::SocketAddr, path::Path, process::Stdio, time::Duration};

use crate::procs::{Reaped, free_port, pinned};

const CONFIG: &str = include_str!("envoy.yaml");

/// How Envoy runs: `config` replaces the chart's `envoy.yaml` (same placeholders) to price config levers.
pub struct Options<'a> {
    pub cpus: Option<&'a str>,
    pub response_body_mode: &'a str,
    pub concurrency: u32,
    pub config: Option<&'a Path>,
}

/// Starts Envoy proxying to the EPP's ext_proc port; returns it and its listener once it accepts connections.
/// `response_body_mode` overrides the chart's FULL_DUPLEX_STREAMED (e.g. NONE, to price the per-token stream).
pub async fn start(binary: &Path, ext_proc_port: u16, options: &Options<'_>) -> std::io::Result<(Reaped, SocketAddr)> {
    let listen = free_port()?;
    let template = match options.config {
        Some(path) => std::fs::read_to_string(path)?,
        None => CONFIG.to_owned(),
    };
    let config = template
        .replace(
            "response_body_mode: FULL_DUPLEX_STREAMED",
            &format!("response_body_mode: {}", options.response_body_mode),
        )
        .replace("ADMIN_PORT", &free_port()?.to_string())
        .replace("LISTEN_PORT", &listen.to_string())
        .replace("EXT_PROC_PORT", &ext_proc_port.to_string());
    let path = std::env::temp_dir().join(format!("epp_load-envoy-{}.yaml", std::process::id()));
    std::fs::write(&path, config)?;
    let mut command = pinned(binary, options.cpus);
    command
        .args(["--concurrency", &options.concurrency.to_string(), "--log-level", "warn", "--base-id"])
        .arg(listen.to_string())
        .arg("-c")
        .arg(&path);
    let envoy = Reaped(command.stdout(Stdio::null()).stderr(Stdio::null()).spawn()?);
    let addr = SocketAddr::from(([127, 0, 0, 1], listen));
    wait_listening(addr).await?;
    Ok((envoy, addr))
}

pub async fn wait_listening(addr: SocketAddr) -> std::io::Result<()> {
    for _ in 0..200 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(std::io::Error::other(format!("nothing listening on {addr}")))
}
