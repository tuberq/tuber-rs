//! Helpers shared by the tuber-cli integration tests.

// Each test crate compiles its own copy and uses only part of it.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

/// Stand-in server for a single command.
pub struct FakeServer {
    pub addr: String,
    /// The command line it was sent, without the trailing CRLF.
    pub received: mpsc::Receiver<String>,
}

/// Accept one connection, read the one command sent on it, answer `reply`.
pub fn fake_server(reply: &'static [u8]) -> FakeServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (tx, received) = mpsc::channel();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        (&stream).write_all(reply).unwrap();
        let _ = tx.send(line.trim_end().to_string());
    });
    FakeServer { addr, received }
}
