//! Where tuber-cli finds the server: `--addr`, then `TUBER_URL` (the name the
//! Ruby gem reads), `TUBER_ADDR`, `BEANSTALKD_URL`, then localhost:11300.

mod common;

use common::fake_server;
use std::process::Command;
use std::time::Duration;

/// Address env vars, cleared from every run so tests are hermetic regardless
/// of the environment `cargo test` inherits.
const ADDR_ENVS: &[&str] = &["TUBER_URL", "TUBER_ADDR", "BEANSTALKD_URL"];

/// Nothing listens here: a client that dials it fails to connect.
const DEAD_ADDR: &str = "127.0.0.1:1";

/// Run `tuber-cli <args> delete 1` with `envs` (where the value `"SERVER"`
/// stands for a fake server's address) as the only address env vars, and
/// assert the command reached that server.
fn assert_reaches_server(args: &[&str], envs: &[(&str, &str)]) {
    let server = fake_server(b"DELETED\r\n");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tuber-cli"));
    for e in ADDR_ENVS {
        cmd.env_remove(e);
    }
    for (k, v) in envs {
        cmd.env(k, if *v == "SERVER" { &server.addr } else { *v });
    }
    let args: Vec<&str> = args
        .iter()
        .map(|a| {
            if *a == "SERVER" {
                server.addr.as_str()
            } else {
                a
            }
        })
        .collect();
    let out = cmd.args(&args).args(["delete", "1"]).output().unwrap();

    assert!(
        out.status.success(),
        "{args:?} {envs:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        server
            .received
            .recv_timeout(Duration::from_secs(2))
            .as_deref(),
        Ok("delete 1"),
        "{args:?} {envs:?}: the command never reached the server"
    );
}

#[test]
fn reads_tuber_url() {
    assert_reaches_server(&[], &[("TUBER_URL", "SERVER")]);
}

#[test]
fn tuber_url_beats_tuber_addr() {
    assert_reaches_server(&[], &[("TUBER_URL", "SERVER"), ("TUBER_ADDR", DEAD_ADDR)]);
}

#[test]
fn tuber_addr_still_works_on_its_own() {
    assert_reaches_server(&[], &[("TUBER_ADDR", "SERVER")]);
}

#[test]
fn tuber_addr_beats_beanstalkd_url() {
    assert_reaches_server(
        &[],
        &[("TUBER_ADDR", "SERVER"), ("BEANSTALKD_URL", DEAD_ADDR)],
    );
}

#[test]
fn falls_back_to_beanstalkd_url() {
    assert_reaches_server(&[], &[("BEANSTALKD_URL", "SERVER")]);
}

#[test]
fn an_empty_tuber_url_falls_through() {
    assert_reaches_server(&[], &[("TUBER_URL", ""), ("BEANSTALKD_URL", "SERVER")]);
}

#[test]
fn addr_flag_beats_every_env_var() {
    assert_reaches_server(
        &["--addr", "SERVER"],
        &[
            ("TUBER_URL", DEAD_ADDR),
            ("TUBER_ADDR", DEAD_ADDR),
            ("BEANSTALKD_URL", DEAD_ADDR),
        ],
    );
}
