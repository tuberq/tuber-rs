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
