use clap::{Parser, Subcommand};
use std::io::Read;
use tuber_lib::client::TuberClient;

#[derive(Parser)]
#[command(name = "tuber-cli", version, about = "CLI for tuber/beanstalkd job queue")]
struct Cli {
    /// Server address [host][:port] (default: localhost:11300)
    #[arg(short, long, global = true, value_name = "ADDR", env = "TUBER_ADDR")]
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

    /// Peek at a job by ID
    Peek {
        /// Job ID
        id: u64,
    },

    /// Peek at the next ready job in a tube
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

    /// Delete multiple jobs by ID
    DeleteBatch {
        /// Job IDs
        ids: Vec<u64>,
    },

    /// Bury a reserved job
    Bury {
        /// Job ID
        id: u64,

        /// Priority for the buried job
        #[arg(long, default_value = "0")]
        priority: u32,
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
    let addr = tuber_lib::resolve_addr(cli.addr.as_deref());
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

        Command::Put { body, tube, priority, delay, ttr } => {
            if tube != "default" {
                client.use_tube(&tube).await?;
            }
            let data = match body {
                Some(b) => b,
                None => {
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf
                }
            };
            let id = client.put(priority, delay, ttr, data.as_bytes()).await?;
            output(&cli.format, &serde_json::json!({ "id": id }))?;
        }

        Command::Reserve { timeout, tube } => {
            if let Some(tube) = tube {
                client.use_tube(&tube).await?;
            }
            let (id, body) = client.reserve(timeout).await?;
            output(&cli.format, &serde_json::json!({ "id": id, "body": body }))?;
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

        Command::Peek { id } => {
            let (id, body) = client.peek(id).await?;
            output(&cli.format, &serde_json::json!({ "id": id, "body": body }))?;
        }

        Command::PeekReady { tube } => {
            if tube != "default" {
                client.use_tube(&tube).await?;
            }
            let (id, body) = client.peek_ready().await?;
            output(&cli.format, &serde_json::json!({ "id": id, "body": body }))?;
        }

        Command::PeekBuried { tube } => {
            if tube != "default" {
                client.use_tube(&tube).await?;
            }
            let (id, body) = client.peek_buried().await?;
            output(&cli.format, &serde_json::json!({ "id": id, "body": body }))?;
        }

        Command::PeekDelayed { tube } => {
            if tube != "default" {
                client.use_tube(&tube).await?;
            }
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

        Command::DeleteBatch { ids } => {
            let mut deleted = Vec::new();
            let mut errors = Vec::new();
            for id in &ids {
                match client.delete(*id).await {
                    Ok(()) => deleted.push(*id),
                    Err(e) => errors.push(serde_json::json!({ "id": id, "error": e.to_string() })),
                }
            }
            output(&cli.format, &serde_json::json!({ "deleted": deleted, "errors": errors }))?;
            if !errors.is_empty() {
                std::process::exit(1);
            }
        }

        Command::Bury { id, priority } => {
            client.bury(id, priority).await?;
            output(&cli.format, &serde_json::json!({ "status": "buried", "id": id }))?;
        }

        Command::Pause { tube, delay } => {
            client.pause_tube(&tube, delay).await?;
            output(&cli.format, &serde_json::json!({ "status": "paused", "tube": tube, "delay": delay }))?;
        }
    }

    Ok(())
}

fn output(format: &OutputFormat, value: &impl serde::Serialize) -> anyhow::Result<()> {
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(value)?);
        }
        OutputFormat::Text => {
            // Render as compact JSON for text mode — structured types get pretty-printed
            let v = serde_json::to_value(value)?;
            print_text(&v, 0);
        }
    }
    Ok(())
}

fn print_text(value: &serde_json::Value, indent: usize) {
    let pad = " ".repeat(indent);
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                match v {
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                        println!("{pad}{k}:");
                        print_text(v, indent + 2);
                    }
                    _ => println!("{pad}{k}: {}", format_value(v)),
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                match item {
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                        println!("{pad}-");
                        print_text(item, indent + 2);
                    }
                    _ => println!("{pad}- {}", format_value(item)),
                }
            }
        }
        _ => println!("{pad}{}", format_value(value)),
    }
}

fn format_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}
