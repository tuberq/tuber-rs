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
    pub current_jobs_ready: u64,
    pub current_jobs_reserved: u64,
    pub current_jobs_delayed: u64,
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
    pub binlog_enabled: bool,
    pub binlog_total_bytes: u64,
    pub binlog_file_count: u64,
    pub binlog_current_index: u64,
    pub binlog_oldest_index: u64,
    pub name: String,
    pub hostname: String,
    pub os: String,
    pub platform: String,
    pub processing_time_fast_threshold: f64,
}

impl ServerStats {
    pub fn from_yaml(yaml: &str) -> Self {
        let m = parse_yaml_map(yaml);
        Self {
            version: get_str(&m, "version"),
            uptime: get_u64(&m, "uptime"),
            current_connections: get_u64(&m, "current-connections"),
            current_producers: get_u64(&m, "current-producers"),
            current_workers: get_u64(&m, "current-workers"),
            current_waiting: get_u64(&m, "current-waiting"),
            current_jobs_ready: get_u64(&m, "current-jobs-ready"),
            current_jobs_reserved: get_u64(&m, "current-jobs-reserved"),
            current_jobs_delayed: get_u64(&m, "current-jobs-delayed"),
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
            binlog_enabled: get_bool(&m, "binlog-enabled"),
            binlog_total_bytes: get_u64(&m, "binlog-total-bytes"),
            binlog_file_count: get_u64(&m, "binlog-file-count"),
            binlog_current_index: get_u64(&m, "binlog-current-index"),
            binlog_oldest_index: get_u64(&m, "binlog-oldest-index"),
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
    pub current_jobs_ready: u64,
    pub current_jobs_reserved: u64,
    pub current_jobs_delayed: u64,
    pub current_jobs_buried: u64,
    pub total_jobs: u64,
    pub total_reserves: u64,
    pub total_timeouts: u64,
    pub processing_time_ewma: f64,
    pub processing_time_ewma_fast: f64,
    pub processing_time_samples_fast: u64,
    pub processing_time_ewma_slow: f64,
    pub processing_time_samples_slow: u64,
    pub processing_time_p50: f64,
    pub processing_time_p95: f64,
    pub processing_time_p99: f64,
    pub queue_time_ewma: f64,
    pub cmd_delete: u64,
}

impl TubeStats {
    pub fn current_total(&self) -> u64 {
        self.current_jobs_ready + self.current_jobs_reserved + self.current_jobs_delayed + self.current_jobs_buried
    }

    pub fn from_yaml(yaml: &str) -> Self {
        let m = parse_yaml_map(yaml);
        Self {
            name: get_str(&m, "name"),
            current_jobs_ready: get_u64(&m, "current-jobs-ready"),
            current_jobs_reserved: get_u64(&m, "current-jobs-reserved"),
            current_jobs_delayed: get_u64(&m, "current-jobs-delayed"),
            current_jobs_buried: get_u64(&m, "current-jobs-buried"),
            total_jobs: get_u64(&m, "total-jobs"),
            total_reserves: get_u64(&m, "cmd-reserve-with-timeout"),
            total_timeouts: get_u64(&m, "total-timeouts"),
            processing_time_ewma: get_f64(&m, "processing-time-ewma"),
            processing_time_ewma_fast: get_f64(&m, "processing-time-ewma-fast"),
            processing_time_samples_fast: get_u64(&m, "processing-time-samples-fast"),
            processing_time_ewma_slow: get_f64(&m, "processing-time-ewma-slow"),
            processing_time_samples_slow: get_u64(&m, "processing-time-samples-slow"),
            processing_time_p50: get_f64(&m, "processing-time-p50"),
            processing_time_p95: get_f64(&m, "processing-time-p95"),
            processing_time_p99: get_f64(&m, "processing-time-p99"),
            queue_time_ewma: get_f64(&m, "queue-time-ewma"),
            cmd_delete: get_u64(&m, "cmd-delete"),
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
    pub pending: u64,
    pub buried: u64,
    pub complete: bool,
    pub waiting_jobs: u64,
}

impl GroupStats {
    pub fn from_yaml(yaml: &str) -> Self {
        let m = parse_yaml_map(yaml);
        Self {
            name: get_str(&m, "name"),
            pending: get_u64(&m, "pending"),
            buried: get_u64(&m, "buried"),
            complete: get_bool(&m, "complete"),
            waiting_jobs: get_u64(&m, "waiting-jobs"),
        }
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
