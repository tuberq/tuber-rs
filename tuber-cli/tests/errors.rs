//! How a failed command reports itself: a JSON error on stderr (plain text
//! with `--format text`), never a backtrace, and an exit code per kind of
//! failure, so a script can tell "no such job" from "no server".

mod common;

use common::scripted_server;
use std::process::{Command, Output};

fn run(replies: &[&'static [u8]], args: &[&str]) -> Output {
    let server = scripted_server(replies);
    run_at(&server.addr, args)
}

fn run_at(addr: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tuber-cli"))
        .args(["--addr", addr])
        .args(args)
        // anyhow prints a backtrace with this set, if main returns its error.
        .env("RUST_BACKTRACE", "1")
        .output()
        .unwrap()
}

/// The JSON error on stderr, and the exit code.
fn failure(out: &Output) -> (serde_json::Value, Option<i32>) {
    assert!(out.stdout.is_empty(), "stdout: {}", String::from_utf8_lossy(&out.stdout));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("acktrace"), "a backtrace: {stderr}");
    let err = serde_json::from_str(&stderr).unwrap_or_else(|e| panic!("{e}: {stderr}"));
    (err, out.status.code())
}

#[test]
fn not_found_is_a_json_error_and_exits_3() {
    let (err, code) = failure(&run(&[b"NOT_FOUND\r\n"], &["delete", "999"]));
    assert_eq!(err["error"], "NOT_FOUND", "{err}");
    assert!(err["message"].is_string(), "{err}");
    assert_eq!(code, Some(3));
}

/// Replies, arguments, the `error` reported, and the exit code.
type Case<'a> = (&'a [&'static [u8]], &'a [&'a str], &'a str, i32);

#[test]
fn each_kind_of_failure_has_its_own_exit_code() {
    let cases: [Case; 7] = [
        (&[b"TIMED_OUT\r\n"], &["reserve"], "TIMED_OUT", 4),
        (&[b"DEADLINE_SOON\r\n"], &["reserve-batch", "10", "--timeout", "5"], "DEADLINE_SOON", 4),
        (&[b"DEADLINE_SOON\r\n"], &["reserve"], "DEADLINE_SOON", 4),
        (&[b"USING default\r\n", b"DRAINING\r\n"], &["put", "x"], "DRAINING", 5),
        (&[b"UNKNOWN_COMMAND\r\n"], &["drain"], "UNKNOWN_COMMAND", 5),
        (&[], &["stats-tube", "two words"], "INVALID_INPUT", 2),
        (&[b"WHAT\r\n"], &["delete", "1"], "PROTOCOL_ERROR", 6),
    ];
    for (replies, args, error, exit) in cases {
        let (err, code) = failure(&run(replies, args));
        assert_eq!((err["error"].as_str(), code), (Some(error), Some(exit)), "{args:?}: {err}");
    }
}

#[test]
fn no_server_exits_6() {
    // A port that was just free: nothing is listening on it.
    let addr = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    let (err, code) = failure(&run_at(&addr, &["stats"]));
    assert_eq!((err["error"].as_str(), code), (Some("UNREACHABLE"), Some(6)), "{err}");
    // Said once, not "Connection refused: Connection refused".
    let message = err["message"].as_str().unwrap();
    assert_eq!(message.matches("onnection refused").count(), 1, "{message}");
}

#[test]
fn text_format_errors_are_one_plain_line() {
    let out = run(&[b"NOT_FOUND\r\n"], &["--format", "text", "delete", "999"]);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "Error: NOT_FOUND\n");
    assert_eq!(out.status.code(), Some(3));
}

/// A bad argument is refused before connecting, so it exits 2 whether a
/// server is up or not, rather than 6 when none is.
#[test]
fn a_bad_argument_exits_2_with_no_server_up() {
    let addr = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    let dir = env!("CARGO_TARGET_TMPDIR");
    let bad_entry = format!("{dir}/bad-entry.json");
    std::fs::write(&bad_entry, r#"[{"name":"r","tube":"two words","spec":"@daily","body":"x"}]"#).unwrap();
    let missing = format!("{dir}/no-such-schedule-file.json");
    let cases: [(&[&str], &str, i32); 10] = [
        (&["stats-tube", "two words"], "INVALID_INPUT", 2),
        (&["put", "--tube", "two words", "x"], "INVALID_INPUT", 2),
        (&["put", "--group", "two words", "x"], "INVALID_INPUT", 2),
        (&["put", "--concurrency", "k:many", "x"], "INVALID_INPUT", 2),
        (&["reserve", "--tube", "two words"], "INVALID_INPUT", 2),
        (&["peek-ready", "--tube", "two words"], "INVALID_INPUT", 2),
        (&["stats-group", "two words"], "INVALID_INPUT", 2),
        (&["stats-schedule", "ops", "two words"], "INVALID_INPUT", 2),
        (&["schedule-sync", "ops", &bad_entry], "INVALID_INPUT", 2),
        (&["schedule-sync", "ops", &missing], "ERROR", 1),
    ];
    for (args, error, exit) in cases {
        let (err, code) = failure(&run_at(&addr, args));
        assert_eq!((err["error"].as_str(), code), (Some(error), Some(exit)), "{args:?}: {err}");
    }
}

/// With no server, `put` fails at once rather than first reading a body
/// from stdin that may be a long time coming, or never come at all.
#[test]
fn put_with_no_server_fails_without_waiting_for_stdin() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let addr = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tuber-cli"))
        .args(["--addr", &addr, "put"])
        // Held open, and never written to.
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if start.elapsed() > Duration::from_secs(5) {
            child.kill().unwrap();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.and_then(|s| s.code()), Some(6), "still waiting on stdin after 5s");
}
