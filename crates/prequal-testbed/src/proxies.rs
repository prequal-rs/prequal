//! Runs N proxy/router processes in front of a fleet (a dedicated LB tier).

use std::{
    io,
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub struct ProxyFleet {
    children: Vec<Child>,
    pub addrs: Vec<SocketAddr>,
}

impl ProxyFleet {
    /// Starts `count` copies of `bin`; `configure` adds each copy's arguments/env given the
    /// address it must listen on. Waits until every copy accepts connections.
    /// `PREQUAL_STATS=<dir>` sends each copy's stderr to `<dir>/proxy-<i>.log`; `PREQUAL_PROXY_CPUS` pins them.
    pub fn spawn(bin: &Path, count: usize, configure: impl Fn(&mut Command, SocketAddr)) -> io::Result<Self> {
        let mut fleet = Self { children: Vec::new(), addrs: Vec::new() };
        let stats_dir = std::env::var_os("PREQUAL_STATS").map(PathBuf::from);
        for i in 0..count {
            let addr = free_local_addr()?;
            let stderr = match &stats_dir {
                Some(dir) => Stdio::from(std::fs::File::create(dir.join(format!("proxy-{i}.log")))?),
                None => Stdio::null(),
            };
            let mut cmd = crate::pinned_command(bin, "PREQUAL_PROXY_CPUS");
            configure(&mut cmd, addr);
            fleet.children.push(cmd.stdout(Stdio::null()).stderr(stderr).spawn()?);
            fleet.addrs.push(addr);
        }
        fleet.wait_until_listening(Duration::from_secs(20))?;
        Ok(fleet)
    }

    fn wait_until_listening(&self, limit: Duration) -> io::Result<()> {
        let deadline = Instant::now() + limit;
        for addr in &self.addrs {
            while TcpStream::connect_timeout(addr, Duration::from_millis(200)).is_err() {
                if Instant::now() > deadline {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, format!("proxy {addr} never listened")));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(())
    }
}

impl Drop for ProxyFleet {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// Racy (the port could be taken before the proxy binds) but fine for a local benchmark.
fn free_local_addr() -> io::Result<SocketAddr> {
    TcpListener::bind("127.0.0.1:0")?.local_addr()
}
