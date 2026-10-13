mod app;
mod ui;

use app::{
    flush_target, format_secs, kick_target, parse_count, parse_duration, parse_scope, plural, App,
    FlushScope, Mode, PromptKind,
};
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
    /// Server address [host][:port] [default: $TUBER_URL, $TUBER_ADDR,
    /// $BEANSTALKD_URL, then localhost:11300]
    #[arg(value_name = "HOST")]
    host: Option<String>,

    /// Server address, as tuber-cli takes it; the same as HOST
    #[arg(short, long, value_name = "ADDR", conflicts_with = "host")]
    addr: Option<String>,

    /// Poll interval in seconds
    #[arg(short, long, default_value = "1.5")]
    interval: f64,
}

impl Cli {
    fn addr(self) -> Option<String> {
        self.addr.or(self.host)
    }
}

enum PollMsg {
    Snapshot(Box<Snapshot>),
    Error(String),
    Status { msg: String, error: bool },
}

/// A server command a keypress asked for.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Pause { tube: String, delay: u32 },
    Kick { tube: String, bound: u32 },
    Flush { tube: String, scope: FlushScope },
}

impl Command {
    /// The tube the command acts on, for error reporting.
    fn tube(&self) -> &str {
        match self {
            Command::Pause { tube, .. }
            | Command::Kick { tube, .. }
            | Command::Flush { tube, .. } => tube,
        }
    }
}

/// What a keypress asked for. Commands are returned rather than issued inline
/// so key handling stays synchronous and testable.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    None,
    Quit,
    Run(Command),
}

fn handle_key(app: &mut App, key: KeyEvent) -> Action {
    // The prompt swallows everything, including `q` — otherwise typing a
    // duration could quit the app.
    if matches!(app.mode, Mode::Prompt { .. }) {
        return handle_prompt_key(app, key);
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
        KeyCode::Char('f') => {
            open_prompt(app, PromptKind::Flush);
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
/// jobs can only ever report "kicked 0", and a flush prompt on an empty tube
/// has nothing to delete in either scope.
fn open_prompt(app: &mut App, kind: PromptKind) {
    let Some(tube) = app.selected_tube() else {
        app.set_status(no_tube_reason(app), true);
        return;
    };
    let (available, _) = kick_target(tube.current_jobs_buried, tube.current_jobs_delayed);
    // Read before the shadow below ends the borrow of the snapshot.
    let total = tube.current_total();
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
        PromptKind::Flush if total == 0 => {
            app.set_status(format!("{tube} is empty"), false);
            return;
        }
        // No prefill, deliberately: typing the scope word is the act of
        // consent for a command with no undo. A tube with jobs but none
        // buried still opens, since `all` may be what is wanted.
        PromptKind::Flush => String::new(),
    };
    app.mode = Mode::Prompt { kind, tube, buf };
}

/// Destructures the mode once, so the buffer being read on Enter is the same
/// one the character keys wrote to. Takes the whole `App` because a flush reads
/// the live snapshot to decide whether Enter has anything to act on.
fn handle_prompt_key(app: &mut App, key: KeyEvent) -> Action {
    let Mode::Prompt { kind, tube, buf } = &mut app.mode else {
        return Action::None;
    };
    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::Normal;
            Action::None
        }
        KeyCode::Enter => {
            // Copied out so the borrow of `app.mode` ends before the snapshot
            // is read.
            let (kind, tube, buf) = (*kind, tube.clone(), buf.clone());
            match prompt_command(kind, &tube, &buf, app.tube_by_name(&tube)) {
                Some(command) => {
                    app.mode = Mode::Normal;
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

/// What Enter on a prompt asks the server to do, or `None` when it would ask
/// for nothing — bad input, or a command whose count is already zero. `stats`
/// is the prompt's tube in the latest snapshot, `None` once it has vanished.
///
/// Every kind refuses a no-op, the rule `parse_count` already encodes for kick:
/// Enter on a prompt should always do something, and the hint on screen has
/// said `= nothing to delete` all along.
fn prompt_command(
    kind: PromptKind,
    tube: &str,
    buf: &str,
    stats: Option<&tuber_lib::model::TubeStats>,
) -> Option<Command> {
    let tube = tube.to_string();
    match kind {
        PromptKind::Pause => parse_duration(buf).map(|delay| Command::Pause { tube, delay }),
        PromptKind::Kick => parse_count(buf).map(|bound| Command::Kick { tube, bound }),
        PromptKind::Flush => {
            let scope = parse_scope(buf)?;
            (flush_target(scope, stats?) > 0).then_some(Command::Flush { tube, scope })
        }
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
        // Both flushes name their tube in the command, so neither needs a
        // `use`. A server without the extension answers UNKNOWN_COMMAND, which
        // the client raises as an error and `spawn_command` shows in red.
        Command::Flush { tube, scope } => {
            let (flushed, what) = match scope {
                FlushScope::Buried => (client.flush_buried(tube).await?, "buried "),
                FlushScope::All => (client.flush_tube(tube).await?, ""),
            };
            Ok(format!(
                "{tube}: flushed {flushed} {what}job{}",
                plural(flushed as usize)
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
    let interval = Duration::from_secs_f64(cli.interval);
    let addr = tuber_lib::server_addr(cli.addr().as_deref());

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

#[cfg(test)]
mod tests {
    use super::*;
    use app::test_support::{app_with, tube};
    use app::{FlushScope, Selection};
    use crossterm::event::KeyModifiers;

    fn cli_addr(args: &[&str]) -> Result<Option<String>, clap::Error> {
        Cli::try_parse_from(std::iter::once("tuber-tui").chain(args.iter().copied())).map(|c| c.addr())
    }

    /// `-a`/`--addr` is how tuber-cli takes the address, so it works here too,
    /// beside the bare HOST.
    #[test]
    fn the_address_is_a_bare_host_or_addr_as_in_tuber_cli() {
        assert_eq!(cli_addr(&[]).unwrap(), None);
        assert_eq!(cli_addr(&["hetz01:11331"]).unwrap().as_deref(), Some("hetz01:11331"));
        assert_eq!(cli_addr(&["--addr", "hetz01:11331"]).unwrap().as_deref(), Some("hetz01:11331"));
        assert_eq!(cli_addr(&["-a", ":11301", "-i", "0.5"]).unwrap().as_deref(), Some(":11301"));
    }

    #[test]
    fn a_host_and_addr_together_are_refused() {
        let err = cli_addr(&["hetz01:11331", "--addr", "hetz01:11300"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    /// A plain press. `handle_key` is only ever reached for `KeyEventKind::Press`
    /// — the filter is in the select loop — so the default kind is right here.
    fn press(app: &mut App, code: KeyCode) -> Action {
        handle_key(app, KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// Type a buffer into an open prompt, one character at a time.
    fn type_in(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    /// The open prompt's kind, tube and buffer. `None` in Normal mode.
    fn prompt(app: &App) -> Option<(PromptKind, String, String)> {
        match &app.mode {
            Mode::Prompt { kind, tube, buf } => Some((*kind, tube.clone(), buf.clone())),
            Mode::Normal => None,
        }
    }

    fn status(app: &App) -> (String, bool) {
        let s = app.status.as_ref().expect("a status was set");
        (s.msg.clone(), s.error)
    }

    /// A tube holding work in every state, with the cursor on it.
    fn app_on_tube() -> App {
        let mut app = app_with(vec![tube("q", 5, 12, 3, 23)]);
        app.selected = Some(Selection::Tube("q".to_string()));
        app
    }

    #[test]
    fn flush_refuses_the_server_row() {
        let mut app = app_on_tube();
        app.selected = Some(Selection::Server);
        assert_eq!(press(&mut app, KeyCode::Char('f')), Action::None);
        assert!(prompt(&app).is_none(), "no prompt on the server row");
        assert!(status(&app).1, "the refusal is an error");
    }

    #[test]
    fn flush_refuses_a_tube_with_no_jobs() {
        let mut app = app_with(vec![tube("q", 0, 0, 0, 0)]);
        app.selected = Some(Selection::Tube("q".to_string()));
        assert_eq!(press(&mut app, KeyCode::Char('f')), Action::None);
        assert!(prompt(&app).is_none());
        // Not an error: the key worked, there is simply nothing to delete.
        assert_eq!(status(&app), ("q is empty".to_string(), false));
    }

    #[test]
    fn flush_opens_an_empty_prompt() {
        let mut app = app_on_tube();
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(
            prompt(&app),
            Some((PromptKind::Flush, "q".to_string(), String::new())),
            "the scope word is the deliberate act, so nothing is prefilled"
        );
    }

    #[test]
    fn typing_all_flushes_the_whole_tube() {
        let mut app = app_on_tube();
        press(&mut app, KeyCode::Char('f'));
        type_in(&mut app, "all");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::Run(Command::Flush {
                tube: "q".to_string(),
                scope: FlushScope::All,
            })
        );
        assert!(prompt(&app).is_none(), "the prompt closes on a command");
    }

    #[test]
    fn a_bare_b_flushes_the_buried_jobs() {
        let mut app = app_on_tube();
        press(&mut app, KeyCode::Char('f'));
        type_in(&mut app, "b");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::Run(Command::Flush {
                tube: "q".to_string(),
                scope: FlushScope::Buried,
            })
        );
    }

    #[test]
    fn enter_does_nothing_until_the_scope_names_jobs_that_exist() {
        // A tube with jobs but none buried still opens the prompt, since `all`
        // may be what is wanted — but `buried` there can only report 0.
        let mut app = app_with(vec![tube("q", 5, 0, 3, 0)]);
        app.selected = Some(Selection::Tube("q".to_string()));

        for buf in ["", "x", "b"] {
            press(&mut app, KeyCode::Char('f'));
            type_in(&mut app, buf);
            assert_eq!(press(&mut app, KeyCode::Enter), Action::None, "buf {buf:?}");
            assert!(
                prompt(&app).is_some(),
                "the prompt stays open on {buf:?} so it can be corrected"
            );
            press(&mut app, KeyCode::Esc);
        }
    }

    #[test]
    fn esc_closes_the_flush_prompt() {
        let mut app = app_on_tube();
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(prompt(&app).is_none());
    }
}
