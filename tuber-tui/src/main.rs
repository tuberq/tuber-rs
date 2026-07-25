mod app;
mod ui;

use app::{format_secs, parse_duration, App, Mode};
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

/// What a keypress asked for. Commands are returned rather than issued inline
/// so key handling stays synchronous and testable.
enum Action {
    None,
    Quit,
    Pause { tube: String, delay: u32 },
}

fn handle_key(app: &mut App, key: KeyEvent) -> Action {
    // The prompt swallows everything, including `q` — otherwise typing a
    // duration could quit the app.
    if matches!(app.mode, Mode::PausePrompt { .. }) {
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
            match app.selected.clone() {
                Some(tube) => {
                    app.mode = Mode::PausePrompt {
                        tube,
                        buf: String::new(),
                    }
                }
                None => app.set_status("No tube selected".to_string(), true),
            }
            Action::None
        }
        KeyCode::Char('u') => {
            let target = app.selected_tube().map(|t| (t.name.clone(), t.is_paused()));
            match target {
                Some((tube, true)) => Action::Pause { tube, delay: 0 },
                Some((tube, false)) => {
                    app.set_status(format!("{tube} is not paused"), false);
                    Action::None
                }
                None => Action::None,
            }
        }
        _ => Action::None,
    }
}

/// Takes the mode by reference and destructures once, so the buffer being read
/// on Enter is the same one the character keys wrote to.
fn handle_prompt_key(mode: &mut Mode, key: KeyEvent) -> Action {
    let Mode::PausePrompt { tube, buf } = mode else {
        return Action::None;
    };
    match key.code {
        KeyCode::Esc => {
            *mode = Mode::Normal;
            Action::None
        }
        KeyCode::Enter => match parse_duration(buf) {
            Some(delay) => {
                let tube = tube.clone();
                *mode = Mode::Normal;
                Action::Pause { tube, delay }
            }
            // No status message: it would linger after Esc, and the prompt
            // already flags the bad input inline while it is being typed.
            None => Action::None,
        },
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

/// Issue a pause on its own short-lived connection. `pause-tube` is server-side
/// tube state rather than connection state, so this is equivalent to issuing it
/// on the poller's connection — and it leaves the poller's `use`/`watch` state
/// untouched, which will matter once `kick` lands.
fn spawn_pause(addr: String, tube: String, delay: u32, tx: mpsc::Sender<PollMsg>) {
    tokio::spawn(async move {
        let result: io::Result<Snapshot> = async {
            let mut client = TuberClient::connect(&addr).await?;
            client.pause_tube(&tube, delay).await?;
            // Refresh on the same connection so the change shows immediately
            // instead of waiting out the poll interval.
            client.fetch_snapshot().await
        }
        .await;

        let (msg, error) = match result {
            Ok(snap) => {
                let _ = tx.send(PollMsg::Snapshot(Box::new(snap))).await;
                let what = if delay == 0 {
                    "unpaused".to_string()
                } else {
                    format!("paused for {}", format_secs(delay as u64))
                };
                (format!("{tube} {what}"), false)
            }
            Err(e) => (format!("{tube}: {e}"), true),
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

    // Kept for command dispatch; the poller takes its own copies.
    let cmd_addr = addr.clone();
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

    let mut app = App::new();
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
                            Action::Pause { tube, delay } => {
                                spawn_pause(cmd_addr.clone(), tube, delay, cmd_tx.clone());
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
