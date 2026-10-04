//! Shutdown signals. Handlers are installed explicitly because as PID 1 (distroless) the default SIGTERM
//! disposition is ignored.

use std::{future::Future, io};

/// Installs the handlers now and returns a future that resolves with the first signal's name.
#[cfg(unix)]
pub fn install() -> io::Result<impl Future<Output = &'static str>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! {
            _ = terminate.recv() => "SIGTERM",
            _ = interrupt.recv() => "SIGINT",
        }
    })
}

/// Installs the handler on first poll and returns a future that resolves on Ctrl-C.
#[cfg(not(unix))]
pub fn install() -> io::Result<impl Future<Output = &'static str>> {
    Ok(async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => "Ctrl-C",
            // No handler means no signal ever arrives.
            Err(_) => std::future::pending().await,
        }
    })
}
