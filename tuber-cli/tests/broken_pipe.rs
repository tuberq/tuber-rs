//! `tuber-cli … | head` closes stdout before the CLI has finished writing.
//! That must end the output quietly, not panic.

mod common;

use common::fake_server;
use std::process::{Command, Output, Stdio};

/// Run tuber-cli with stdout on a pipe whose read end is already closed, so
/// its first write fails with EPIPE -- what `| head` does, minus the race.
fn run_with_closed_stdout(reply: &'static [u8], args: &[&str]) -> Output {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    Command::new(env!("CARGO_BIN_EXE_tuber-cli"))
        .arg("--addr")
        .arg(fake_server(reply).addr)
        .args(args)
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

fn assert_no_panic(out: &Output) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "tuber-cli panicked: {stderr}");
}

#[test]
fn json_output_into_a_closed_pipe_exits_cleanly() {
    let out = run_with_closed_stdout(b"DELETED\r\n", &["delete", "1"]);
    assert_no_panic(&out);
    assert!(out.status.success(), "status: {:?}", out.status);
}

#[test]
fn text_output_into_a_closed_pipe_exits_cleanly() {
    let out = run_with_closed_stdout(b"DELETED\r\n", &["--format", "text", "delete", "1"]);
    assert_no_panic(&out);
    assert!(out.status.success(), "status: {:?}", out.status);
}

#[test]
fn a_closed_pipe_keeps_the_commands_own_exit_status() {
    // delete-batch exits 3, as NOT_FOUND does, when any id was not found;
    // losing the reader of its output must not turn that into a success.
    let out = run_with_closed_stdout(b"DELETED_BATCH 1 1\r\n", &["delete-batch", "1", "2"]);
    assert_no_panic(&out);
    assert_eq!(out.status.code(), Some(3), "status: {:?}", out.status);
}
