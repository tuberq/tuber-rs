//! tuber-tui started with no terminal: from cron, a service, or a pipe.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// It has nothing to draw on, so it says so and exits 1. It used to panic
/// inside ratatui::init, exit 101, and print a backtrace hint.
#[test]
fn without_a_terminal_it_fails_with_an_error_not_a_panic() {
    // With a controlling terminal the child would find it through /dev/tty
    // and start for real; this checks the case where there is none.
    if std::fs::File::open("/dev/tty").is_ok() {
        eprintln!("skipped: this process has a controlling terminal");
        return;
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_tuber-tui"))
        .args(["-a", "127.0.0.1:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(10) {
            child.kill().unwrap();
            panic!("still running after 10s");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "panicked: {stderr}");
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("terminal"), "stderr: {stderr}");
}
