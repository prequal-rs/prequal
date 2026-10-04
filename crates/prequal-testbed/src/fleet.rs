//! Runs the server fleet in a child process so an overloaded client can't distort server timing.

use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::SocketAddr,
    process::{Child, ChildStdin, ChildStdout, Stdio},
};

use crate::metrics::{self, ProcessUsage};

pub struct FleetProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    pub addrs: Vec<SocketAddr>,
}

impl FleetProcess {
    /// Re-executes this binary with `--serve` plus `fleet_args` (pinned per `PREQUAL_FLEET_CPUS`), then waits for its
    /// addresses.
    pub fn spawn(fleet_args: &[String]) -> io::Result<Self> {
        let mut child = crate::pinned_command(std::env::current_exe()?, "PREQUAL_FLEET_CPUS")
            .arg("--serve")
            .args(fleet_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take();
        let mut stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let mut addrs = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            if stdout.read_line(&mut line)? == 0 {
                return Err(io::Error::other("fleet exited before becoming ready"));
            }
            match line.trim() {
                "ready" => break,
                addr => addrs.push(addr.parse().map_err(io::Error::other)?),
            }
        }
        Ok(Self { child, stdin, stdout, addrs })
    }

    /// Closes the fleet's stdin (its shutdown signal) and returns its CPU and memory usage.
    pub fn shutdown(mut self) -> io::Result<ProcessUsage> {
        drop(self.stdin.take());
        let mut rest = String::new();
        self.stdout.read_to_string(&mut rest)?;
        self.child.wait()?;
        Ok(rest.lines().find_map(ProcessUsage::decode).unwrap_or_default())
    }
}

/// Child side: announce addresses, block until the parent closes stdin, then report usage.
pub fn announce_and_wait(addrs: &[SocketAddr]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    for addr in addrs {
        writeln!(out, "{addr}")?;
    }
    writeln!(out, "ready")?;
    out.flush()?;
    io::stdin().lock().read_to_end(&mut Vec::new())?;
    writeln!(out, "{}", metrics::current().encode())?;
    out.flush()
}
