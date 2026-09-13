use std::time::{Duration, Instant};
use tuber_lib::model::{Snapshot, TubeStats};

/// How long a command result stays on screen.
const STATUS_TTL: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortMode {
    Name,
    Size,
}

impl SortMode {
    pub fn next(self) -> Self {
        match self {
            SortMode::Name => SortMode::Size,
            SortMode::Size => SortMode::Name,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SortMode::Name => "name",
            SortMode::Size => "size",
        }
    }
}

/// What a prompt is collecting. The prompt machinery — buffer editing, Esc,
/// Enter — is identical for all of them; only parsing and wording differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    /// A pause duration, parsed by [`parse_duration`].
    Pause,
    /// A kick bound, parsed by [`parse_count`].
    Kick,
    /// A flush scope, parsed by [`parse_scope`].
    Flush,
}

pub enum Mode {
    Normal,
    /// Awaiting input for `tube`. `buf` is the raw keyed input.
    Prompt {
        kind: PromptKind,
        tube: String,
        buf: String,
    },
}

pub struct Status {
    pub msg: String,
    pub error: bool,
    pub at: Instant,
}

/// What the cursor is on.
///
/// An enum rather than an `Option<String>` because the server's totals row is
/// selectable like a tube, and a tube may legitimately be named "server" — so
/// the row cannot be identified by a sentinel name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    Tube(String),
    /// The totals row below the last tube. Selecting it turns the detail pane
    /// over to the server, which is why there is no separate server page: the
    /// cursor already says what the pane should describe.
    Server,
}

impl Selection {
    /// The tube's name, or `None` when the cursor is on the server row.
    pub fn tube_name(&self) -> Option<&str> {
        match self {
            Selection::Tube(name) => Some(name),
            Selection::Server => None,
        }
    }

    /// What the row is called on screen. Read by both the chart's totals row and
    /// the detail pane's heading, so the two cannot come to call it different
    /// things.
    pub fn label(&self) -> &str {
        match self {
            Selection::Tube(name) => name,
            Selection::Server => "server",
        }
    }
}

pub struct App {
    /// The address we dialled, as resolved from the argument or `TUBER_ADDR`.
    ///
    /// Worth holding because it is the only identity of the server the *client*
    /// is sure of: `stats` reports the server's own `hostname`, which inside a
    /// container is the container ID, so it answers "which box is this" rather
    /// than "which server am I watching".
    pub addr: String,
    pub current: Option<Snapshot>,
    pub previous: Option<Snapshot>,
    pub error: Option<String>,
    /// Selection is held by tube *name*, never row index: the list re-sorts
    /// on every poll and an index would slide onto a different tube.
    pub selected: Option<Selection>,
    pub sort: SortMode,
    pub mode: Mode,
    pub hide_empty: bool,
    pub status: Option<Status>,
}

impl App {
    pub fn new(addr: String) -> Self {
        Self {
            addr,
            current: None,
            previous: None,
            error: None,
            selected: None,
            sort: SortMode::Name,
            mode: Mode::Normal,
            // Empty tubes are hidden by default: on a real server most tubes
            // are empty most of the time, and they crowd out the ones doing
            // work. Paused tubes stay visible even at zero — see pinned.
            hide_empty: true,
            status: None,
        }
    }

    pub fn update(&mut self, snapshot: Snapshot) {
        self.previous = self.current.take();
        self.current = Some(snapshot);
        self.error = None;
        self.reconcile_selection();
    }

    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
    }

    pub fn set_status(&mut self, msg: String, error: bool) {
        self.status = Some(Status {
            msg,
            error,
            at: Instant::now(),
        });
    }

    /// The current status message, if one is set and still fresh.
    pub fn live_status(&self) -> Option<&Status> {
        self.status.as_ref().filter(|s| s.at.elapsed() < STATUS_TTL)
    }

    /// Point the cursor at the first visible tube if it currently points at
    /// nothing, or at a tube that has vanished or been filtered out.
    ///
    /// The server row is always a valid target, so a cursor resting there is
    /// never disturbed — and it is where the cursor lands when there are no
    /// tubes to point at, rather than nowhere.
    pub fn reconcile_selection(&mut self) {
        if self.selected == Some(Selection::Server) {
            return;
        }
        // Scoped so the borrow ends before `selected` is assigned; only the one
        // replacement name is cloned, not every visible tube's name.
        let replacement = {
            let tubes = self.visible_tubes();
            match self.index_of_selected(&tubes) {
                Some(_) => None,
                None => Some(match tubes.first() {
                    Some(t) => Selection::Tube(t.name.clone()),
                    None => Selection::Server,
                }),
            }
        };
        if let Some(selection) = replacement {
            self.selected = Some(selection);
        }
    }

    pub fn toggle_hide_empty(&mut self) {
        self.hide_empty = !self.hide_empty;
        self.reconcile_selection();
    }

    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
    }

    /// Total tubes the server reported, before any filtering.
    pub fn tube_count(&self) -> usize {
        self.current.as_ref().map_or(0, |snap| snap.tubes.len())
    }

    /// Tubes matching `keep`, in display order.
    fn filtered_tubes(&self, keep: impl Fn(&TubeStats) -> bool) -> Vec<&TubeStats> {
        let Some(snap) = &self.current else {
            return Vec::new();
        };
        let mut tubes: Vec<&TubeStats> = snap.tubes.iter().filter(|t| keep(t)).collect();
        self.sort_tubes(&mut tubes);
        tubes
    }

    /// Tubes shown in the chart. Single source of truth for both rendering and
    /// cursor movement, so the two cannot disagree — callers should compute
    /// this once per frame and pass the slice down.
    pub fn visible_tubes(&self) -> Vec<&TubeStats> {
        self.filtered_tubes(|t| !self.hide_empty || pinned(t) || self.tube_active(t))
    }

    /// Holding jobs now, or having moved any since the last poll.
    ///
    /// This pins a tube that drained a moment ago onto the screen for a poll or
    /// two. It used to select the timing table's rows, which was deliberately
    /// *not* a subset of the chart's — a just-drained tube was absent from the
    /// chart while its percentiles were still the most interesting thing on
    /// screen. Now that the timing detail is a pane for the *selected* tube,
    /// unreachable means uninspectable, so the same predicate has to keep the
    /// tube selectable instead.
    fn tube_active(&self, tube: &TubeStats) -> bool {
        if tube.current_total() > 0 {
            return true;
        }
        match self.previous_tube(&tube.name) {
            Some(prev) => {
                tube.total_reserves > prev.total_reserves || tube.cmd_delete > prev.cmd_delete
            }
            None => false,
        }
    }

    fn sort_tubes(&self, tubes: &mut [&TubeStats]) {
        match self.sort {
            SortMode::Name => tubes.sort_by(|a, b| a.name.cmp(&b.name)),
            SortMode::Size => tubes.sort_by_key(|t| std::cmp::Reverse(t.current_total())),
        }
    }

    /// Row of the cursor within an already-computed visible list. The single
    /// encoding of "the cursor is on a real row" — callers that already hold
    /// the list should use this rather than recomputing it.
    pub fn index_of_selected(&self, tubes: &[&TubeStats]) -> Option<usize> {
        let name = self.selected.as_ref()?.tube_name()?;
        tubes.iter().position(|t| t.name == name)
    }

    /// True when the cursor is on the server's totals row.
    pub fn server_selected(&self) -> bool {
        self.selected == Some(Selection::Server)
    }

    /// Move the cursor over the visible tubes and the server row below them,
    /// which is one list as far as navigation is concerned — pressing down past
    /// the last tube lands on the totals rather than stopping.
    pub fn move_selection(&mut self, delta: isize) {
        let next = {
            let tubes = self.visible_tubes();
            // The server row is the last position, so the list is never empty.
            let server_row = tubes.len() as isize;
            let current = match self.selected {
                Some(Selection::Server) => server_row,
                _ => self.index_of_selected(&tubes).unwrap_or(0) as isize,
            };
            match (current + delta).clamp(0, server_row) {
                row if row == server_row => Selection::Server,
                row => Selection::Tube(tubes[row as usize].name.clone()),
            }
        };
        self.selected = Some(next);
    }

    /// A tube's stats from the latest snapshot.
    pub fn tube_by_name(&self, name: &str) -> Option<&TubeStats> {
        self.current.as_ref()?.tubes.iter().find(|t| t.name == name)
    }

    /// The selected tube's stats, if the cursor is on one. `None` when it is on
    /// the server row, which is what makes the tube commands refuse there.
    pub fn selected_tube(&self) -> Option<&TubeStats> {
        self.tube_by_name(self.selected.as_ref()?.tube_name()?)
    }

    /// Calculate rate (per second) of a cumulative counter between snapshots.
    pub fn rate(&self, current_val: u64, previous_val: u64) -> f64 {
        if let (Some(cur), Some(prev)) = (&self.current, &self.previous) {
            let elapsed = cur.fetched_at.duration_since(prev.fetched_at).as_secs_f64();
            if elapsed > 0.0 {
                return (current_val.saturating_sub(previous_val)) as f64 / elapsed;
            }
        }
        0.0
    }

    fn previous_tube(&self, name: &str) -> Option<&TubeStats> {
        self.previous
            .as_ref()
            .and_then(|snap| snap.tubes.iter().find(|t| t.name == name))
    }

    /// Get the previous total job count for a tube by name.
    pub fn previous_tube_total(&self, name: &str) -> Option<u64> {
        self.previous_tube(name).map(|t| t.current_total())
    }

    /// Get rates for server-level throughput counters.
    pub fn server_rates(&self) -> (f64, f64, f64, f64) {
        if let (Some(cur), Some(prev)) = (&self.current, &self.previous) {
            let puts = self.rate(cur.server.cmd_put, prev.server.cmd_put);
            let reserves = self.rate(
                cur.server.cmd_reserve + cur.server.cmd_reserve_with_timeout,
                prev.server.cmd_reserve + prev.server.cmd_reserve_with_timeout,
            );
            let deletes = self.rate(cur.server.cmd_delete, prev.server.cmd_delete);
            let timeouts = self.rate(cur.server.job_timeouts, prev.server.job_timeouts);
            (puts, reserves, deletes, timeouts)
        } else {
            (0.0, 0.0, 0.0, 0.0)
        }
    }
}

/// Reasons a tube stays on screen even when empty tubes are hidden, beyond
/// holding jobs or having just moved some — which is `tube_active`'s question.
/// Kept separate so visibility policy can grow without overloading what "empty"
/// means: a paused tube at zero is exactly what the badge exists to show.
fn pinned(tube: &TubeStats) -> bool {
    tube.is_paused()
}

/// Parse a pause duration. Bare numbers are seconds, matching
/// `tuber-cli pause --delay`; `m` and `h` suffixes scale from there.
/// `0` is valid and means unpause.
pub fn parse_duration(input: &str) -> Option<u32> {
    let s = input.trim();
    let last = s.chars().last()?;
    let (digits, multiplier) = match last {
        's' | 'S' => (&s[..s.len() - 1], 1u32),
        'm' | 'M' => (&s[..s.len() - 1], 60),
        'h' | 'H' => (&s[..s.len() - 1], 3600),
        _ => (s, 1),
    };
    let n: u32 = digits.trim().parse().ok()?;
    n.checked_mul(multiplier)
}

/// Parse a kick bound: a plain count of jobs. Zero is rejected — `kick 0` is a
/// no-op, and Enter on a prompt should always do something.
pub fn parse_count(input: &str) -> Option<u32> {
    let n: u32 = input.trim().parse().ok()?;
    (n > 0).then_some(n)
}

/// What a single `kick` would act on: how many jobs, and which state they are
/// in. The server kicks buried jobs whenever the tube has any and only falls
/// through to delayed once there are none — one state per command, never both,
/// so a bound past the buried count does *not* spill into the delayed jobs.
/// This is therefore the ceiling on what any one kick can move.
pub fn kick_target(buried: u64, delayed: u64) -> (u64, &'static str) {
    match buried {
        0 => (delayed, "delayed"),
        buried => (buried, "buried"),
    }
}

/// What `kick <bound>` will actually move: `12 buried`, `5 delayed`, or
/// `nothing to kick`. The prompt's only feedback, and worth spelling out
/// because the state being kicked is not something the bound alone tells you.
pub fn kick_plan(bound: u32, available: u64, state: &str) -> String {
    match available.min(bound as u64) {
        0 => "nothing to kick".to_string(),
        n => format!("{n} {state}"),
    }
}

/// Which jobs a flush throws away: only the buried ones (`flush-buried`), or
/// every job in the tube (`flush-tube`, which takes reserved jobs too).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlushScope {
    Buried,
    All,
}

/// Parse a flush scope: `buried` or `all`, case-insensitive, and any prefix of
/// either — they differ at the first letter, so `b` and `a` are unambiguous.
/// The empty buffer is rejected before the prefix test, which would otherwise
/// match it against both.
pub fn parse_scope(input: &str) -> Option<FlushScope> {
    let s = input.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    if "buried".starts_with(&s) {
        return Some(FlushScope::Buried);
    }
    if "all".starts_with(&s) {
        return Some(FlushScope::All);
    }
    None
}

/// How many jobs a flush of `scope` would delete right now. The one count that
/// opening the prompt, the hint and Enter all read, so they cannot disagree.
pub fn flush_target(scope: FlushScope, tube: &TubeStats) -> u64 {
    match scope {
        FlushScope::Buried => tube.current_jobs_buried,
        FlushScope::All => tube.current_total(),
    }
}

/// What Enter will delete: `delete 23 buried`, `delete 1943 jobs (12 reserved)`
/// or `nothing to delete`. Reserved jobs are called out for `all` because
/// flushing takes them out from under a worker that is still running them —
/// the one consequence of this command that the total alone does not show.
pub fn flush_plan(scope: FlushScope, tube: &TubeStats) -> String {
    let n = flush_target(scope, tube);
    if n == 0 {
        return "nothing to delete".to_string();
    }
    match scope {
        FlushScope::Buried => format!("delete {n} buried"),
        FlushScope::All => {
            let jobs = format!("delete {n} job{}", plural(n as usize));
            match tube.current_jobs_reserved {
                0 => jobs,
                reserved => format!("{jobs} ({reserved} reserved)"),
            }
        }
    }
}

/// Pluralising suffix for a count. Here rather than in `ui.rs` so the command
/// results in `main.rs` can reach it too.
pub fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Compact duration for the pause badge: `45s`, `12m`, `2h5m`.
pub fn format_secs(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    // Minutes round up. Asking for 5m and immediately seeing "PAUSED 4m"
    // because three seconds elapsed reads as though it was short-changed.
    let mins = secs.div_ceil(60);
    if mins < 60 {
        format!("{mins}m")
    } else {
        let (hours, rem) = (mins / 60, mins % 60);
        if rem == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h{rem}m")
        }
    }
}

/// Fixtures shared by the unit tests in this crate's other modules. `ui.rs`
/// keeps its own richer tubes; these are the minimum a key handler needs.
#[cfg(test)]
pub mod test_support {
    use super::*;
    use tuber_lib::model::ServerStats;

    pub fn tube(name: &str, ready: u64, reserved: u64, delayed: u64, buried: u64) -> TubeStats {
        TubeStats::from_yaml(&format!(
            "name: {name}\n\
             current-jobs-ready: {ready}\n\
             current-jobs-reserved: {reserved}\n\
             current-jobs-delayed: {delayed}\n\
             current-jobs-buried: {buried}\n",
        ))
    }

    pub fn app_with(tubes: Vec<TubeStats>) -> App {
        let mut app = App::new("test:11300".to_string());
        app.update(Snapshot {
            server: ServerStats::from_yaml("version: tuber 0.14.2\n"),
            tubes,
            fetched_at: Instant::now(),
        });
        app
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_numbers_are_seconds() {
        assert_eq!(parse_duration("30"), Some(30));
        assert_eq!(parse_duration("0"), Some(0));
        assert_eq!(parse_duration(" 90 "), Some(90));
    }

    #[test]
    fn suffixes_scale() {
        assert_eq!(parse_duration("45s"), Some(45));
        assert_eq!(parse_duration("5m"), Some(300));
        assert_eq!(parse_duration("2h"), Some(7200));
        assert_eq!(parse_duration("2H"), Some(7200));
        assert_eq!(parse_duration("5 m"), Some(300));
    }

    #[test]
    fn rejects_junk() {
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("   "), None);
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("m"), None);
        assert_eq!(parse_duration("-5"), None);
        assert_eq!(parse_duration("1.5m"), None);
        // Would overflow u32 seconds.
        assert_eq!(parse_duration("9999999h"), None);
    }

    #[test]
    fn formats_compactly() {
        assert_eq!(format_secs(0), "0s");
        assert_eq!(format_secs(45), "45s");
        assert_eq!(format_secs(3600), "1h");
    }

    #[test]
    fn minutes_round_up_so_a_fresh_pause_shows_what_was_asked_for() {
        // pause-time-left is already a few seconds down by the time the badge
        // renders; a 5m pause must not immediately read as 4m.
        assert_eq!(format_secs(297), "5m");
        assert_eq!(format_secs(300), "5m");
        assert_eq!(format_secs(747), "13m");
        assert_eq!(format_secs(3916), "1h6m");
        // Carrying into the next hour must not produce "1h60m".
        assert_eq!(format_secs(7150), "2h");
    }

    #[test]
    fn counts_are_positive_integers() {
        assert_eq!(parse_count("1"), Some(1));
        assert_eq!(parse_count(" 250 "), Some(250));
        // Unlike a pause duration, 0 does nothing at all.
        assert_eq!(parse_count("0"), None);
        assert_eq!(parse_count(""), None);
        assert_eq!(parse_count("-1"), None);
        assert_eq!(parse_count("5m"), None);
        assert_eq!(parse_count("1.5"), None);
    }

    /// Verified against tuber 0.13.0: a tube with 3 buried and 5 delayed given
    /// `kick 5` replies `KICKED 3`. The bound does not spill into the delayed
    /// jobs, so neither may the plan.
    #[test]
    fn kick_targets_buried_while_any_remain() {
        assert_eq!(kick_target(42, 3), (42, "buried"));
        assert_eq!(kick_target(0, 3), (3, "delayed"));
        assert_eq!(kick_target(0, 0), (0, "delayed"));
    }

    #[test]
    fn kick_plan_reports_the_state_it_will_move() {
        let (avail, state) = kick_target(42, 3);
        assert_eq!(kick_plan(5, avail, state), "5 buried");
        assert_eq!(kick_plan(42, avail, state), "42 buried");
        // A bound past the buried count still only kicks the buried jobs.
        assert_eq!(kick_plan(1000, avail, state), "42 buried");

        let (avail, state) = kick_target(0, 10);
        assert_eq!(kick_plan(7, avail, state), "7 delayed");
    }

    #[test]
    fn kick_plan_says_so_when_there_is_nothing_to_do() {
        let (avail, state) = kick_target(0, 0);
        assert_eq!(kick_plan(u32::MAX, avail, state), "nothing to kick");
    }

    #[test]
    fn scopes_accept_unambiguous_prefixes() {
        assert_eq!(parse_scope("buried"), Some(FlushScope::Buried));
        assert_eq!(parse_scope("b"), Some(FlushScope::Buried));
        assert_eq!(parse_scope("BU"), Some(FlushScope::Buried));
        assert_eq!(parse_scope(" buried "), Some(FlushScope::Buried));
        assert_eq!(parse_scope("all"), Some(FlushScope::All));
        assert_eq!(parse_scope("a"), Some(FlushScope::All));
        assert_eq!(parse_scope("Al"), Some(FlushScope::All));
    }

    #[test]
    fn scopes_reject_anything_else() {
        // `"buried".starts_with("")` is true, so the empty buffer has to be
        // turned away before the prefix test runs.
        assert_eq!(parse_scope(""), None);
        assert_eq!(parse_scope("   "), None);
        assert_eq!(parse_scope("x"), None);
        assert_eq!(parse_scope("buriedx"), None);
        assert_eq!(parse_scope("ball"), None);
    }

    #[test]
    fn flush_target_counts_what_the_scope_deletes() {
        let tube = test_support::tube("q", 5, 12, 3, 23);
        assert_eq!(flush_target(FlushScope::Buried, &tube), 23);
        // Every state, reserved included.
        assert_eq!(flush_target(FlushScope::All, &tube), 43);
    }

    #[test]
    fn flush_plan_names_the_pile_it_deletes() {
        let tube = test_support::tube("q", 1908, 12, 0, 23);
        assert_eq!(flush_plan(FlushScope::Buried, &tube), "delete 23 buried");
        // Reserved is called out because flushing pulls those jobs out from
        // under a worker that is still running them.
        assert_eq!(
            flush_plan(FlushScope::All, &tube),
            "delete 1943 jobs (12 reserved)"
        );
    }

    #[test]
    fn flush_plan_mentions_reserved_only_when_there_are_some() {
        assert_eq!(
            flush_plan(FlushScope::All, &test_support::tube("q", 4, 0, 0, 0)),
            "delete 4 jobs"
        );
        assert_eq!(
            flush_plan(FlushScope::All, &test_support::tube("q", 1, 0, 0, 0)),
            "delete 1 job"
        );
    }

    #[test]
    fn flush_plan_says_so_when_there_is_nothing_to_delete() {
        let ready_only = test_support::tube("q", 5, 0, 0, 0);
        assert_eq!(
            flush_plan(FlushScope::Buried, &ready_only),
            "nothing to delete"
        );
        let empty = test_support::tube("q", 0, 0, 0, 0);
        assert_eq!(flush_plan(FlushScope::All, &empty), "nothing to delete");
    }

    #[test]
    fn sort_mode_cycles_back_to_name() {
        assert_eq!(SortMode::Name.next(), SortMode::Size);
        assert_eq!(SortMode::Name.next().next(), SortMode::Name);
    }
}
