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
/// Enter — is identical for both; only parsing and wording differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    /// A pause duration, parsed by [`parse_duration`].
    Pause,
    /// A kick bound, parsed by [`parse_count`].
    Kick,
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

pub struct App {
    pub current: Option<Snapshot>,
    pub previous: Option<Snapshot>,
    pub error: Option<String>,
    /// Selection is held by tube *name*, never row index: the list re-sorts
    /// on every poll and an index would slide onto a different tube.
    pub selected: Option<String>,
    pub sort: SortMode,
    pub mode: Mode,
    pub hide_empty: bool,
    pub status: Option<Status>,
}

impl App {
    pub fn new() -> Self {
        Self {
            current: None,
            previous: None,
            error: None,
            selected: None,
            sort: SortMode::Name,
            mode: Mode::Normal,
            // Empty tubes are hidden by default: on a real server most tubes
            // are empty most of the time, and they crowd out the ones doing
            // work. Paused tubes stay visible even at zero — see is_empty.
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
    pub fn reconcile_selection(&mut self) {
        // Scoped so the borrow ends before `selected` is assigned; only the one
        // replacement name is cloned, not every visible tube's name.
        let replacement = {
            let tubes = self.visible_tubes();
            match self.index_of_selected(&tubes) {
                Some(_) => None,
                None => Some(tubes.first().map(|t| t.name.clone())),
            }
        };
        if let Some(name) = replacement {
            self.selected = name;
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
        self.filtered_tubes(|t| {
            !self.hide_empty || !is_empty(t) || pinned(t) || self.tube_active(t)
        })
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
        let name = self.selected.as_ref()?;
        tubes.iter().position(|t| &t.name == name)
    }

    pub fn move_selection(&mut self, delta: isize) {
        let next_name = {
            let tubes = self.visible_tubes();
            if tubes.is_empty() {
                return;
            }
            let current = self.index_of_selected(&tubes).unwrap_or(0) as isize;
            let next = (current + delta).clamp(0, tubes.len() as isize - 1) as usize;
            tubes[next].name.clone()
        };
        self.selected = Some(next_name);
    }

    /// A tube's stats from the latest snapshot.
    pub fn tube_by_name(&self, name: &str) -> Option<&TubeStats> {
        self.current.as_ref()?.tubes.iter().find(|t| t.name == name)
    }

    /// The selected tube's stats, if the cursor is on one.
    pub fn selected_tube(&self) -> Option<&TubeStats> {
        self.tube_by_name(self.selected.as_deref()?)
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

/// Holding no jobs at all. Purely about *contents*, not recent activity: a
/// tube that drained a moment ago counts as empty. `current_total` covers
/// buried, so an empty tube cannot be hiding failures.
///
/// Distinct from `tube_active`, which is about motion rather than contents.
fn is_empty(tube: &TubeStats) -> bool {
    tube.current_total() == 0
}

/// Reasons a tube stays on screen even when empty tubes are hidden. Kept
/// separate from `is_empty` so visibility policy can grow without overloading
/// what "empty" means: a paused tube at zero is exactly what the badge exists
/// to show.
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
    fn sort_mode_cycles_back_to_name() {
        assert_eq!(SortMode::Name.next(), SortMode::Size);
        assert_eq!(SortMode::Name.next().next(), SortMode::Name);
    }
}
