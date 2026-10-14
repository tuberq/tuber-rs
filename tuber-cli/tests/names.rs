//! A tube, group or tag name is sent inside a command line, so one holding a
//! CRLF would end that command and start another.

mod common;

use common::scripted_server;
use std::process::Command;

/// `stats-tube $'default\r\nflush-tube victim'` once exited 0 and emptied
/// `victim`. It must fail without sending anything.
#[test]
fn a_name_with_a_crlf_does_not_reach_the_server() {
    // Answers for both halves, so the old behaviour shows as a success and a
    // flush rather than as a hang.
    let server = scripted_server(&[b"OK 4\r\n---\n\r\n", b"FLUSHED 3\r\n"]);
    let out = Command::new(env!("CARGO_BIN_EXE_tuber-cli"))
        .args(["--addr", &server.addr])
        .args(["stats-tube", "default\r\nflush-tube victim"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "status: {:?}", out.status);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("invalid tube name"), "stderr: {stderr}");
    let sent = server.transcript();
    assert!(!sent.iter().any(|l| l.contains("flush-tube")), "sent: {sent:?}");
}

#[test]
fn a_tag_with_a_space_does_not_reach_the_server() {
    let server = scripted_server(&[b"USING default\r\n", b"INSERTED 1\r\n"]);
    let out = Command::new(env!("CARGO_BIN_EXE_tuber-cli"))
        .args(["--addr", &server.addr])
        .args(["put", "--group", "batch-1 aft:batch-1", "{}"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "status: {:?}", out.status);
    let sent = server.transcript();
    assert!(!sent.iter().any(|l| l.starts_with("put")), "sent: {sent:?}");
}

/// `delete-batch` with no ids is a mistake, not a request to delete nothing.
#[test]
fn a_bare_delete_batch_is_a_usage_error() {
    let server = scripted_server(&[b"DELETED_BATCH 0 0\r\n"]);
    let out = Command::new(env!("CARGO_BIN_EXE_tuber-cli"))
        .args(["--addr", &server.addr, "delete-batch"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(server.transcript().is_empty(), "sent: {:?}", server.transcript());
}
