//! What can go wrong talking to tuber, as a type a caller can match on.

use crate::schedule::ScheduleSyncError;
use std::fmt;
use std::io;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Why a command failed.
///
/// Tuber's own refusals ([`NotFound`](Self::NotFound),
/// [`Draining`](Self::Draining), …) leave the connection usable. After
/// [`Io`](Self::Io), [`Timeout`](Self::Timeout) or
/// [`Protocol`](Self::Protocol) it is not: the client no longer knows where the
/// next reply starts, so it closes the connection, which also hands back any
/// job it held, and every later call is [`Closed`](Self::Closed). Connect again
/// to go on.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The connection failed: refused, reset, or closed by the server.
    Io(io::Error),
    /// No reply within the client's timeout. A reply may still be on its way,
    /// so the connection is closed rather than left to read it as the next
    /// command's.
    Timeout,
    /// A reply this client can't read: tuber and the client disagree about
    /// the protocol, or about where they are in it.
    Protocol(String),
    /// An earlier `Io`, `Timeout` or `Protocol` error closed this connection.
    Closed,
    /// Refused before anything was sent: a name, tag or argument tuber would
    /// refuse or misread.
    InvalidInput(String),

    /// `NOT_FOUND`: no such job, tube, group or schedule entry, or the job
    /// isn't in a state the command applies to (not reserved by this
    /// connection, say).
    NotFound,
    /// `DRAINING`: the server is refusing new puts.
    Draining,
    /// `INTERNAL_ERROR`. On a `put` this means the job was not queued, but its
    /// record may have reached disk, so a tuber restart can bring it back.
    /// Retry such a put with an `idp:` key, so that the retry and the
    /// resurrected job can't both run.
    InternalError,
    /// `OUT_OF_MEMORY`: the server is at its job-memory limit.
    OutOfMemory,
    /// `OUT_OF_STORAGE`: the server's disk is full.
    OutOfStorage,
    /// `JOB_TOO_BIG`: the body is over the server's size limit.
    JobTooBig,
    /// `BAD_FORMAT`: the server couldn't parse the command.
    BadFormat,
    /// `UNKNOWN_COMMAND`: the server doesn't have this command; an older
    /// tuber, or beanstalkd for a tuber extension.
    UnknownCommand,
    /// `EXPECTED_CRLF`: a put body wasn't followed by CRLF. Tuber closes the
    /// connection after it, so this one of its replies is fatal.
    ExpectedCrlf,
    /// `NOT_IGNORED`: `ignore` would have left the connection watching no
    /// tube.
    NotIgnored,
    /// `DEADLINE_SOON` to a `reserve-batch`: a job this connection holds is
    /// within a second of its TTR, so no batch was reserved. Touch or finish
    /// the jobs held (`touch_all`), then reserve again. A single reserve
    /// returns this as [`Reserved::DeadlineSoon`](crate::Reserved::DeadlineSoon)
    /// instead.
    DeadlineSoon,

    /// A schedule sync tuber refused; see [`ScheduleSyncError`].
    ScheduleSync(ScheduleSyncError),
}

impl Error {
    /// The error a one-word refusal from tuber stands for, if it is one.
    pub(crate) fn from_reply(line: &str) -> Option<Self> {
        Some(match line {
            "NOT_FOUND" => Self::NotFound,
            "DRAINING" => Self::Draining,
            "INTERNAL_ERROR" => Self::InternalError,
            "OUT_OF_MEMORY" => Self::OutOfMemory,
            "OUT_OF_STORAGE" => Self::OutOfStorage,
            "JOB_TOO_BIG" => Self::JobTooBig,
            "BAD_FORMAT" => Self::BadFormat,
            "UNKNOWN_COMMAND" => Self::UnknownCommand,
            "EXPECTED_CRLF" => Self::ExpectedCrlf,
            "NOT_IGNORED" => Self::NotIgnored,
            _ => return None,
        })
    }

    /// Tuber's word for this refusal, `NOT_FOUND` and so on; `None` for an
    /// error that isn't one of tuber's replies.
    pub fn reply(&self) -> Option<&'static str> {
        Some(match self {
            Self::NotFound => "NOT_FOUND",
            Self::Draining => "DRAINING",
            Self::InternalError => "INTERNAL_ERROR",
            Self::OutOfMemory => "OUT_OF_MEMORY",
            Self::OutOfStorage => "OUT_OF_STORAGE",
            Self::JobTooBig => "JOB_TOO_BIG",
            Self::BadFormat => "BAD_FORMAT",
            Self::UnknownCommand => "UNKNOWN_COMMAND",
            Self::ExpectedCrlf => "EXPECTED_CRLF",
            Self::NotIgnored => "NOT_IGNORED",
            Self::DeadlineSoon => "DEADLINE_SOON",
            _ => return None,
        })
    }

    /// True when this error closed the connection: reconnect to go on.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::Io(_) | Self::Timeout | Self::Protocol(_) | Self::Closed | Self::ExpectedCrlf
        )
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Timeout => f.write_str("timed out waiting for the server"),
            Self::Protocol(msg) => write!(f, "protocol error: {msg}"),
            Self::Closed => f.write_str("connection closed after an earlier error"),
            Self::InvalidInput(msg) => f.write_str(msg),
            Self::ScheduleSync(e) => write!(f, "{e}"),
            other => f.write_str(other.reply().unwrap_or("unknown error")),
        }
    }
}

/// `Io` and `ScheduleSync` print their inner error as their own message, so
/// they pass on its source rather than the error itself, which would say it
/// twice in a chain.
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => e.source(),
            Self::ScheduleSync(e) => e.source(),
            _ => None,
        }
    }
}

/// For callers working in `io::Result`.
impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        let kind = match e {
            Error::Io(e) => return e,
            Error::Timeout => io::ErrorKind::TimedOut,
            Error::InvalidInput(_) => io::ErrorKind::InvalidInput,
            Error::Protocol(_) => io::ErrorKind::InvalidData,
            Error::Closed => io::ErrorKind::NotConnected,
            _ => io::ErrorKind::Other,
        };
        io::Error::new(kind, e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as _;

    /// Each message in the chain, as anyhow's `{:#}` prints it.
    fn chain(err: &Error) -> Vec<String> {
        let mut messages = vec![err.to_string()];
        let mut source = err.source();
        while let Some(e) = source {
            messages.push(e.to_string());
            source = e.source();
        }
        messages
    }

    /// A wrapper that prints its inner error and also returns it as the
    /// source says everything twice: tuber-cli printed "Connection refused
    /// (os error 111): Connection refused (os error 111)".
    #[test]
    fn each_message_is_said_once() {
        let io = Error::Io(io::Error::new(io::ErrorKind::ConnectionRefused, "Connection refused"));
        assert_eq!(chain(&io), ["Connection refused"]);
        let sync = Error::ScheduleSync(ScheduleSyncError {
            owner: "splat".into(),
            entry: Some("rollup".into()),
            status: "BAD_SCHEDULE".into(),
        });
        assert_eq!(chain(&sync).len(), 1, "{:?}", chain(&sync));
    }
}
