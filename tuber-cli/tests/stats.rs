//! tuber 0.17.0-dev.4 counts jobs put back because the connection holding
//! them closed: a worker that died or quit mid-job. That and a TTR timeout
//! are the two ways a job goes back without its worker saying so, so the
//! JSON carries the one beside the other, at each level.

mod common;

use common::{scripted_server, tuber_cli};

/// `OK <bytes>` and the YAML, as the server frames a stats reply.
fn ok(yaml: &str) -> &'static [u8] {
    Box::leak(format!("OK {}\r\n{yaml}\r\n", yaml.len()).into_bytes().into_boxed_slice())
}

fn stats_json(yaml: &str, args: &[&str]) -> serde_json::Value {
    let out = tuber_cli(&scripted_server(&[ok(yaml)]), args, b"");
    assert!(out.status.success(), "{:?}: {}", out.status, String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn stats_carries_job_disconnects() {
    let v = stats_json("---\njob-timeouts: 2\njob-disconnects: 5\n", &["stats"]);
    assert_eq!(v["job_timeouts"], 2);
    assert_eq!(v["job_disconnects"], 5);
}

#[test]
fn stats_tube_carries_total_disconnects() {
    let v = stats_json(
        "---\nname: \"emails\"\ntotal-timeouts: 1\ntotal-disconnects: 3\n",
        &["stats-tube", "emails"],
    );
    assert_eq!(v["total_timeouts"], 1);
    assert_eq!(v["total_disconnects"], 3);
}

#[test]
fn stats_job_carries_disconnects() {
    let v = stats_json(
        "---\nid: 7\ntube: \"emails\"\nstate: \"ready\"\ntimeouts: 0\ndisconnects: 2\n",
        &["stats-job", "7"],
    );
    assert_eq!(v["timeouts"], 0);
    assert_eq!(v["disconnects"], 2);
}

/// Servers before tuber 0.17.0-dev.4, and beanstalkd, don't count them.
/// The key is still there, as 0, as `current_jobs_held` is.
#[test]
fn a_server_that_does_not_count_them_reads_zero() {
    let v = stats_json("---\nid: 7\ntube: \"emails\"\nstate: \"ready\"\ntimeouts: 1\n", &["stats-job", "7"]);
    assert_eq!(v["disconnects"], 0);
}
