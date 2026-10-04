/// CPU seconds (user + kernel) and peak working set of the current process, when the OS exposes them.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessUsage {
    pub cpu_s: f64,
    pub peak_mb: f64,
}

impl ProcessUsage {
    pub fn encode(self) -> String {
        format!("usage {:.3} {:.1}", self.cpu_s, self.peak_mb)
    }

    pub fn decode(line: &str) -> Option<Self> {
        let mut parts = line.strip_prefix("usage ")?.split(' ');
        Some(Self { cpu_s: parts.next()?.parse().ok()?, peak_mb: parts.next()?.parse().ok()? })
    }
}

#[cfg(windows)]
pub fn current() -> ProcessUsage {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::{
            ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{GetCurrentProcess, GetProcessTimes},
        },
    };

    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32 | u64::from(t.dwLowDateTime)) as f64;
    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    let mut memory: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    memory.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY: the pseudo-handle is always valid and every out-pointer refers to a live local.
    let ok = unsafe {
        let process = GetCurrentProcess();
        GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) != 0
            && K32GetProcessMemoryInfo(process, &mut memory, memory.cb) != 0
    };
    if !ok {
        return ProcessUsage::default();
    }
    ProcessUsage {
        cpu_s: (ticks(kernel) + ticks(user)) / 1e7,
        peak_mb: memory.PeakWorkingSetSize as f64 / (1024.0 * 1024.0),
    }
}

#[cfg(unix)]
pub fn current() -> ProcessUsage {
    // SAFETY: getrusage only writes the zeroed local.
    let mut usage = unsafe { std::mem::zeroed::<libc::rusage>() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return ProcessUsage::default();
    }
    let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    // ru_maxrss is KiB on Linux, bytes on macOS.
    let rss_unit = if cfg!(target_os = "macos") { 1.0 } else { 1024.0 };
    ProcessUsage {
        cpu_s: secs(usage.ru_utime) + secs(usage.ru_stime),
        peak_mb: usage.ru_maxrss as f64 * rss_unit / (1024.0 * 1024.0),
    }
}

#[cfg(not(any(unix, windows)))]
pub fn current() -> ProcessUsage {
    ProcessUsage::default()
}
