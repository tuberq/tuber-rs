use clap::{Parser, Subcommand};
use std::io::{Read, Write};
use tuber_lib::client::TuberClient;

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
        /// Job body (reads from stdin if not provided)
        body: Option<String>,

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
        /// the tube. Avoid colons in the key -- the last one is read as a TTL.
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

        /// Concurrency key: limit how many jobs with this key run at once
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
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
                Some(b) => b,
                None => {
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf
                }
            };
            let mut tags = Vec::new();
            if let Some(key) = idempotent {
                tags.push(match idempotent_ttl {
                    Some(ttl) => format!("idp:{key}:{ttl}"),
                    None => format!("idp:{key}"),
                });
            }
            if let Some(g) = group {
                tags.push(format!("grp:{g}"));
            }
            if let Some(a) = after {
                tags.push(format!("aft:{a}"));
            }
            if let Some(c) = concurrency {
                tags.push(format!("con:{c}"));
            }
            let res = client.put_tagged(priority, delay, ttr, data.as_bytes(), &tags).await?;
            match res.existing_state {
                // idp: key matched -- nothing was enqueued, id is the existing job.
                Some(state) => output(
                    &cli.format,
                    &serde_json::json!({ "id": res.id, "duplicate": true, "state": state }),
                )?,
                None => output(&cli.format, &serde_json::json!({ "id": res.id }))?,
            }
        }

        Command::Reserve { timeout, tube } => {
            if let Some(tube) = tube {
                client.watch_only(&tube).await?;
            }
            let (id, body) = client.reserve(timeout).await?;
            output(&cli.format, &serde_json::json!({ "id": id, "body": body }))?;
        }

        Command::ReserveBatch { count, timeout, tube } => {
            if let Some(tube) = tube {
                client.watch_only(&tube).await?;
            }
            let jobs = client.reserve_batch(count, timeout).await?;
            let jobs: Vec<_> = jobs
                .into_iter()
                .map(|(id, body)| serde_json::json!({ "id": id, "body": body }))
                .collect();
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
            let (id, body) = client.reserve_job(id).await?;
            output(&cli.format, &serde_json::json!({ "id": id, "body": body }))?;
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
            let (id, body) = client.peek(id).await?;
            output(&cli.format, &serde_json::json!({ "id": id, "body": body }))?;
        }

        Command::PeekReady { tube } => {
            client.use_tube(&tube).await?;
            let (id, body) = client.peek_ready().await?;
            output(&cli.format, &serde_json::json!({ "id": id, "body": body }))?;
        }

        Command::PeekBuried { tube } => {
            client.use_tube(&tube).await?;
            let (id, body) = client.peek_buried().await?;
            output(&cli.format, &serde_json::json!({ "id": id, "body": body }))?;
        }

        Command::PeekDelayed { tube } => {
            client.use_tube(&tube).await?;
            let (id, body) = client.peek_delayed().await?;
            output(&cli.format, &serde_json::json!({ "id": id, "body": body }))?;
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
                std::process::exit(1);
            }
        }

        Command::Pause { tube, delay } => {
            client.pause_tube(&tube, delay).await?;
            output(&cli.format, &serde_json::json!({ "status": "paused", "tube": tube, "delay": delay }))?;
        }
    }

    Ok(())
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
