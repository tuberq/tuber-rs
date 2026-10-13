//! Helpers shared by the tuber-cli integration tests.

// Each test crate compiles its own copy and uses only part of it.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
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

/// Stand-in server for a whole conversation on one connection: the n-th
/// command read is answered with `replies[n]`. A `put` or `schedule-put` has
/// its body read too, and reported after its line, so a test sees exactly
/// what was framed. The connection closes after the last reply.
pub fn scripted_server(replies: &[&'static [u8]]) -> FakeServer {
    let replies = replies.to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (tx, received) = mpsc::channel();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(&stream);
        for reply in replies {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                return;
            }
            let line = line.trim_end().to_string();
            // `<bytes>` is put's 5th word and schedule-put's 7th.
            let bytes_at = match line.split_whitespace().next() {
                Some("put") => Some(4),
                Some("schedule-put") => Some(6),
                _ => None,
            };
            let _ = tx.send(line.clone());
            if let Some(i) = bytes_at {
                let n: usize = line.split_whitespace().nth(i).unwrap().parse().unwrap();
                let mut body = vec![0u8; n + 2];
                reader.read_exact(&mut body).unwrap();
                body.truncate(n);
                let _ = tx.send(String::from_utf8(body).unwrap());
            }
            (&stream).write_all(reply).unwrap();
        }
    });
    FakeServer { addr, received }
}

impl FakeServer {
    /// Everything the server was sent, once the client has finished.
    pub fn transcript(&self) -> Vec<String> {
        self.received.try_iter().collect()
    }
}
