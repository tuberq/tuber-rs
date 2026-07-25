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

pub enum Mode {
    Normal,
    /// Awaiting a pause duration for `tube`. `buf` is the raw keyed input.
    PausePrompt { tube: String, buf: String },
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
            hide_empty: false,
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
        let visible: Vec<String> = self.visible_tubes().iter().map(|t| t.name.clone()).collect();
        let lost = match &self.selected {
            Some(name) => !visible.contains(name),
            None => true,
        };
        if lost {
            self.selected = visible.first().cloned();
        }
    }

    pub fn toggle_hide_empty(&mut self) {
        self.hide_empty = !self.hide_empty;
        self.reconcile_selection();
    }

    pub fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
    }

    /// Tubes shown in the chart, filtered and sorted. Single source of truth
    /// for both rendering and cursor movement, so the two cannot disagree.
    pub fn visible_tubes(&self) -> Vec<&TubeStats> {
        let Some(snap) = &self.current else {
            return Vec::new();
        };
        let mut tubes: Vec<&TubeStats> = snap
            .tubes
            .iter()
            .filter(|t| !self.hide_empty || !is_idle(t))
            .collect();
        self.sort_tubes(&mut tubes);
        tubes
    }

    pub fn hidden_count(&self) -> usize {
        let Some(snap) = &self.current else {
            return 0;
        };
        if !self.hide_empty {
            return 0;
        }
        snap.tubes.iter().filter(|t| is_idle(t)).count()
    }

    /// Tubes worth a row in the timing table: they have timing data *and* are
    /// doing something. `processing_time_ewma` alone is a lifetime property
    /// that never decays, so it would list every tube the server has ever run.
    pub fn timing_tubes(&self) -> Vec<&TubeStats> {
        let Some(snap) = &self.current else {
            return Vec::new();
        };
        let mut tubes: Vec<&TubeStats> = snap
            .tubes
            .iter()
            .filter(|t| t.processing_time_ewma > 0.0 && self.tube_active(t))
            .collect();
        self.sort_tubes(&mut tubes);
        tubes
    }

    /// Holding jobs now, or having moved any since the last poll.
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

    pub fn selected_index(&self) -> Option<usize> {
        let name = self.selected.as_ref()?;
        self.visible_tubes().iter().position(|t| &t.name == name)
    }

    pub fn move_selection(&mut self, delta: isize) {
        let names: Vec<String> = self.visible_tubes().iter().map(|t| t.name.clone()).collect();
        if names.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|n| names.iter().position(|x| x == n))
            .unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, names.len() as isize - 1) as usize;
        self.selected = Some(names[next].clone());
    }

    /// The selected tube's stats, if the cursor is on one.
    pub fn selected_tube(&self) -> Option<&TubeStats> {
        let name = self.selected.as_ref()?;
        self.current
            .as_ref()?
            .tubes
            .iter()
            .find(|t| &t.name == name)
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

/// A tube with nothing in it and no pause worth reporting. `current_total`
/// covers buried, so an empty tube cannot be hiding failures — but a paused
/// tube at zero is exactly what the pause badge exists to show.
fn is_idle(tube: &TubeStats) -> bool {
    tube.current_total() == 0 && !tube.is_paused()
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
    fn sort_mode_cycles_back_to_name() {
        assert_eq!(SortMode::Name.next(), SortMode::Size);
        assert_eq!(SortMode::Name.next().next(), SortMode::Name);
    }
}
