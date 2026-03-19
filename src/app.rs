use crate::model::Snapshot;

pub struct App {
    pub current: Option<Snapshot>,
    pub previous: Option<Snapshot>,
    pub error: Option<String>,
}

impl App {
    pub fn new() -> Self {
        Self {
            current: None,
            previous: None,
            error: None,
        }
    }

    pub fn update(&mut self, snapshot: Snapshot) {
        self.previous = self.current.take();
        self.current = Some(snapshot);
        self.error = None;
    }

    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
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

    /// Get rates for server-level throughput counters.
    pub fn server_rates(&self) -> (f64, f64, f64, f64) {
        if let (Some(cur), Some(prev)) = (&self.current, &self.previous) {
            let puts = self.rate(cur.server.cmd_put, prev.server.cmd_put);
            let reserves = self.rate(cur.server.cmd_reserve, prev.server.cmd_reserve);
            let deletes = self.rate(cur.server.cmd_delete, prev.server.cmd_delete);
            let timeouts = self.rate(cur.server.job_timeouts, prev.server.job_timeouts);
            (puts, reserves, deletes, timeouts)
        } else {
            (0.0, 0.0, 0.0, 0.0)
        }
    }
}
