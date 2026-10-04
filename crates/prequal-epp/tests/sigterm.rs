//! SIGTERM drains and exits 0. As PID 1 in a container without a handler, the binary would ignore it and wait
//! out the pod's grace period for SIGKILL.
#![cfg(unix)]

use std::{
    io::{BufRead, BufReader},
    net::{TcpListener, TcpStream},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn free_port() -> String {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port().to_string()
}

#[test]
fn sigterm_drains_and_exits_zero() {
    let (grpc, health, metrics) = (free_port(), free_port(), free_port());
    let mut child = Command::new(env!("CARGO_BIN_EXE_prequal-epp"))
        .args(["--endpoints", "127.0.0.1:9", "--secure-serving=false", "--drain-timeout", "2s"])
        .args(["--grpc-port", &grpc, "--grpc-health-port", &health, "--metrics-port", &metrics])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap()).lines().map_while(Result::ok);
    assert!(stderr.by_ref().any(|line| line.contains("ext_proc on")), "never started");
    // A connection that never completes its HTTP/2 handshake must not hold the exit past the drain timeout.
    let _idle = TcpStream::connect(format!("127.0.0.1:{grpc}")).unwrap();

    assert!(Command::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap().success());
    let sent = Instant::now();
    let exit = loop {
        if let Some(exit) = child.try_wait().unwrap() {
            break exit;
        }
        assert!(sent.elapsed() < Duration::from_secs(10), "still running 10 s after SIGTERM");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(exit.success(), "{exit}");
    let log: Vec<String> = stderr.collect();
    assert!(log.iter().any(|line| line.contains("SIGTERM")), "{log:#?}");
}
