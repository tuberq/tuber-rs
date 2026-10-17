use crate::parse::{get_bool, get_f64, get_str, get_u64, parse_yaml_map};
use std::time::Instant;

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(dead_code)]
pub struct ServerStats {
    pub version: String,
    pub uptime: u64,
    pub current_connections: u64,
    pub current_producers: u64,
    pub current_workers: u64,
    pub current_waiting: u64,
    /// A subset of `current_jobs_ready`, not a state of its own — the same
    /// relationship the per-tube counts have.
    pub current_jobs_urgent: u64,
    pub current_jobs_ready: u64,
    pub current_jobs_reserved: u64,
    pub current_jobs_delayed: u64,
    /// aft: jobs waiting for their group to complete (tuber 0.17.0). Stored as
    /// delayed with no deadline but counted apart from `current_jobs_delayed`,
    /// and out of reach of `peek-delayed` and `kick`. 0 from older servers and
    /// beanstalkd, which do not send it.
    #[cfg_attr(feature = "serde", serde(default))]
    pub current_jobs_held: u64,
    pub current_jobs_buried: u64,
    pub cmd_put: u64,
    pub cmd_reserve: u64,
    pub cmd_reserve_with_timeout: u64,
    pub cmd_delete: u64,
    pub job_timeouts: u64,
    pub total_jobs: u64,
    pub rusage_utime: f64,
    pub rusage_stime: f64,
    pub rusage_maxrss: u64,
    pub draining: bool,
    pub max_job_size: u64,
    pub current_jobs_size: u64,
    pub max_jobs_size: u64,
    /// Recurring schedule entries held, across all owners.
    pub current_schedules: u64,
    /// Memory the schedule entries are charged against `max_jobs_size`,
    /// beside `current_jobs_size`. See [`memory_in_use`](Self::memory_in_use).
    pub current_schedules_size: u64,
    /// Seconds of silence before TCP keepalive probes start on a client
    /// connection; 0 when off.
    pub tcp_keepalive: u64,
    pub binlog_enabled: bool,
    /// True after a WAL write or fsync error: persistence is gone and puts are
    /// refused with `INTERNAL_ERROR` until restart. `binlog_enabled` and every
    /// `binlog_*` figure read as they do without `-b` then, so this is what
    /// tells a failed WAL from an in-memory server. Always false without `-b`,
    /// and from servers before tuber 0.16.0.
    pub binlog_failed: bool,
    pub binlog_total_bytes: u64,
    pub binlog_file_count: u64,
    pub binlog_current_index: u64,
    pub binlog_oldest_index: u64,
    pub toast_total_bytes: u64,
    pub toast_live_bytes: u64,
    pub toast_segments: u64,
    pub max_storage_bytes: u64,
    pub toast_compactions_total: u64,
    pub toast_bodies_migrated_total: u64,
    pub toast_bodies_dropped_corrupted: u64,
    pub recovered_missing_bodies: u64,
    pub reclaimed_orphan_bodies: u64,
    pub reclaimed_stranded_bodies: u64,
    pub name: String,
    pub hostname: String,
    pub os: String,
    pub platform: String,
    pub processing_time_fast_threshold: f64,
}

impl ServerStats {
    /// What `max_jobs_size` is measured against: job bodies plus recurring
    /// schedule entries. A put is refused `OUT_OF_MEMORY` when this would
    /// pass the limit, so it is the figure to compare with it, not
    /// `current_jobs_size` alone.
    pub fn memory_in_use(&self) -> u64 {
        self.current_jobs_size.saturating_add(self.current_schedules_size)
    }

    pub fn from_yaml(yaml: &str) -> Self {
        let m = parse_yaml_map(yaml);
        Self {
            version: get_str(&m, "version"),
            uptime: get_u64(&m, "uptime"),
            current_connections: get_u64(&m, "current-connections"),
            current_producers: get_u64(&m, "current-producers"),
            current_workers: get_u64(&m, "current-workers"),
            current_waiting: get_u64(&m, "current-waiting"),
            current_jobs_urgent: get_u64(&m, "current-jobs-urgent"),
            current_jobs_ready: get_u64(&m, "current-jobs-ready"),
            current_jobs_reserved: get_u64(&m, "current-jobs-reserved"),
            current_jobs_delayed: get_u64(&m, "current-jobs-delayed"),
            current_jobs_held: get_u64(&m, "current-jobs-held"),
            current_jobs_buried: get_u64(&m, "current-jobs-buried"),
            cmd_put: get_u64(&m, "cmd-put"),
            cmd_reserve: get_u64(&m, "cmd-reserve"),
            cmd_reserve_with_timeout: get_u64(&m, "cmd-reserve-with-timeout"),
            cmd_delete: get_u64(&m, "cmd-delete"),
            job_timeouts: get_u64(&m, "job-timeouts"),
            total_jobs: get_u64(&m, "total-jobs"),
            rusage_utime: get_f64(&m, "rusage-utime"),
            rusage_stime: get_f64(&m, "rusage-stime"),
            rusage_maxrss: get_u64(&m, "rusage-maxrss"),
            draining: get_bool(&m, "draining"),
            max_job_size: get_u64(&m, "max-job-size"),
            current_jobs_size: get_u64(&m, "current-jobs-size"),
            max_jobs_size: get_u64(&m, "max-jobs-size"),
            current_schedules: get_u64(&m, "current-schedules"),
            current_schedules_size: get_u64(&m, "current-schedules-size"),
            tcp_keepalive: get_u64(&m, "tcp-keepalive"),
            binlog_enabled: get_bool(&m, "binlog-enabled"),
            binlog_failed: get_bool(&m, "binlog-failed"),
            binlog_total_bytes: get_u64(&m, "binlog-total-bytes"),
            binlog_file_count: get_u64(&m, "binlog-file-count"),
            binlog_current_index: get_u64(&m, "binlog-current-index"),
            binlog_oldest_index: get_u64(&m, "binlog-oldest-index"),
            toast_total_bytes: get_u64(&m, "toast-total-bytes"),
            toast_live_bytes: get_u64(&m, "toast-live-bytes"),
            toast_segments: get_u64(&m, "toast-segments"),
            max_storage_bytes: get_u64(&m, "max-storage-bytes"),
            toast_compactions_total: get_u64(&m, "toast-compactions-total"),
            toast_bodies_migrated_total: get_u64(&m, "toast-bodies-migrated-total"),
            toast_bodies_dropped_corrupted: get_u64(&m, "toast-bodies-dropped-corrupted"),
            recovered_missing_bodies: get_u64(&m, "recovered-missing-bodies"),
            reclaimed_orphan_bodies: get_u64(&m, "reclaimed-orphan-bodies"),
            reclaimed_stranded_bodies: get_u64(&m, "reclaimed-stranded-bodies"),
            name: get_str(&m, "name"),
            hostname: get_str(&m, "hostname"),
            os: get_str(&m, "os"),
            platform: get_str(&m, "platform"),
            processing_time_fast_threshold: {
                let v = get_f64(&m, "processing-time-fast-threshold");
                if v > 0.0 { v } else { 0.1 }
            },
        }
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(dead_code)]
pub struct TubeStats {
    pub name: String,
    pub current_jobs_urgent: u64,
    pub current_jobs_ready: u64,
    pub current_jobs_reserved: u64,
    pub current_jobs_delayed: u64,
    /// aft: jobs waiting for their group to complete (tuber 0.17.0). Stored as
    /// delayed with no deadline but counted apart from `current_jobs_delayed`,
    /// and out of reach of `peek-delayed` and `kick`. 0 from older servers and
    /// beanstalkd, which do not send it.
    #[cfg_attr(feature = "serde", serde(default))]
    pub current_jobs_held: u64,
    pub current_jobs_buried: u64,
    pub total_jobs: u64,
    pub current_using: u64,
    pub current_watching: u64,
    pub current_waiting: u64,
    pub total_reserves: u64,
    pub total_timeouts: u64,
    pub total_buries: u64,
    pub oldest_ready_id: u64,
    pub oldest_ready_age: u64,
    pub oldest_delayed_age: u64,
    pub oldest_buried_age: u64,
    pub cmd_pause_tube: u64,
    pub pause: u64,
    pub pause_time_left: u64,
    pub bury_rate: f64,
    pub processing_time_ewma: f64,
    pub processing_time_min: f64,
    pub processing_time_max: f64,
    pub processing_time_samples: u64,
    pub processing_time_fast_threshold: f64,
    pub processing_time_ewma_fast: f64,
    pub processing_time_samples_fast: u64,
    pub processing_time_ewma_slow: f64,
    pub processing_time_samples_slow: u64,
    pub processing_time_p50: f64,
    pub processing_time_p95: f64,
    pub processing_time_p99: f64,
    pub queue_time_ewma: f64,
    pub queue_time_min: f64,
    pub queue_time_max: f64,
    pub queue_time_samples: u64,
    pub cmd_delete: u64,
    /// Puts whose `idp:` key matched a job already in the tube, by that job's
    /// state, so nothing was queued. A dedup against a **buried** job is the
    /// one to watch: nothing runs until that job is kicked, so a recurring
    /// put that keeps hitting it drops its work every time.
    pub total_idp_dedups_ready: u64,
    pub total_idp_dedups_reserved: u64,
    pub total_idp_dedups_delayed: u64,
    pub total_idp_dedups_buried: u64,
    /// Dedups against a cooldown tombstone (`idp:<key>:<ttl>`, job deleted).
    pub total_idp_dedups_deleted: u64,
}

impl TubeStats {
    pub fn current_total(&self) -> u64 {
        self.current_jobs_ready
            + self.current_jobs_reserved
            + self.current_jobs_delayed
            + self.current_jobs_held
            + self.current_jobs_buried
    }

    /// True when the tube is paused and the pause has not yet elapsed.
    pub fn is_paused(&self) -> bool {
        self.pause_time_left > 0
    }

    pub fn from_yaml(yaml: &str) -> Self {
        let m = parse_yaml_map(yaml);
        Self {
            name: get_str(&m, "name"),
            current_jobs_urgent: get_u64(&m, "current-jobs-urgent"),
            current_jobs_ready: get_u64(&m, "current-jobs-ready"),
            current_jobs_reserved: get_u64(&m, "current-jobs-reserved"),
            current_jobs_delayed: get_u64(&m, "current-jobs-delayed"),
            current_jobs_held: get_u64(&m, "current-jobs-held"),
            current_jobs_buried: get_u64(&m, "current-jobs-buried"),
            total_jobs: get_u64(&m, "total-jobs"),
            current_using: get_u64(&m, "current-using"),
            current_watching: get_u64(&m, "current-watching"),
            current_waiting: get_u64(&m, "current-waiting"),
            total_reserves: get_u64(&m, "total-reserves"),
            total_timeouts: get_u64(&m, "total-timeouts"),
            total_buries: get_u64(&m, "total-buries"),
            oldest_ready_id: get_u64(&m, "oldest-ready-id"),
            oldest_ready_age: get_u64(&m, "oldest-ready-age"),
            oldest_delayed_age: get_u64(&m, "oldest-delayed-age"),
            oldest_buried_age: get_u64(&m, "oldest-buried-age"),
            cmd_pause_tube: get_u64(&m, "cmd-pause-tube"),
            pause: get_u64(&m, "pause"),
            pause_time_left: get_u64(&m, "pause-time-left"),
            bury_rate: get_f64(&m, "bury-rate"),
            processing_time_ewma: get_f64(&m, "processing-time-ewma"),
            processing_time_min: get_f64(&m, "processing-time-min"),
            processing_time_max: get_f64(&m, "processing-time-max"),
            processing_time_samples: get_u64(&m, "processing-time-samples"),
            processing_time_fast_threshold: get_f64(&m, "processing-time-fast-threshold"),
            processing_time_ewma_fast: get_f64(&m, "processing-time-ewma-fast"),
            processing_time_samples_fast: get_u64(&m, "processing-time-samples-fast"),
            processing_time_ewma_slow: get_f64(&m, "processing-time-ewma-slow"),
            processing_time_samples_slow: get_u64(&m, "processing-time-samples-slow"),
            processing_time_p50: get_f64(&m, "processing-time-p50"),
            processing_time_p95: get_f64(&m, "processing-time-p95"),
            processing_time_p99: get_f64(&m, "processing-time-p99"),
            queue_time_ewma: get_f64(&m, "queue-time-ewma"),
            queue_time_min: get_f64(&m, "queue-time-min"),
            queue_time_max: get_f64(&m, "queue-time-max"),
            queue_time_samples: get_u64(&m, "queue-time-samples"),
            cmd_delete: get_u64(&m, "cmd-delete"),
            total_idp_dedups_ready: get_u64(&m, "total-idp-dedups-ready"),
            total_idp_dedups_reserved: get_u64(&m, "total-idp-dedups-reserved"),
            total_idp_dedups_delayed: get_u64(&m, "total-idp-dedups-delayed"),
            total_idp_dedups_buried: get_u64(&m, "total-idp-dedups-buried"),
            total_idp_dedups_deleted: get_u64(&m, "total-idp-dedups-deleted"),
        }
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(dead_code)]
pub struct JobStats {
    pub id: u64,
    pub tube: String,
    pub state: String,
    pub pri: u64,
    pub age: u64,
    pub delay: u64,
    pub ttr: u64,
    pub time_left: u64,
    pub time_reserved: u64,
    pub reserves: u64,
    pub timeouts: u64,
    pub releases: u64,
    pub buries: u64,
    pub kicks: u64,
    pub idempotency_key: String,
    pub idempotency_ttl: u64,
    pub group: String,
    pub after_group: String,
    pub concurrency_key: String,
    pub concurrency_limit: u64,
    pub file: u64,
}

impl JobStats {
    pub fn from_yaml(yaml: &str) -> Self {
        let m = parse_yaml_map(yaml);
        Self {
            id: get_u64(&m, "id"),
            tube: get_str(&m, "tube"),
            state: get_str(&m, "state"),
            pri: get_u64(&m, "pri"),
            age: get_u64(&m, "age"),
            delay: get_u64(&m, "delay"),
            ttr: get_u64(&m, "ttr"),
            time_left: get_u64(&m, "time-left"),
            time_reserved: get_u64(&m, "time-reserved"),
            reserves: get_u64(&m, "reserves"),
            timeouts: get_u64(&m, "timeouts"),
            releases: get_u64(&m, "releases"),
            buries: get_u64(&m, "buries"),
            kicks: get_u64(&m, "kicks"),
            idempotency_key: get_str(&m, "idempotency-key"),
            idempotency_ttl: get_u64(&m, "idempotency-ttl"),
            group: get_str(&m, "group"),
            after_group: get_str(&m, "after-group"),
            concurrency_key: get_str(&m, "concurrency-key"),
            concurrency_limit: get_u64(&m, "concurrency-limit"),
            file: get_u64(&m, "file"),
        }
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(dead_code)]
pub struct GroupStats {
    pub name: String,
    pub ready: u64,
    pub reserved: u64,
    pub delayed: u64,
    pub buried: u64,
    pub waiting_jobs: u64,
}

impl GroupStats {
    pub fn from_yaml(yaml: &str) -> Self {
        let m = parse_yaml_map(yaml);
        Self {
            name: get_str(&m, "name"),
            ready: get_u64(&m, "ready"),
            reserved: get_u64(&m, "reserved"),
            delayed: get_u64(&m, "delayed"),
            buried: get_u64(&m, "buried"),
            waiting_jobs: get_u64(&m, "waiting-jobs"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `stats-tube` output for a paused tube.
    const PAUSED_TUBE: &str = "---
name: \"active_job_goodreads\"
current-jobs-urgent: 0
current-jobs-ready: 2543
current-jobs-reserved: 0
current-jobs-delayed: 0
current-jobs-buried: 0
total-jobs: 30577
current-using: 0
current-watching: 18
current-waiting: 0
oldest-ready-id: 278276622
oldest-ready-age: 84940
oldest-delayed-age: 0
oldest-buried-age: 0
cmd-delete: 28034
cmd-pause-tube: 1
pause: 3600
pause-time-left: 3316
total-reserves: 28089
total-timeouts: 54
total-buries: 0
bury-rate: 0.000000
processing-time-ewma: 0.002992
processing-time-min: 0.000832
processing-time-max: 77.396080
processing-time-samples: 28034
processing-time-fast-threshold: 0.100000
processing-time-ewma-fast: 0.002625
processing-time-samples-fast: 27867
processing-time-ewma-slow: 6.209128
processing-time-samples-slow: 167
processing-time-p50: 0.006194
processing-time-p95: 0.223164
processing-time-p99: 0.469719
queue-time-ewma: 9621.553614
queue-time-min: 0.000008
queue-time-max: 52790.799139
queue-time-samples: 28089
";

    #[test]
    fn parses_pause_fields() {
        let t = TubeStats::from_yaml(PAUSED_TUBE);
        assert_eq!(t.name, "active_job_goodreads");
        assert_eq!(t.cmd_pause_tube, 1);
        assert_eq!(t.pause, 3600);
        assert_eq!(t.pause_time_left, 3316);
        assert!(t.is_paused());
    }

    #[test]
    fn not_paused_when_no_time_left() {
        // A tube that was paused earlier but has since resumed still reports
        // cmd-pause-tube and pause, but pause-time-left drops to 0.
        let yaml = "---\nname: \"x\"\ncmd-pause-tube: 4\npause: 60\npause-time-left: 0\n";
        let t = TubeStats::from_yaml(yaml);
        assert_eq!(t.pause, 60);
        assert!(!t.is_paused());
    }

    #[test]
    fn parses_timing_extremes_and_samples() {
        let t = TubeStats::from_yaml(PAUSED_TUBE);
        assert_eq!(t.processing_time_samples, 28034);
        assert_eq!(t.queue_time_samples, 28089);
        assert_eq!(t.bury_rate, 0.0);
        assert!((t.processing_time_min - 0.000832).abs() < 1e-9);
        assert!((t.processing_time_max - 77.396080).abs() < 1e-6);
        assert!((t.processing_time_fast_threshold - 0.1).abs() < 1e-9);
        assert!((t.queue_time_max - 52790.799139).abs() < 1e-6);
    }

    #[test]
    fn parses_idp_dedups_by_state() {
        let yaml = "---\nname: \"splat.maintenance\"\ntotal-idp-dedups-ready: 4\n\
                    total-idp-dedups-reserved: 3\ntotal-idp-dedups-delayed: 2\n\
                    total-idp-dedups-buried: 70\ntotal-idp-dedups-deleted: 1\n";
        let t = TubeStats::from_yaml(yaml);
        assert_eq!(t.total_idp_dedups_ready, 4);
        assert_eq!(t.total_idp_dedups_reserved, 3);
        assert_eq!(t.total_idp_dedups_delayed, 2);
        assert_eq!(t.total_idp_dedups_buried, 70);
        assert_eq!(t.total_idp_dedups_deleted, 1);
    }

    /// tuber 0.17.0's held count: aft: jobs waiting on their group. They are
    /// in no other count, so the total has to add them or a tube full of them
    /// reads as empty.
    #[test]
    fn parses_held_jobs_into_the_tube_total() {
        let yaml = "---\nname: \"q\"\ncurrent-jobs-ready: 2\ncurrent-jobs-delayed: 3\n\
                    current-jobs-held: 7\ncurrent-jobs-buried: 1\n";
        let t = TubeStats::from_yaml(yaml);
        assert_eq!(t.current_jobs_held, 7);
        assert_eq!(t.current_jobs_delayed, 3, "held is not part of delayed");
        assert_eq!(t.current_total(), 13);
    }

    #[test]
    fn parses_server_held_jobs() {
        let s = ServerStats::from_yaml("---\ncurrent-jobs-delayed: 3\ncurrent-jobs-held: 7\n");
        assert_eq!(s.current_jobs_held, 7);
        assert_eq!(s.current_jobs_delayed, 3);
    }

    /// Older tuber servers and beanstalkd do not send the field.
    #[test]
    fn held_defaults_to_zero_when_absent() {
        assert_eq!(TubeStats::from_yaml(PAUSED_TUBE).current_jobs_held, 0);
        assert_eq!(ServerStats::from_yaml("---\nversion: 1.13\n").current_jobs_held, 0);
    }

    #[test]
    fn parses_schedule_and_keepalive_server_fields() {
        let yaml = "---\ncurrent-jobs-size: 1000\nmax-jobs-size: 10000\n\
                    tcp-keepalive: 60\ncurrent-schedules: 9\ncurrent-schedules-size: 4600\n";
        let s = ServerStats::from_yaml(yaml);
        assert_eq!(s.current_schedules, 9);
        assert_eq!(s.current_schedules_size, 4600);
        assert_eq!(s.tcp_keepalive, 60);
        // Schedule entries are charged against max-jobs-size beside job bodies.
        assert_eq!(s.memory_in_use(), 5600);
    }

    /// After a WAL failure every binlog-* figure is 0, as without -b; only
    /// binlog-failed tells the two apart.
    #[test]
    fn parses_binlog_failed() {
        let failed = ServerStats::from_yaml("---\nbinlog-enabled: false\nbinlog-failed: true\n");
        assert!(!failed.binlog_enabled && failed.binlog_failed);
        let in_memory = ServerStats::from_yaml("---\nbinlog-enabled: false\nbinlog-failed: false\n");
        assert!(!in_memory.binlog_failed);
        // Servers before tuber 0.16.0 don't report it.
        assert!(!ServerStats::from_yaml("---\nbinlog-enabled: true\n").binlog_failed);
    }

    #[test]
    fn missing_pause_fields_default_to_unpaused() {
        // beanstalkd and older tuber builds may omit these entirely.
        let t = TubeStats::from_yaml("---\nname: \"x\"\ncurrent-jobs-ready: 1\n");
        assert_eq!(t.pause, 0);
        assert_eq!(t.pause_time_left, 0);
        assert!(!t.is_paused());
    }
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Snapshot {
    pub server: ServerStats,
    pub tubes: Vec<TubeStats>,
    #[cfg_attr(feature = "serde", serde(skip, default = "Instant::now"))]
    pub fetched_at: Instant,
}
