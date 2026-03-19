use crate::parse::{get_bool, get_f64, get_str, get_u64, parse_yaml_map};
use std::time::Instant;

#[derive(Clone, Debug)]
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
    pub cmd_delete: u64,
    pub job_timeouts: u64,
    pub total_jobs: u64,
    pub rusage_utime: f64,
    pub rusage_stime: f64,
    pub draining: bool,
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
            cmd_delete: get_u64(&m, "cmd-delete"),
            job_timeouts: get_u64(&m, "job-timeouts"),
            total_jobs: get_u64(&m, "total-jobs"),
            rusage_utime: get_f64(&m, "rusage-utime"),
            rusage_stime: get_f64(&m, "rusage-stime"),
            draining: get_bool(&m, "draining"),
        }
    }
}

#[derive(Clone, Debug)]
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
    pub cmd_delete: u64,
}

impl TubeStats {
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
            cmd_delete: get_u64(&m, "cmd-delete"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub server: ServerStats,
    pub tubes: Vec<TubeStats>,
    pub fetched_at: Instant,
}
