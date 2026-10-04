//! Shared plumbing for the benchmark binaries: child-process fleets, proxy tiers, process metrics.

pub mod fleet;
pub mod metrics;
pub mod proxies;

use std::{ffi::OsStr, io, process::Command};

/// A `Command` for `program`, run under `taskset -c <list>` when env var `cpus_var` holds a CPU list (e.g. `2-5,8-11`).
pub fn pinned_command(program: impl AsRef<OsStr>, cpus_var: &str) -> Command {
    match std::env::var(cpus_var) {
        Ok(cpus) if !cpus.is_empty() => {
            let mut cmd = Command::new("taskset");
            cmd.arg("-c").arg(cpus).arg(program);
            cmd
        }
        _ => Command::new(program),
    }
}

#[cfg(windows)]
pub fn raise_timer_resolution() {
    // Windows' default 15.6 ms timer tick would swamp millisecond-scale service times.
    unsafe { windows_sys::Win32::Media::timeBeginPeriod(1) };
}

#[cfg(not(windows))]
pub fn raise_timer_resolution() {}

/// Raises the open-file soft limit to the hard limit; children (fleet, proxies) inherit it.
#[cfg(unix)]
pub fn raise_fd_limit() {
    // Linux defaults to 1024; a client plus its probe pool exceeds that and fails connects, which reads as errors.
    // SAFETY: getrlimit/setrlimit only touch the zeroed local.
    unsafe {
        let mut limit = std::mem::zeroed::<libc::rlimit>();
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0 && limit.rlim_cur < limit.rlim_max {
            limit.rlim_cur = limit.rlim_max;
            libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
        }
    }
}

#[cfg(not(unix))]
pub fn raise_fd_limit() {}

pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> io::Result<T> + Send + 'static) -> io::Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(io::Error::other)?
}
