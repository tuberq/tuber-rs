//! Job bodies are bytes. JSON can't carry bytes that aren't UTF-8 in a
//! string, so those come out base64-encoded, under their own key.

mod common;

use common::scripted_server;
use std::process::{Command, Output};

fn run(replies: &[&'static [u8]], args: &[&str]) -> Output {
    let server = scripted_server(replies);
    Command::new(env!("CARGO_BIN_EXE_tuber-cli"))
        .args(["--addr", &server.addr])
        .args(args)
        .output()
        .unwrap()
}

fn json(out: &Output) -> serde_json::Value {
    assert!(out.status.success(), "{:?}: {}", out.status, String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn a_text_body_is_a_string() {
    let out = run(&[b"RESERVED 7 5\r\nhello\r\n"], &["reserve"]);
    assert_eq!(json(&out), serde_json::json!({ "id": 7, "body": "hello" }));
}

#[test]
fn a_binary_body_is_base64() {
    let out = run(&[b"RESERVED 7 3\r\n\xff\x00a\r\n"], &["reserve"]);
    assert_eq!(json(&out), serde_json::json!({ "id": 7, "body_base64": "/wBh" }));
}

#[test]
fn a_batch_with_a_binary_body_keeps_every_job() {
    let out = run(
        &[b"RESERVED_BATCH 2\r\nRESERVED 1 1\r\n\xff\r\nRESERVED 2 3\r\nok1\r\n"],
        &["reserve-batch", "2"],
    );
    assert_eq!(
        json(&out),
        serde_json::json!({
            "reserved": 2,
            "jobs": [{ "id": 1, "body_base64": "/w==" }, { "id": 2, "body": "ok1" }],
        })
    );
}

/// Run `put` with `body` on stdin, and return what the server was sent.
fn put_from_stdin(body: &[u8]) -> (Output, Vec<String>) {
    use std::io::Write;
    use std::process::Stdio;
    let server = scripted_server(&[b"USING default\r\n", b"INSERTED 1\r\n"]);
    let mut child = Command::new(env!("CARGO_BIN_EXE_tuber-cli"))
        .args(["--addr", &server.addr, "put"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(body).unwrap();
    let out = child.wait_with_output().unwrap();
    (out, server.transcript())
}

/// `tuber-cli put < image.png` has to send the file's bytes, not refuse them
/// for not being text.
#[test]
fn a_binary_body_on_stdin_is_put_as_is() {
    let (out, sent) = put_from_stdin(b"\xff\x00a");
    json(&out);
    assert_eq!(sent, ["use default", "put 0 0 120 3", r"\xff\x00a"]);
}

#[cfg(unix)]
#[test]
fn a_binary_body_argument_is_put_as_is() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let server = scripted_server(&[b"USING default\r\n", b"INSERTED 1\r\n"]);
    let out = Command::new(env!("CARGO_BIN_EXE_tuber-cli"))
        .args(["--addr", &server.addr, "put"])
        // Not a NUL here: argv can't hold one.
        .arg(OsStr::from_bytes(b"\xffa"))
        .output()
        .unwrap();
    json(&out);
    assert_eq!(server.transcript(), ["use default", "put 0 0 120 2", r"\xffa"]);
}
