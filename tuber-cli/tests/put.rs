//! `tuber-cli put` and what it says about an `idp:` dedup.

mod common;

use common::scripted_server;
use std::process::{Command, Output};

fn put(replies: &[&'static [u8]]) -> Output {
    let server = scripted_server(replies);
    Command::new(env!("CARGO_BIN_EXE_tuber-cli"))
        .args(["--addr", &server.addr])
        .args(["put", "--tube", "splat.maintenance", "--idempotent", "rollup", "{}"])
        .output()
        .unwrap()
}

/// A dedup against a buried job queues nothing, and nothing runs until that job
/// is kicked. Run from cron, a put like that drops its work every time; the
/// JSON on stdout says so, but nothing is mailed unless stderr does too.
#[test]
fn a_dedup_against_a_buried_job_warns_on_stderr() {
    let out = put(&[b"USING splat.maintenance\r\n", b"INSERTED 47101 BURIED 1024\r\n"]);
    // Still a success: the key did its job.
    assert!(out.status.success(), "status: {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"duplicate\": true"), "stdout: {stdout}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning") && stderr.contains("buried job 47101"),
        "no warning on stderr: {stderr:?}"
    );
}

#[test]
fn other_dedups_and_fresh_puts_are_quiet() {
    for reply in [&b"INSERTED 47101 READY\r\n"[..], b"INSERTED 47102\r\n"] {
        let out = put(&[b"USING splat.maintenance\r\n", reply]);
        assert!(out.status.success(), "status: {:?}", out.status);
        assert!(out.stderr.is_empty(), "stderr: {:?}", String::from_utf8_lossy(&out.stderr));
    }
}

/// A dedup whose put was more urgent than the job it matched raises that job's
/// priority, and tuber says to what: `INSERTED <id> <state> <pri>`.
#[test]
fn a_dedup_that_raised_the_priority_says_so() {
    let out = put(&[b"USING splat.maintenance\r\n", b"INSERTED 47101 READY 10\r\n"]);
    assert!(out.status.success(), "status: {:?}", out.status);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        json,
        serde_json::json!({ "id": 47101, "duplicate": true, "state": "READY", "raised_pri": 10 })
    );
}

fn put_with(args: &[&str], replies: &[&'static [u8]]) -> (Output, Vec<String>) {
    let server = scripted_server(replies);
    let out = Command::new(env!("CARGO_BIN_EXE_tuber-cli"))
        .args(["--addr", &server.addr, "put"])
        .args(args)
        .output()
        .unwrap();
    (out, server.transcript())
}

/// In `idp:series:123` the last colon starts a TTL, so tuber would read key
/// `series` with a 123s TTL. --idempotent-ttl is how to give a TTL, so a colon
/// in the key is refused rather than misread.
#[test]
fn an_idempotency_key_with_a_colon_is_refused() {
    let (out, sent) = put_with(
        &["--idempotent", "series:123", "x"],
        &[b"USING default\r\n", b"INSERTED 1\r\n"],
    );
    assert_eq!(out.status.code(), Some(2), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!sent.iter().any(|l| l.starts_with("put")), "sent: {sent:?}");
}

/// --concurrency has no flag of its own for the limit, so `key:N` gives one.
#[test]
fn a_concurrency_key_may_carry_its_limit() {
    let (out, sent) = put_with(
        &["--concurrency", "user-42:3", "--idempotent", "k", "--idempotent-ttl", "60", "x"],
        &[b"USING default\r\n", b"INSERTED 1\r\n"],
    );
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(sent[1], "put 0 0 120 1 idp:k:60 con:user-42:3");
}
