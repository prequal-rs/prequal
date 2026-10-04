//! Termination signals and the drain they start: readiness flips to NOT_SERVING, ext_proc stops taking new
//! streams, and in-flight ones get up to `--drain-timeout` to finish.

use std::{io, time::Duration};

use tokio::sync::watch;

/// SIGTERM and SIGINT handlers, installed when built. Without them the binary, as PID 1 in a container, ignores
/// SIGTERM entirely (the kernel drops default-disposition signals to PID 1).
pub struct Signals {
    #[cfg(unix)]
    term: tokio::signal::unix::Signal,
    #[cfg(unix)]
    int: tokio::signal::unix::Signal,
}

impl Signals {
    pub fn install() -> io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Self { term: signal(SignalKind::terminate())?, int: signal(SignalKind::interrupt())? })
        }
        #[cfg(not(unix))]
        Ok(Self {})
    }

    /// Resolves with the signal's name once one arrives.
    pub async fn recv(&mut self) -> &'static str {
        #[cfg(unix)]
        tokio::select! {
            _ = self.term.recv() => "SIGTERM",
            _ = self.int.recv() => "SIGINT",
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            "Ctrl-C"
        }
    }
}

/// Broadcasts the start of a drain to every server.
pub fn channel() -> (watch::Sender<bool>, Draining) {
    let (tx, rx) = watch::channel(false);
    (tx, Draining(rx))
}

#[derive(Clone)]
pub struct Draining(watch::Receiver<bool>);

impl Draining {
    pub fn started(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once the drain starts (or its sender is gone).
    pub async fn wait(mut self) {
        let _ = self.0.wait_for(|draining| *draining).await;
    }
}

/// Parses a Go-style duration as llm-d's flags take it (`30s`, `1m30s`, `500ms`); a bare number is seconds.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let bad = || format!("invalid duration {s:?} (expected e.g. 25s, 1m30s, 500ms)");
    if let Ok(secs) = s.parse::<f64>() {
        return Duration::try_from_secs_f64(secs).map_err(|_| bad());
    }
    let mut total = Duration::ZERO;
    let mut rest = s;
    while !rest.is_empty() {
        let split = rest.find(|c: char| !c.is_ascii_digit() && c != '.').ok_or_else(bad)?;
        let (number, tail) = rest.split_at(split);
        let unit_len = tail.find(|c: char| c.is_ascii_digit()).unwrap_or(tail.len());
        let (unit, next) = tail.split_at(unit_len);
        let scale = match unit {
            "ms" => 1e-3,
            "s" => 1.0,
            "m" => 60.0,
            "h" => 3600.0,
            _ => return Err(bad()),
        };
        let value: f64 = number.parse().map_err(|_| bad())?;
        total += Duration::try_from_secs_f64(value * scale).map_err(|_| bad())?;
        rest = next;
    }
    if s.is_empty() { Err(bad()) } else { Ok(total) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_go_durations() {
        assert_eq!(parse_duration("25s"), Ok(Duration::from_secs(25)));
        assert_eq!(parse_duration("1m30s"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("7"), Ok(Duration::from_secs(7)));
        assert_eq!(parse_duration("0s"), Ok(Duration::ZERO));
        for bad in ["", "s", "10x", "-1", "1.5.2s"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn draining_wakes_waiters() {
        let (tx, draining) = channel();
        assert!(!draining.started());
        let waiter = tokio::spawn(draining.clone().wait());
        tx.send(true).unwrap();
        waiter.await.unwrap();
        assert!(draining.started());
    }
}
