use anyhow::Context;
use clap::{Parser, Subcommand};
use std::fmt;
use std::io::{Read, Write};
use std::process::ExitCode;
use tuber_lib::{tag, Job, Reserved, TuberClient};

mod schedule_file;

#[derive(Parser)]
#[command(name = "tuber-cli", version, about = "CLI for tuber/beanstalkd job queue")]
struct Cli {
    /// Server address [host][:port] [default: $TUBER_URL, $TUBER_ADDR,
    /// $BEANSTALKD_URL, then localhost:11300]
    #[arg(short, long, global = true, value_name = "ADDR")]
    addr: Option<String>,

    /// Output format
    #[arg(short, long, global = true, default_value = "json")]
    format: OutputFormat,

    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, clap::ValueEnum)]
enum OutputFormat {
    Json,
    Text,
}

#[derive(Subcommand)]
enum Command {
    /// Show server statistics
    Stats,

    /// List all tubes
    ListTubes,

    /// Show statistics for a specific tube
    StatsTube {
        /// Tube name
        name: String,
    },

    /// Put a job into a tube
    Put {
        /// Job body (reads from stdin if not provided). Bytes, as given: it
        /// need not be text.
        body: Option<std::ffi::OsString>,

        /// Tube to use (default: "default")
        #[arg(long, default_value = "default")]
        tube: String,

        /// Job priority (0 is highest)
        #[arg(long, default_value = "0")]
        priority: u32,

        /// Delay in seconds before job becomes ready
        #[arg(long, default_value = "0")]
        delay: u32,

        /// Time-to-run in seconds
        #[arg(long, default_value = "120")]
        ttr: u32,

        /// Idempotency key: drop the put if a live job with this key exists in
        /// the tube. Letters, digits and - + / ; . $ _ ( ): no colon, which
        /// tuber would read as the start of a TTL.
        #[arg(long)]
        idempotent: Option<String>,

        /// Seconds the --idempotent key stays reserved (default: job lifetime)
        #[arg(long, requires = "idempotent")]
        idempotent_ttl: Option<u32>,

        /// Add the job to a named group (for fan-out/fan-in)
        #[arg(long)]
        group: Option<String>,

        /// Hold the job until the named group completes
        #[arg(long)]
        after: Option<String>,

        /// Concurrency key: at most one job with this key runs at once, or N
        /// as key:N
        #[arg(long)]
        concurrency: Option<String>,
    },

    /// Reserve the next available job
    Reserve {
        /// Timeout in seconds (0 = return immediately if no job)
        #[arg(long, default_value = "0")]
        timeout: u32,

        /// Tube to watch
        #[arg(long)]
        tube: Option<String>,
    },

    /// Reserve up to <count> jobs in a single command
    ReserveBatch {
        /// Maximum number of jobs to reserve (1-1000)
        count: u32,

        /// Seconds to wait for at least one job (omit or 0 = return immediately)
        #[arg(long)]
        timeout: Option<u32>,

        /// Tube to watch
        #[arg(long)]
        tube: Option<String>,
    },

    /// Delete a job by ID
    Delete {
        /// Job ID
        id: u64,
    },

    /// Kick buried or delayed jobs
    Kick {
        /// Maximum number of jobs to kick
        #[arg(default_value = "1")]
        bound: u32,

        /// Tube to use
        #[arg(long, default_value = "default")]
        tube: String,
    },

    /// Kick one specific buried or delayed job back to ready
    KickJob {
        /// Job ID
        id: u64,
    },

    /// Reserve one specific job by ID, ignoring tube and watch list
    ReserveJob {
        /// Job ID
        id: u64,
    },

    /// Enter drain mode: reject new puts, let in-flight work finish
    Drain,

    /// Leave drain mode and resume accepting puts
    Undrain,

    /// Peek at a job by ID
    Peek {
        /// Job ID
        id: u64,
    },

    /// Peek at the next ready job in a tube.
    ///
    /// Stateless: --tube is required (no implicit "use" state from prior commands).
    PeekReady {
        /// Tube to use
        #[arg(long, default_value = "default")]
        tube: String,
    },

    /// Peek at the next buried job in a tube
    PeekBuried {
        /// Tube to use
        #[arg(long, default_value = "default")]
        tube: String,
    },

    /// Peek at the next delayed job in a tube
    PeekDelayed {
        /// Tube to use
        #[arg(long, default_value = "default")]
        tube: String,
    },

    /// Show statistics for a job
    StatsJob {
        /// Job ID
        id: u64,
    },

    /// Show statistics for a job group
    StatsGroup {
        /// Group name
        name: String,
    },

    /// Delete all jobs in a tube
    FlushTube {
        /// Tube name
        tube: String,
    },

    /// Delete all buried jobs in a tube (ready/delayed/reserved untouched)
    FlushBuried {
        /// Tube name
        tube: String,
    },

    /// Delete multiple jobs by ID
    DeleteBatch {
        /// Job IDs
        #[arg(required = true)]
        ids: Vec<u64>,
    },

    /// Pause a tube for a number of seconds
    Pause {
        /// Tube name
        tube: String,

        /// Pause duration in seconds
        #[arg(long)]
        delay: u32,
    },

    /// List recurring schedule entries, as owner and name
    ListSchedules {
        /// Only this owner's entries
        #[arg(long)]
        owner: Option<String>,
    },

    /// Show one schedule entry: its spec, next fires, last result and counters
    StatsSchedule {
        /// The app that owns the entry
        owner: String,

        /// Entry name
        name: String,
    },

    /// Replace an owner's whole recurring schedule with the entries in a file.
    ///
    /// The file is a JSON array of entries (see README). On success the
    /// owner's entries are exactly those: any it had that the file leaves
    /// out are deleted, so always sync the complete set. If tuber refuses
    /// one entry, nothing changes and the previous schedule keeps running.
    ScheduleSync {
        /// The app's name for its set of entries
        owner: String,

        /// JSON file of entries, or - for stdin
        file: String,

        /// Allow an empty file, which deletes every entry the owner has
        #[arg(long)]
        allow_empty: bool,
    },
}

/// Exit codes, one per kind of outcome, so a script can tell "no such job"
/// from "no server". Clap's own usage errors exit 2 as well.
mod exit {
    pub const OTHER: u8 = 1;
    /// Bad arguments, refused before anything was sent.
    pub const USAGE: u8 = 2;
    /// `NOT_FOUND`, or a `delete-batch` id that was.
    pub const NOT_FOUND: u8 = 3;
    /// A reserve that got no job: `TIMED_OUT` or `DEADLINE_SOON`.
    pub const NO_JOB: u8 = 4;
    /// Tuber refused the command: `DRAINING`, `OUT_OF_MEMORY`, a schedule
    /// entry, …
    pub const REFUSED: u8 = 5;
    /// No server, or it stopped answering or made no sense.
    pub const UNREACHABLE: u8 = 6;
}

/// A failure the CLI itself decides on, rather than one the client reports.
#[derive(Debug)]
enum Failure {
    /// A reserve that got no job, with tuber's reply.
    NoJob(&'static str),
    /// Arguments that can't be acted on.
    Usage(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::NoJob(reply) => f.write_str(reply),
            Failure::Usage(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for Failure {}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let format = cli.format.clone();
    match run(cli).await {
        Ok(code) => code,
        Err(err) => report(&format, &err),
    }
}

/// Report `err` on stderr, as JSON or a line of text, and return its exit
/// code. Never a backtrace: the message is the whole story.
fn report(format: &OutputFormat, err: &anyhow::Error) -> ExitCode {
    let (error, code) = classify(err);
    let message = format!("{err:#}");
    let line = match format {
        OutputFormat::Json => serde_json::json!({ "error": error, "message": message }).to_string(),
        OutputFormat::Text => format!("Error: {message}"),
    };
    // Nothing more to do if stderr is gone too.
    let _ = writeln!(std::io::stderr().lock(), "{line}");
    ExitCode::from(code)
}

/// The `error` word a failure reports, and its exit code: tuber's own reply
/// where there is one, so it reads as it does in tuber's docs.
fn classify(err: &anyhow::Error) -> (String, u8) {
    use tuber_lib::Error as E;
    if let Some(failure) = err.downcast_ref::<Failure>() {
        return match failure {
            Failure::NoJob(reply) => (reply.to_string(), exit::NO_JOB),
            Failure::Usage(_) => ("INVALID_INPUT".into(), exit::USAGE),
        };
    }
    let Some(err) = err.downcast_ref::<E>() else {
        return ("ERROR".into(), exit::OTHER);
    };
    match err {
        E::NotFound => ("NOT_FOUND".into(), exit::NOT_FOUND),
        E::DeadlineSoon => ("DEADLINE_SOON".into(), exit::NO_JOB),
        E::InvalidInput(_) => ("INVALID_INPUT".into(), exit::USAGE),
        E::Io(_) => ("UNREACHABLE".into(), exit::UNREACHABLE),
        E::Timeout => ("NO_REPLY".into(), exit::UNREACHABLE),
        E::Protocol(_) | E::Closed => ("PROTOCOL_ERROR".into(), exit::UNREACHABLE),
        E::ScheduleSync(e) => (e.status.clone(), exit::REFUSED),
        other => match other.reply() {
            Some(reply) => (reply.to_string(), exit::REFUSED),
            None => ("ERROR".into(), exit::OTHER),
        },
    }
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let addr = tuber_lib::server_addr(cli.addr.as_deref());
    let mut client = TuberClient::connect(&addr).await?;

    match cli.command {
        Command::Stats => {
            let stats = client.stats().await?;
            output(&cli.format, &stats)?;
        }

        Command::ListTubes => {
            let tubes = client.list_tubes().await?;
            output(&cli.format, &tubes)?;
        }

        Command::StatsTube { name } => {
            let stats = client.stats_tube(&name).await?;
            output(&cli.format, &stats)?;
        }

        Command::Put {
            body,
            tube,
            priority,
            delay,
            ttr,
            idempotent,
            idempotent_ttl,
            group,
            after,
            concurrency,
        } => {
            client.use_tube(&tube).await?;
            let data = match body {
                // On Unix these are the argument's own bytes.
                Some(b) => b.into_encoded_bytes(),
                None => {
                    let mut buf = Vec::new();
                    std::io::stdin().read_to_end(&mut buf)?;
                    buf
                }
            };
            let mut tags = Vec::new();
            if let Some(key) = idempotent {
                tags.push(match idempotent_ttl {
                    Some(ttl) => tag::idp_for(&key, ttl)?,
                    None => tag::idp(&key)?,
                });
            }
            if let Some(g) = group {
                tags.push(tag::group(&g)?);
            }
            if let Some(a) = after {
                tags.push(tag::after(&a)?);
            }
            if let Some(c) = concurrency {
                tags.push(match c.rsplit_once(':') {
                    Some((key, limit)) => {
                        let limit = limit.parse().map_err(|_| {
                            Failure::Usage(format!("--concurrency {c}: the limit after the colon must be a number"))
                        })?;
                        tag::concurrency_limit(key, limit)?
                    }
                    None => tag::concurrency(&c)?,
                });
            }
            let res = client.put_tagged(priority, delay, ttr, &data, &tags).await?;
            // A dedup against a buried job is still not a failure, but nothing
            // was queued and nothing runs until that job is kicked. Run from
            // cron, a put like that drops its work every time, and only stderr
            // gets mailed.
            if res.existing_state.as_deref() == Some("BURIED") {
                eprintln!(
                    "warning: idempotency key matched buried job {}; nothing was \
                     queued, and nothing will run until it is kicked",
                    res.id
                );
            }
            let mut json = serde_json::json!({ "id": res.id });
            // idp: key matched -- nothing was enqueued, id is the existing job.
            if let Some(state) = res.existing_state {
                json["duplicate"] = true.into();
                json["state"] = state.into();
            }
            if let Some(pri) = res.raised_pri {
                json["raised_pri"] = pri.into();
            }
            // beanstalkd out of memory: queued, but buried until kicked.
            if res.buried {
                json["buried"] = true.into();
            }
            output(&cli.format, &json)?;
        }

        Command::Reserve { timeout, tube } => {
            if let Some(tube) = tube {
                client.watch_only(&tube).await?;
            }
            match client.reserve_with_timeout(timeout).await? {
                Reserved::Job(job) => output(&cli.format, &job_json(&job))?,
                Reserved::TimedOut => return Err(Failure::NoJob("TIMED_OUT").into()),
                Reserved::DeadlineSoon => return Err(Failure::NoJob("DEADLINE_SOON").into()),
            }
        }

        Command::ReserveBatch { count, timeout, tube } => {
            if let Some(tube) = tube {
                client.watch_only(&tube).await?;
            }
            let jobs = client.reserve_batch(count, timeout).await?;
            let jobs: Vec<_> = jobs.iter().map(job_json).collect();
            output(&cli.format, &serde_json::json!({ "reserved": jobs.len(), "jobs": jobs }))?;
        }

        Command::Delete { id } => {
            client.delete(id).await?;
            output(&cli.format, &serde_json::json!({ "status": "deleted", "id": id }))?;
        }

        Command::Kick { bound, tube } => {
            client.use_tube(&tube).await?;
            let count = client.kick(bound).await?;
            output(&cli.format, &serde_json::json!({ "kicked": count }))?;
        }

        Command::KickJob { id } => {
            client.kick_job(id).await?;
            output(&cli.format, &serde_json::json!({ "status": "kicked", "id": id }))?;
        }

        Command::ReserveJob { id } => {
            let job = client.reserve_job(id).await?;
            output(&cli.format, &job_json(&job))?;
        }

        Command::Drain => {
            client.drain().await?;
            output(&cli.format, &serde_json::json!({ "status": "draining" }))?;
        }

        Command::Undrain => {
            client.undrain().await?;
            output(&cli.format, &serde_json::json!({ "status": "not_draining" }))?;
        }

        Command::Peek { id } => {
            let job = client.peek(id).await?;
            output(&cli.format, &job_json(&job))?;
        }

        Command::PeekReady { tube } => {
            client.use_tube(&tube).await?;
            let job = client.peek_ready().await?;
            output(&cli.format, &job_json(&job))?;
        }

        Command::PeekBuried { tube } => {
            client.use_tube(&tube).await?;
            let job = client.peek_buried().await?;
            output(&cli.format, &job_json(&job))?;
        }

        Command::PeekDelayed { tube } => {
            client.use_tube(&tube).await?;
            let job = client.peek_delayed().await?;
            output(&cli.format, &job_json(&job))?;
        }

        Command::StatsJob { id } => {
            let stats = client.stats_job(id).await?;
            output(&cli.format, &stats)?;
        }

        Command::StatsGroup { name } => {
            let stats = client.stats_group(&name).await?;
            output(&cli.format, &stats)?;
        }

        Command::FlushTube { tube } => {
            let count = client.flush_tube(&tube).await?;
            output(&cli.format, &serde_json::json!({ "flushed": count, "tube": tube }))?;
        }

        Command::FlushBuried { tube } => {
            let count = client.flush_buried(&tube).await?;
            output(&cli.format, &serde_json::json!({ "flushed": count, "tube": tube }))?;
        }

        Command::DeleteBatch { ids } => {
            let (deleted, not_found) = client.delete_batch(&ids).await?;
            output(&cli.format, &serde_json::json!({ "deleted": deleted, "not_found": not_found }))?;
            if not_found > 0 {
                return Ok(ExitCode::from(exit::NOT_FOUND));
            }
        }

        Command::Pause { tube, delay } => {
            client.pause_tube(&tube, delay).await?;
            output(&cli.format, &serde_json::json!({ "status": "paused", "tube": tube, "delay": delay }))?;
        }

        Command::ListSchedules { owner } => {
            let entries = client.list_schedules(owner.as_deref()).await?;
            let entries: Vec<_> = entries
                .into_iter()
                .map(|(owner, name)| serde_json::json!({ "owner": owner, "name": name }))
                .collect();
            output(&cli.format, &entries)?;
        }

        Command::StatsSchedule { owner, name } => {
            let stats = client.stats_schedule(&owner, &name).await?;
            output(&cli.format, &stats)?;
        }

        Command::ScheduleSync { owner, file, allow_empty } => {
            let json = if file == "-" {
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf)?;
                buf
            } else {
                std::fs::read_to_string(&file).with_context(|| format!("reading {file}"))?
            };
            let entries = schedule_file::parse(&json)?;
            if entries.is_empty() && !allow_empty {
                let source = if file == "-" { "stdin" } else { file.as_str() };
                return Err(Failure::Usage(format!(
                    "{source} has no entries, and syncing none removes all of {owner}'s \
                     schedules; pass --allow-empty if that's meant"
                ))
                .into());
            }
            let result = client.sync_schedules(&owner, &entries, allow_empty).await?;
            output(
                &cli.format,
                &serde_json::json!({
                    "owner": owner,
                    "added": result.added,
                    "changed": result.changed,
                    "removed": result.removed,
                    "unchanged": result.unchanged,
                }),
            )?;
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// A job as JSON: its body as a string when it is UTF-8, and otherwise as
/// `body_base64`, since a JSON string can't hold arbitrary bytes.
fn job_json(job: &Job) -> serde_json::Value {
    match job.body_str() {
        Some(body) => serde_json::json!({ "id": job.id, "body": body }),
        None => serde_json::json!({ "id": job.id, "body_base64": base64(&job.body) }),
    }
}

/// Standard base64, padded (RFC 4648 §4).
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn output(format: &OutputFormat, value: &impl serde::Serialize) -> anyhow::Result<()> {
    let mut out = std::io::stdout().lock();
    let written = match format {
        OutputFormat::Json => writeln!(out, "{}", serde_json::to_string_pretty(value)?),
        OutputFormat::Text => {
            // Render as compact JSON for text mode — structured types get pretty-printed
            let v = serde_json::to_value(value)?;
            print_text(&mut out, &v, 0)
        }
    }
    .and_then(|()| out.flush());
    match written {
        // The reader went away (`tuber-cli … | head`). The command itself has
        // already run, so this ends the output, not the command: callers like
        // delete-batch still choose the exit status.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => Ok(other?),
    }
}

fn print_text(
    out: &mut impl Write,
    value: &serde_json::Value,
    indent: usize,
) -> std::io::Result<()> {
    let pad = " ".repeat(indent);
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                match v {
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                        writeln!(out, "{pad}{k}:")?;
                        print_text(out, v, indent + 2)?;
                    }
                    _ => writeln!(out, "{pad}{k}: {}", format_value(v))?,
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                match item {
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                        writeln!(out, "{pad}-")?;
                        print_text(out, item, indent + 2)?;
                    }
                    _ => writeln!(out, "{pad}- {}", format_value(item))?,
                }
            }
        }
        _ => writeln!(out, "{pad}{}", format_value(value))?,
    }
    Ok(())
}

fn format_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_matches_rfc_4648() {
        for (raw, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(raw.as_bytes()), encoded, "{raw:?}");
        }
        assert_eq!(base64(b"\xff\x00a"), "/wBh");
    }
}
