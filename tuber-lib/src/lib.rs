//! An async client for [tuber](https://github.com/tuberq/tuber), the
//! beanstalkd-compatible work queue, and its extensions: idempotent puts,
//! groups, concurrency limits and recurring schedules.
//!
//! ```no_run
//! use tuber_lib::{tag, Reserved, TuberClient};
//!
//! # async fn handle(_body: &[u8]) -> Result<(), ()> { Ok(()) }
//! # async fn run() -> tuber_lib::Result<()> {
//! let mut client = TuberClient::connect("localhost:11300").await?;
//!
//! // Produce
//! client.use_tube("emails").await?;
//! let tags = [tag::idp("welcome-42")?, tag::concurrency_limit("user-42", 3)?];
//! let put = client.put_tagged(1024, 0, 120, b"send welcome", &tags).await?;
//!
//! // Consume
//! client.watch_only("emails").await?;
//! loop {
//!     match client.reserve_with_timeout(5).await? {
//!         Reserved::Job(job) => match handle(&job.body).await {
//!             Ok(()) => client.delete(job.id).await?,
//!             Err(_) => client.release(job.id, 1024, 30).await?,
//!         },
//!         Reserved::TimedOut => continue,
//!         // A job this connection holds is about to run out of TTR.
//!         Reserved::DeadlineSoon => _ = client.touch_all().await?,
//!     }
//! }
//! # }
//! ```
//!
//! Errors are [`Error`]s. Tuber's refusals leave the connection usable; an
//! I/O error, a timeout or a reply the client can't read closes it (see
//! [`Error::is_fatal`]).

pub mod client;
mod error;
pub mod model;
mod name;
pub mod parse;
pub mod schedule;

pub use client::{Job, PutResult, ReserveMode, Reserved, TuberClient};
pub use name::tag;
pub use error::{Error, Result};

const DEFAULT_HOST: &str = "localhost";
const DEFAULT_PORT: &str = "11300";

/// Normalize a host:port input string.
///
/// - `None` or empty -> `"localhost:11300"`
/// - `":1234"` -> `"localhost:1234"`
/// - `"myhost"` -> `"myhost:11300"`
/// - `"myhost:1234"` -> `"myhost:1234"`
pub fn resolve_addr(input: Option<&str>) -> String {
    let input = input.unwrap_or("").trim();
    if input.is_empty() {
        return format!("{DEFAULT_HOST}:{DEFAULT_PORT}");
    }
    if input.starts_with(':') {
        return format!("{DEFAULT_HOST}{input}");
    }
    if input.contains(':') {
        return input.to_string();
    }
    format!("{input}:{DEFAULT_PORT}")
}

/// Env vars that name the server, most preferred first. `TUBER_URL` is what
/// the Ruby gem reads, `TUBER_ADDR` what tuber-cli/tuber-tui read before they
/// adopted `TUBER_URL`, and `BEANSTALKD_URL` the beanstalkd convention.
/// Despite the names, each holds a bare `[host][:port]`, not a URL with a
/// scheme.
pub const ADDR_ENV_VARS: [&str; 3] = ["TUBER_URL", "TUBER_ADDR", "BEANSTALKD_URL"];

/// The server to dial: `flag` if given, else the first non-empty var in
/// [`ADDR_ENV_VARS`], normalized by [`resolve_addr`].
pub fn server_addr(flag: Option<&str>) -> String {
    server_addr_with(flag, |var| std::env::var(var).ok())
}

fn server_addr_with(flag: Option<&str>, env: impl Fn(&str) -> Option<String>) -> String {
    let from_env = ADDR_ENV_VARS
        .iter()
        .find_map(|var| env(var).filter(|v| !v.trim().is_empty()));
    resolve_addr(flag.or(from_env.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::server_addr_with;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn server_addr_prefers_flag_then_env_in_order() {
        let all = env(&[
            ("TUBER_URL", "url:1"),
            ("TUBER_ADDR", "addr:2"),
            ("BEANSTALKD_URL", "bs:3"),
        ]);
        assert_eq!(server_addr_with(Some("flag:0"), &all), "flag:0");
        assert_eq!(server_addr_with(None, &all), "url:1");
        let no_url = env(&[("TUBER_ADDR", "addr:2"), ("BEANSTALKD_URL", "bs:3")]);
        assert_eq!(server_addr_with(None, &no_url), "addr:2");
        let only_bs = env(&[("BEANSTALKD_URL", "bs:3")]);
        assert_eq!(server_addr_with(None, &only_bs), "bs:3");
        let none = env(&[]);
        assert_eq!(server_addr_with(None, &none), "localhost:11300");
    }

    #[test]
    fn server_addr_skips_blank_vars_and_normalizes_what_it_finds() {
        let blank_url = env(&[("TUBER_URL", "  "), ("TUBER_ADDR", ":1234")]);
        assert_eq!(server_addr_with(None, &blank_url), "localhost:1234");
        let host_only = env(&[("TUBER_URL", "q.internal")]);
        assert_eq!(server_addr_with(None, &host_only), "q.internal:11300");
    }
}
