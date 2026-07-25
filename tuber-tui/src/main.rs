mod app;
mod ui;

use app::{format_secs, kick_target, parse_count, parse_duration, plural, App, Mode, PromptKind};
use clap::Parser;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use futures::StreamExt;
use std::io;
use std::io::stdout;
use std::time::Duration;
use tokio::sync::mpsc;
use tuber_lib::client::TuberClient;
use tuber_lib::model::Snapshot;

#[derive(Parser)]
#[command(name = "tuber-tui", version, about = "TUI dashboard for tuber job queue")]
struct Cli {
    /// Server address [host][:port] (default: localhost:11300)
    #[arg(value_name = "HOST", env = "TUBER_ADDR")]
    addr: Option<String>,

    /// Poll interval in seconds
    #[arg(short, long, default_value = "1.5")]
    interval: f64,
}

enum PollMsg {
    Snapshot(Box<Snapshot>),
    Error(String),
    Status { msg: String, error: bool },
}

/// A server command a keypress asked for.
enum Command {
    Pause { tube: String, delay: u32 },
    Kick { tube: String, bound: u32 },
}

impl Command {
    /// The tube the command acts on, for error reporting.
    fn tube(&self) -> &str {
        match self {
            Command::Pause { tube, .. } | Command::Kick { tube, .. } => tube,
        }
    }
}

/// What a keypress asked for. Commands are returned rather than issued inline
/// so key handling stays synchronous and testable.
enum Action {
    None,
    Quit,
    Run(Command),
}

fn handle_key(app: &mut App, key: KeyEvent) -> Action {
    // The prompt swallows everything, including `q` — otherwise typing a
    // duration could quit the app.
    if matches!(app.mode, Mode::Prompt { .. }) {
        return handle_prompt_key(&mut app.mode, key);
    }

    match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Up | KeyCode::Char('k') => {
            app.move_selection(-1);
            Action::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.move_selection(1);
            Action::None
        }
        KeyCode::Char('s') => {
            app.cycle_sort();
            Action::None
        }
        KeyCode::Char('e') => {
            app.toggle_hide_empty();
            Action::None
        }
        KeyCode::Char('p') => {
            open_prompt(app, PromptKind::Pause);
            Action::None
        }
        // Shift-K: lowercase `k` is the vim-style cursor movement above.
        KeyCode::Char('K') => {
            open_prompt(app, PromptKind::Kick);
            Action::None
        }
        KeyCode::Char('u') => {
            let target = app.selected_tube().map(|t| (t.name.clone(), t.is_paused()));
            match target {
                Some((tube, true)) => Action::Run(Command::Pause { tube, delay: 0 }),
                Some((tube, false)) => {
                    app.set_status(format!("{tube} is not paused"), false);
                    Action::None
                }
                None => {
                    // Reachable by resting the cursor on the totals row, where
                    // silence would read as a broken key rather than as a
                    // command with nothing to act on.
                    app.set_status(no_tube_reason(app), true);
                    Action::None
                }
            }
        }
        _ => Action::None,
    }
}

/// Why a tube command has nothing to act on. The server's totals row takes the
/// cursor like a tube, so this has to name that case rather than claim nothing
/// is selected while something visibly is.
fn no_tube_reason(app: &App) -> String {
    if app.server_selected() {
        "The server row is selected \u{2014} move to a tube first".to_string()
    } else {
        "No tube selected".to_string()
    }
}

/// Open a prompt against the selected tube, refusing early when there is
/// nothing for it to do — a kick prompt on a tube with no buried or delayed
/// jobs can only ever report "kicked 0".
fn open_prompt(app: &mut App, kind: PromptKind) {
    let Some(tube) = app.selected_tube() else {
        app.set_status(no_tube_reason(app), true);
        return;
    };
    let (available, _) = kick_target(tube.current_jobs_buried, tube.current_jobs_delayed);
    let tube = tube.name.clone();

    // One match owns what each kind does, so the refusal and the prefill — both
    // of them facts about Kick — are read together rather than ten lines apart.
    // The kick bound is prefilled with everything one kick can move, so the
    // common case, clearing the whole backlog, is Shift-K then Enter.
    let buf = match kind {
        PromptKind::Pause => String::new(),
        PromptKind::Kick if available == 0 => {
            app.set_status(format!("{tube} has no buried or delayed jobs"), false);
            return;
        }
        PromptKind::Kick => available.to_string(),
    };
    app.mode = Mode::Prompt { kind, tube, buf };
}

/// Takes the mode by reference and destructures once, so the buffer being read
/// on Enter is the same one the character keys wrote to.
fn handle_prompt_key(mode: &mut Mode, key: KeyEvent) -> Action {
    let Mode::Prompt { kind, tube, buf } = mode else {
        return Action::None;
    };
    match key.code {
        KeyCode::Esc => {
            *mode = Mode::Normal;
            Action::None
        }
        KeyCode::Enter => {
            let command = match kind {
                PromptKind::Pause => parse_duration(buf).map(|delay| Command::Pause {
                    tube: tube.clone(),
                    delay,
                }),
                PromptKind::Kick => parse_count(buf).map(|bound| Command::Kick {
                    tube: tube.clone(),
                    bound,
                }),
            };
            match command {
                Some(command) => {
                    *mode = Mode::Normal;
                    Action::Run(command)
                }
                // No status message: it would linger after Esc, and the prompt
                // already flags the bad input inline while it is being typed.
                None => Action::None,
            }
        }
        KeyCode::Backspace => {
            buf.pop();
            Action::None
        }
        KeyCode::Char(c) if buf.len() < 8 => {
            buf.push(c);
            Action::None
        }
        _ => Action::None,
    }
}

/// Run a command and describe what it did. `kick` acts on the connection's
/// *used* tube rather than taking one as an argument, which is why commands get
/// their own connection: doing this on the poller's would leave it reserving
/// from whichever tube was last kicked.
async fn run_command(client: &mut TuberClient, command: &Command) -> io::Result<String> {
    match command {
        Command::Pause { tube, delay } => {
            client.pause_tube(tube, *delay).await?;
            Ok(match delay {
                0 => format!("{tube} unpaused"),
                d => format!("{tube} paused for {}", format_secs(*d as u64)),
            })
        }
        Command::Kick { tube, bound } => {
            client.use_tube(tube).await?;
            let kicked = client.kick(*bound).await?;
            Ok(format!(
                "{tube}: kicked {kicked} job{}",
                plural(kicked as usize)
            ))
        }
    }
}

/// Issue a command on its own short-lived connection, then refresh from the
/// same connection so the result shows immediately instead of waiting out the
/// poll interval.
fn spawn_command(addr: String, command: Command, tx: mpsc::Sender<PollMsg>) {
    tokio::spawn(async move {
        let result: io::Result<(String, Snapshot)> = async {
            let mut client = TuberClient::connect(&addr).await?;
            let msg = run_command(&mut client, &command).await?;
            Ok((msg, client.fetch_snapshot().await?))
        }
        .await;

        let (msg, error) = match result {
            Ok((msg, snap)) => {
                let _ = tx.send(PollMsg::Snapshot(Box::new(snap))).await;
                (msg, false)
            }
            Err(e) => (format!("{}: {e}", command.tube()), true),
        };
        let _ = tx.send(PollMsg::Status { msg, error }).await;
    });
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Terminal setup
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = ratatui::init();

    let (tx, mut rx) = mpsc::channel::<PollMsg>(4);
    let addr = tuber_lib::resolve_addr(cli.addr.as_deref());
    let interval = Duration::from_secs_f64(cli.interval);

    // Kept for command dispatch and for display; the poller takes its own copies.
    let cmd_addr = addr.clone();
    let addr_label = addr.clone();
    let cmd_tx = tx.clone();

    // Poller task
    tokio::spawn(async move {
        loop {
            match TuberClient::connect(&addr).await {
                Ok(mut client) => loop {
                    match client.fetch_snapshot().await {
                        Ok(snap) => {
                            if tx.send(PollMsg::Snapshot(Box::new(snap))).await.is_err() {
                                return;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(PollMsg::Error(e.to_string())).await;
                            break; // reconnect
                        }
                    }
                    tokio::time::sleep(interval).await;
                },
                Err(e) => {
                    let _ = tx.send(PollMsg::Error(format!("Connect failed: {e}"))).await;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    });

    let mut app = App::new(addr_label);
    let mut event_stream = EventStream::new();

    // Initial render
    terminal.draw(|f| ui::render(f, &app))?;

    loop {
        tokio::select! {
            Some(msg) = rx.recv() => {
                match msg {
                    PollMsg::Snapshot(snap) => app.update(*snap),
                    PollMsg::Error(e) => app.set_error(e),
                    PollMsg::Status { msg, error } => app.set_status(msg, error),
                }
                terminal.draw(|f| ui::render(f, &app))?;
            }
            Some(Ok(event)) = event_stream.next() => {
                // Only redraw for events that can change the frame. Key release
                // and repeat events arrive on some terminals and would
                // otherwise cost a full render each.
                let redraw = match event {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        match handle_key(&mut app, key) {
                            Action::Quit => break,
                            Action::Run(command) => {
                                spawn_command(cmd_addr.clone(), command, cmd_tx.clone());
                            }
                            Action::None => {}
                        }
                        true
                    }
                    // The chart/stats split is derived from the frame height,
                    // so a resize must re-render.
                    Event::Resize(_, _) => true,
                    _ => false,
                };
                if redraw {
                    terminal.draw(|f| ui::render(f, &app))?;
                }
            }
        }
    }

    // Terminal teardown
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}
