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
}

#[test]
fn text_format_errors_are_one_plain_line() {
    let out = run(&[b"NOT_FOUND\r\n"], &["--format", "text", "delete", "999"]);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "Error: NOT_FOUND\n");
    assert_eq!(out.status.code(), Some(3));
}
