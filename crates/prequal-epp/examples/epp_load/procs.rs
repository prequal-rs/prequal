//! Child processes and their CPU accounting (procfs; `None` off Linux).

use std::{
    path::{Path, PathBuf},
    process::{Child, Command},
};

/// Kills the child however the harness exits, so no orphan keeps burning a shared box.
pub struct Reaped(pub Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `binary`, under `taskset -c cpus` when given.
pub fn pinned(binary: &Path, cpus: Option<&str>) -> Command {
    match cpus {
        Some(cpus) => {
            let mut taskset = Command::new("taskset");
            taskset.args(["-c", cpus]).arg(binary);
            taskset
        }
        None => Command::new(binary),
    }
}

/// (CPU seconds, peak RSS KiB) of a process.
pub fn cpu_and_peak(pid: u32) -> Option<(f64, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let ticks: f64 = fields[11].parse::<f64>().ok()? + fields[12].parse::<f64>().ok()?;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let hwm = status.lines().find_map(|l| l.strip_prefix("VmHWM:"))?.trim().trim_end_matches(" kB").parse().ok()?;
    Some((ticks / 100.0, hwm))
}

/// CPU seconds of a process (`taskset` execs the binary, so the pid is the binary's).
pub fn cpu(pid: u32) -> Option<f64> {
    cpu_and_peak(pid).map(|(cpu, _)| cpu)
}

/// Busy CPU seconds of the whole host, to flag runs disturbed by other work on a shared box.
pub fn host_busy() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let cpu: Vec<f64> = stat.lines().next()?.split_whitespace().skip(1).filter_map(|v| v.parse().ok()).collect();
    let idle = cpu.get(3)? + cpu.get(4)?;
    Some((cpu.iter().take(8).sum::<f64>() - idle) / 100.0)
}

pub fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// The `prequal-epp` binary next to this example's directory.
pub fn default_epp() -> PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    exe.parent()
        .and_then(|d| d.parent())
        .expect("target dir")
        .join(format!("prequal-epp{}", std::env::consts::EXE_SUFFIX))
}
