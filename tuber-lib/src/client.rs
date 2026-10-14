use crate::error::{Error, Result};
use crate::model::{GroupStats, JobStats, ServerStats, Snapshot, TubeStats};
use crate::name::{check_name, check_tag};
use crate::parse::parse_yaml_list;
use crate::schedule::{self, ScheduleEntry, ScheduleStats, ScheduleSyncError, SyncResult};
use std::future::Future;
use std::io;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// How long [`TuberClient::connect`] waits to connect, and then for each reply.
/// A reserve that asks the server to wait gets that long on top.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Outcome of a `put`. When `existing_state` is `Some`, an `idp:` key matched
/// a job that was already there: nothing was enqueued, `id` is that job's id,
/// and the state is where it currently sits (`READY`, `BURIED`, `DELETED`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PutResult {
    pub id: u64,
    pub existing_state: Option<String>,
    /// When an `idp:` key matched a job less urgent than this put: that job's
    /// priority, raised to this put's. `None` when nothing was raised.
    pub raised_pri: Option<u32>,
    /// The server buried the new job on arrival, as beanstalkd does when it
    /// is out of memory (`BURIED <id>`). The job exists, and runs once kicked.
    pub buried: bool,
}

impl PutResult {
    /// True when the put was deduplicated against an existing job.
    pub fn is_duplicate(&self) -> bool {
        self.existing_state.is_some()
    }
}

/// Parse a `put` reply. `INSERTED <id>` is a fresh insert; a trailing state
/// (`INSERTED <id> <state> [<pri>]`) means an `idp:` key matched an existing
/// job and nothing was enqueued. `BURIED <id>` is a fresh insert buried on
/// arrival. `None` for any other reply.
fn parse_inserted(line: &str) -> Option<PutResult> {
    let (buried, rest) = match line.split_once(' ')? {
        ("INSERTED", rest) => (false, rest),
        ("BURIED", rest) => (true, rest),
        _ => return None,
    };
    let mut fields = rest.split_whitespace();
    let id: u64 = fields.next()?.parse().ok()?;
    if buried {
        return Some(PutResult { id, existing_state: None, raised_pri: None, buried });
    }
    let existing_state = fields.next().map(str::to_string);
    let raised_pri = match fields.next() {
        Some(pri) => Some(pri.parse().ok()?),
        None => None,
    };
    Some(PutResult { id, existing_state, raised_pri, buried })
}

/// A reserved or peeked job. The body is bytes, exactly as put: tuber doesn't
/// require text, so neither does this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: u64,
    pub body: Vec<u8>,
}

impl Job {
    /// The body as text, if it is UTF-8.
    pub fn body_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.body).ok()
    }
}

/// What a reserve came back with. Only [`Job`](Self::Job) holds a
/// reservation; the others are answers, not failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reserved {
    Job(Job),
    /// `TIMED_OUT`: no job became ready within the timeout.
    TimedOut,
    /// `DEADLINE_SOON`: a job this connection holds is within a second of
    /// its TTR. Nothing was reserved: finish, touch or release that job first.
    DeadlineSoon,
}

impl Reserved {
    /// The job, if one was reserved.
    pub fn job(self) -> Option<Job> {
        match self {
            Self::Job(job) => Some(job),
            _ => None,
        }
    }
}

/// How `reserve` chooses among the watched tubes, for this connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReserveMode {
    /// The most urgent job across every watched tube. The default.
    Fifo,
    /// A tube chosen at random in proportion to its watch weight, then its
    /// most urgent job.
    Weighted,
    /// As `Weighted`, but sharing worker time rather than job count: each
    /// tube's weight is divided by how long its jobs take to run.
    WeightedFair,
}

impl ReserveMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fifo => "fifo",
            Self::Weighted => "weighted",
            Self::WeightedFair => "weighted-fair",
        }
    }
}

/// The longest body a reply may announce: tuber's ceiling on a job's size,
/// which `-z` can't raise. A longer one is a protocol error rather than an
/// allocation.
const MAX_BODY: usize = 1 << 30;

/// The largest weight [`TuberClient::watch_weighted`] takes.
pub const MAX_WATCH_WEIGHT: u32 = 9999;

/// One connection to tuber.
///
/// Errors are [`Error`]s: tuber's refusals leave the connection usable, and
/// an I/O, timeout or framing error closes it (see [`Error::is_fatal`]).
pub struct TuberClient {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
    /// Set by a fatal error. The socket has been shut, and every call fails
    /// with [`Error::Closed`] without sending anything.
    closed: bool,
    /// How long a command may take; `None` waits as long as it takes.
    timeout: Option<Duration>,
    /// When the command in flight gives up, set as it is sent.
    deadline: Option<tokio::time::Instant>,
}

impl TuberClient {
    /// Connect, waiting at most [`DEFAULT_TIMEOUT`] to connect and then for
    /// each reply.
    pub async fn connect(addr: &str) -> Result<Self> {
        Self::connect_with_timeout(addr, Some(DEFAULT_TIMEOUT)).await
    }

    /// Connect, waiting at most `timeout` to connect and then for each reply;
    /// `None` waits as long as it takes.
    pub async fn connect_with_timeout(addr: &str, timeout: Option<Duration>) -> Result<Self> {
        let deadline = timeout.map(|t| tokio::time::Instant::now() + t);
        let stream = match within(deadline, TcpStream::connect(addr)).await {
            Some(stream) => stream.map_err(Error::Io)?,
            None => return Err(Error::Timeout),
        };
        stream.set_nodelay(true).map_err(Error::Io)?;
        let (reader, writer) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(reader),
            writer,
            closed: false,
            timeout,
            deadline: None,
        })
    }

    /// Change how long each later command may take; `None` waits as long as
    /// it takes. A reply that misses its deadline is [`Error::Timeout`], and
    /// closes the connection.
    pub fn set_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout;
    }

    pub async fn stats(&mut self) -> Result<ServerStats> {
        self.send_line("stats").await?;
        let body = self.read_ok_body().await?;
        Ok(ServerStats::from_yaml(&body))
    }

    pub async fn list_tubes(&mut self) -> Result<Vec<String>> {
        self.send_line("list-tubes").await?;
        let body = self.read_ok_body().await?;
        Ok(parse_yaml_list(&body))
    }

    pub async fn stats_tube(&mut self, tube: &str) -> Result<TubeStats> {
        check_name("tube name", tube)?;
        self.send_line(&format!("stats-tube {tube}")).await?;
        let body = self.read_ok_body().await?;
        Ok(TubeStats::from_yaml(&body))
    }

    pub async fn fetch_snapshot(&mut self) -> Result<Snapshot> {
        let server = self.stats().await?;
        let tube_names = self.list_tubes().await?;
        let mut tubes = Vec::with_capacity(tube_names.len());
        for name in &tube_names {
            // Only a laxer server could list a name this client won't send,
            // and there is no asking about it.
            if check_name("tube name", name).is_err() {
                continue;
            }
            match self.stats_tube(name).await {
                Ok(tube) => tubes.push(tube),
                // Emptied, and so gone, since list-tubes named it.
                Err(Error::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        // Busiest first, so a caller that takes the head of the list gets the
        // tubes worth looking at. Callers that want another order re-sort.
        tubes.sort_by_key(|t| std::cmp::Reverse(t.total_jobs));
        Ok(Snapshot {
            server,
            tubes,
            fetched_at: Instant::now(),
        })
    }

    /// Switch to a tube for subsequent put commands.
    /// Sends: `use <tube>\r\n` — expects: `USING <tube>\r\n`
    pub async fn use_tube(&mut self, tube: &str) -> Result<String> {
        check_name("tube name", tube)?;
        self.send_line(&format!("use {tube}")).await?;
        let line = self.read_line().await?;
        match line.strip_prefix("USING ") {
            Some(name) => Ok(name.to_string()),
            None => Err(self.refusal(line).await),
        }
    }

    /// Put a job into the currently used tube.
    /// Sends: `put <pri> <delay> <ttr> <bytes>\r\n<data>\r\n` — expects: `INSERTED <id>\r\n`
    pub async fn put(&mut self, priority: u32, delay: u32, ttr: u32, data: &[u8]) -> Result<u64> {
        Ok(self.put_tagged(priority, delay, ttr, data, &[]).await?.id)
    }

    /// Put a job with tuber extension tags appended to the `put` line, e.g.
    /// `idp:<key>[:<ttl>]` (idempotent put), `grp:<name>` (job group),
    /// `aft:<group>` (run after a group completes), `con:<key>` (concurrency
    /// limit). Tags are passed through verbatim, but one that is empty or holds
    /// whitespace is refused before anything is sent, since it would split the
    /// line.
    ///
    /// Note on `idp:` keys — the LAST colon separates key from TTL, so
    /// `idp:series:123` means key `series` with a 123s TTL, not key
    /// `series:123`. Use dashes or dots in keys instead.
    ///
    /// Sends: `put <pri> <delay> <ttr> <bytes> [tags...]\r\n<data>\r\n`
    ///
    /// Expects `INSERTED <id>\r\n` for a fresh job, or `INSERTED <id> <state>
    /// [<pri>]\r\n` when an `idp:` key matched — in which case nothing was
    /// enqueued and the id is the *existing* job's. See [`PutResult`].
    ///
    /// [`Error::InternalError`] means the job wasn't queued, but may come back
    /// after a tuber restart: retry with an `idp:` key.
    pub async fn put_tagged(
        &mut self,
        priority: u32,
        delay: u32,
        ttr: u32,
        data: &[u8],
        tags: &[String],
    ) -> Result<PutResult> {
        for tag in tags {
            check_tag(tag)?;
        }
        let mut cmd = format!("put {} {} {} {}", priority, delay, ttr, data.len());
        for tag in tags {
            cmd.push(' ');
            cmd.push_str(tag);
        }
        cmd.push_str("\r\n");
        // One buffer, so the job goes out in one write.
        let mut frame = cmd.into_bytes();
        frame.extend_from_slice(data);
        frame.extend_from_slice(b"\r\n");
        self.send(&frame).await?;
        let line = self.read_line().await?;
        match parse_inserted(&line) {
            Some(put) => Ok(put),
            None => Err(self.refusal(line).await),
        }
    }

    /// Reserve the next job from the watched tubes, waiting as long as it
    /// takes. Only [`Reserved::DeadlineSoon`] ends the wait without a job.
    /// Sends: `reserve\r\n` — expects: `RESERVED <id> <bytes>\r\n<data>\r\n`
    pub async fn reserve(&mut self) -> Result<Reserved> {
        self.send_line("reserve").await?;
        // No deadline: the reply comes when a job does.
        self.deadline = None;
        self.read_reserved().await
    }

    /// Reserve the next job from the watched tubes, waiting up to `timeout`
    /// seconds; 0 returns at once.
    /// Sends: `reserve-with-timeout <seconds>\r\n` — expects:
    /// `RESERVED <id> <bytes>\r\n<data>\r\n`, `TIMED_OUT` or `DEADLINE_SOON`
    pub async fn reserve_with_timeout(&mut self, timeout: u32) -> Result<Reserved> {
        self.send_line(&format!("reserve-with-timeout {timeout}")).await?;
        self.wait_longer(timeout);
        self.read_reserved().await
    }

    /// Reserve one specific job by ID, regardless of tube or watch list. The
    /// job must be ready, delayed, or buried — not already reserved.
    /// Sends: `reserve-job <id>\r\n` — expects: `RESERVED <id> <bytes>\r\n<data>\r\n`
    pub async fn reserve_job(&mut self, id: u64) -> Result<Job> {
        self.send_line(&format!("reserve-job {id}")).await?;
        self.read_job_response("RESERVED").await
    }

    /// Reserve up to `count` jobs in a single command. With `timeout = Some(n)`
    /// where `n > 0`, blocks up to `n` seconds for at least one job to appear;
    /// `None` or `Some(0)` returns immediately (possibly empty). Reserves from
    /// the watch list, so use [`watch_only`](Self::watch_only) to target a tube.
    /// [`Error::DeadlineSoon`] means a job already held is about to run out of
    /// TTR: [`touch_all`](Self::touch_all), then reserve again.
    /// Sends: `reserve-batch <count> [timeout]\r\n`
    /// Expects: `RESERVED_BATCH <n>\r\n` then n × `RESERVED <id> <bytes>\r\n<data>\r\n`
    pub async fn reserve_batch(&mut self, count: u32, timeout: Option<u32>) -> Result<Vec<Job>> {
        let cmd = match timeout {
            Some(t) => format!("reserve-batch {count} {t}"),
            None => format!("reserve-batch {count}"),
        };
        self.send_line(&cmd).await?;
        self.wait_longer(timeout.unwrap_or(0));
        let line = self.read_line().await?;
        if line == "DEADLINE_SOON" {
            return Err(Error::DeadlineSoon);
        }
        let Some(n) = line.strip_prefix("RESERVED_BATCH ") else {
            return Err(self.refusal(line).await);
        };
        let n: usize = self.parse_field(n, &line).await?;
        let mut jobs = Vec::with_capacity(n);
        for _ in 0..n {
            // Every job is read, whatever its body: one left on the wire
            // would be read as the next command's reply.
            let line = self.read_line().await?;
            jobs.push(self.read_job_body("RESERVED", &line).await?);
        }
        Ok(jobs)
    }

    /// Add a tube to the watch set for subsequent reserves.
    /// Sends: `watch <tube>\r\n` — expects: `WATCHING <count>\r\n`
    pub async fn watch(&mut self, tube: &str) -> Result<u64> {
        check_name("tube name", tube)?;
        self.send_line(&format!("watch {tube}")).await?;
        self.read_u64_response("WATCHING").await
    }

    /// Watch a tube with a weight, for the weighted [`ReserveMode`]s: a tube
    /// of weight 6 is chosen three times as often as one of weight 2. Tubes
    /// watched without one weigh 1. Weights run 1 to [`MAX_WATCH_WEIGHT`].
    /// Sends: `watch <tube> <weight>\r\n` — expects: `WATCHING <count>\r\n`
    pub async fn watch_weighted(&mut self, tube: &str, weight: u32) -> Result<u64> {
        check_name("tube name", tube)?;
        if !(1..=MAX_WATCH_WEIGHT).contains(&weight) {
            return Err(Error::InvalidInput(format!(
                "watch weight {weight} is outside 1 to {MAX_WATCH_WEIGHT}"
            )));
        }
        self.send_line(&format!("watch {tube} {weight}")).await?;
        self.read_u64_response("WATCHING").await
    }

    /// The tubes this connection watches.
    /// Sends: `list-tubes-watched\r\n` — expects: `OK <bytes>\r\n<yaml>\r\n`
    pub async fn list_tubes_watched(&mut self) -> Result<Vec<String>> {
        self.send_line("list-tubes-watched").await?;
        let body = self.read_ok_body().await?;
        Ok(parse_yaml_list(&body))
    }

    /// The tube this connection puts into.
    /// Sends: `list-tube-used\r\n` — expects: `USING <tube>\r\n`
    pub async fn list_tube_used(&mut self) -> Result<String> {
        self.send_line("list-tube-used").await?;
        let line = self.read_line().await?;
        match line.strip_prefix("USING ") {
            Some(name) => Ok(name.to_string()),
            None => Err(self.refusal(line).await),
        }
    }

    /// Choose how this connection's reserves pick among its watched tubes.
    /// Sends: `reserve-mode <mode>\r\n` — expects: `USING <mode>\r\n`
    pub async fn reserve_mode(&mut self, mode: ReserveMode) -> Result<()> {
        self.send_line(&format!("reserve-mode {}", mode.as_str())).await?;
        self.expect_response(&format!("USING {}", mode.as_str())).await
    }

    /// Remove a tube from the watch set.
    /// Sends: `ignore <tube>\r\n` — expects: `WATCHING <count>\r\n`
    /// ([`Error::NotIgnored`] if it was the last)
    pub async fn ignore(&mut self, tube: &str) -> Result<u64> {
        check_name("tube name", tube)?;
        self.send_line(&format!("ignore {tube}")).await?;
        self.read_u64_response("WATCHING").await
    }

    /// Watch only `tube` for subsequent reserves: watch it, then ignore every
    /// other tube the connection watches, `default` included. `reserve` and
    /// `reserve-batch` pull from the watch list — not the used tube — so this
    /// is how you target a specific tube for reserving.
    pub async fn watch_only(&mut self, tube: &str) -> Result<()> {
        self.watch(tube).await?;
        for other in self.list_tubes_watched().await? {
            if other != tube {
                self.ignore(&other).await?;
            }
        }
        Ok(())
    }

    /// Delete a job by ID.
    /// Sends: `delete <id>\r\n` — expects: `DELETED\r\n`
    pub async fn delete(&mut self, id: u64) -> Result<()> {
        self.send_line(&format!("delete {id}")).await?;
        self.expect_response("DELETED").await
    }

    /// Kick at most `bound` buried or delayed jobs in the currently used tube.
    /// Sends: `kick <bound>\r\n` — expects: `KICKED <count>\r\n`
    pub async fn kick(&mut self, bound: u32) -> Result<u64> {
        self.send_line(&format!("kick {bound}")).await?;
        self.read_u64_response("KICKED").await
    }

    /// Kick one specific buried or delayed job back to ready.
    /// Sends: `kick-job <id>\r\n` — expects: `KICKED\r\n`
    pub async fn kick_job(&mut self, id: u64) -> Result<()> {
        self.send_line(&format!("kick-job {id}")).await?;
        self.expect_response("KICKED").await
    }

    /// Enter drain mode: new `put`s are rejected with `DRAINING` while
    /// in-flight work is allowed to finish. Server-wide and persistent until
    /// [`undrain`](Self::undrain). Also triggered by `SIGUSR1`.
    /// Sends: `drain\r\n` — expects: `DRAINING\r\n`
    pub async fn drain(&mut self) -> Result<()> {
        self.send_line("drain").await?;
        self.expect_response("DRAINING").await
    }

    /// Leave drain mode and resume accepting `put`s. Idempotent.
    /// Sends: `undrain\r\n` — expects: `NOT_DRAINING\r\n`
    pub async fn undrain(&mut self) -> Result<()> {
        self.send_line("undrain").await?;
        self.expect_response("NOT_DRAINING").await
    }

    /// Peek at a job by ID without reserving it.
    /// Sends: `peek <id>\r\n` — expects: `FOUND <id> <bytes>\r\n<data>\r\n`
    pub async fn peek(&mut self, id: u64) -> Result<Job> {
        self.send_line(&format!("peek {id}")).await?;
        self.read_job_response("FOUND").await
    }

    /// Peek at the next ready job in the currently used tube.
    pub async fn peek_ready(&mut self) -> Result<Job> {
        self.send_line("peek-ready").await?;
        self.read_job_response("FOUND").await
    }

    /// Peek at the next buried job in the currently used tube.
    pub async fn peek_buried(&mut self) -> Result<Job> {
        self.send_line("peek-buried").await?;
        self.read_job_response("FOUND").await
    }

    /// Peek at the next delayed job in the currently used tube.
    pub async fn peek_delayed(&mut self) -> Result<Job> {
        self.send_line("peek-delayed").await?;
        self.read_job_response("FOUND").await
    }

    /// Peek at a job this connection holds reserved.
    pub async fn peek_reserved(&mut self) -> Result<Job> {
        self.send_line("peek-reserved").await?;
        self.read_job_response("FOUND").await
    }

    /// Show statistics for a job by ID.
    /// Sends: `stats-job <id>\r\n` — expects: `OK <bytes>\r\n<yaml>\r\n`
    pub async fn stats_job(&mut self, id: u64) -> Result<JobStats> {
        self.send_line(&format!("stats-job {id}")).await?;
        let body = self.read_ok_body().await?;
        Ok(JobStats::from_yaml(&body))
    }

    /// Show statistics for a job group.
    /// Sends: `stats-group <name>\r\n` — expects: `OK <bytes>\r\n<yaml>\r\n`
    pub async fn stats_group(&mut self, name: &str) -> Result<GroupStats> {
        check_name("group name", name)?;
        self.send_line(&format!("stats-group {name}")).await?;
        let body = self.read_ok_body().await?;
        Ok(GroupStats::from_yaml(&body))
    }

    /// Flush all jobs from a tube.
    /// Sends: `flush-tube <tube>\r\n` — expects: `FLUSHED <count>\r\n`
    pub async fn flush_tube(&mut self, tube: &str) -> Result<u64> {
        check_name("tube name", tube)?;
        self.send_line(&format!("flush-tube {tube}")).await?;
        self.read_u64_response("FLUSHED").await
    }

    /// Flush only the buried jobs from a tube; ready, delayed, and reserved
    /// jobs are left untouched.
    /// Sends: `flush-buried <tube>\r\n` — expects: `FLUSHED <count>\r\n`
    pub async fn flush_buried(&mut self, tube: &str) -> Result<u64> {
        check_name("tube name", tube)?;
        self.send_line(&format!("flush-buried {tube}")).await?;
        self.read_u64_response("FLUSHED").await
    }

    /// Delete multiple jobs in a single command.
    /// Sends: `delete-batch <id1> <id2> ...\r\n` — expects: `DELETED_BATCH <deleted> <not_found>\r\n`
    pub async fn delete_batch(&mut self, ids: &[u64]) -> Result<(u64, u64)> {
        if ids.is_empty() {
            return Err(Error::InvalidInput("delete-batch needs at least one id".into()));
        }
        let id_strs: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        self.send_line(&format!("delete-batch {}", id_strs.join(" "))).await?;
        let line = self.read_line().await?;
        let Some(rest) = line.strip_prefix("DELETED_BATCH ") else {
            return Err(self.refusal(line).await);
        };
        match rest.split_once(' ') {
            Some((deleted, not_found)) => {
                let deleted = self.parse_field(deleted, &line).await?;
                let not_found = self.parse_field(not_found, &line).await?;
                Ok((deleted, not_found))
            }
            None => Err(self.protocol(format!("unexpected reply {line:?}")).await),
        }
    }

    /// Put a job this connection holds back in its tube, to run again, at
    /// `priority` and after `delay` seconds: how a worker gives up on a job
    /// for now without burying it. [`Error::NotFound`] if this connection
    /// doesn't hold it. [`Error::OutOfMemory`] when beanstalkd, out of
    /// memory, buried the job instead.
    /// Sends: `release <id> <pri> <delay>\r\n` — expects: `RELEASED\r\n`
    pub async fn release(&mut self, id: u64, priority: u32, delay: u32) -> Result<()> {
        self.send_line(&format!("release {id} {priority} {delay}")).await?;
        let line = self.read_line().await?;
        match line.as_str() {
            "RELEASED" => Ok(()),
            "BURIED" => Err(Error::OutOfMemory),
            _ => Err(self.refusal(line).await),
        }
    }

    /// Bury a reserved job.
    /// Sends: `bury <id> <priority>\r\n` — expects: `BURIED\r\n`
    pub async fn bury(&mut self, id: u64, priority: u32) -> Result<()> {
        self.send_line(&format!("bury {id} {priority}")).await?;
        self.expect_response("BURIED").await
    }

    /// Reset the TTR timer on a single reserved job.
    /// Sends: `touch <id>\r\n` — expects: `TOUCHED\r\n`
    pub async fn touch(&mut self, id: u64) -> Result<()> {
        self.send_line(&format!("touch {id}")).await?;
        self.expect_response("TOUCHED").await
    }

    /// Extend the TTR deadline of every job this connection currently holds,
    /// and return how many that was. Takes no ids — jobs already deleted,
    /// released, buried, or lost to TTR simply aren't in the reserved set.
    /// Each job keeps its own TTR; deadlines move individually.
    ///
    /// This is the heartbeat for [`reserve_batch`](Self::reserve_batch)
    /// workers: TTR starts on all N jobs at once but they're processed
    /// serially, so long batches need this or the tail expires. A count lower
    /// than expected means jobs timed out and went back to the queue — nothing
    /// else reports that.
    ///
    /// Sends: `touch-all\r\n` — expects: `TOUCHED_ALL <n>\r\n`
    pub async fn touch_all(&mut self) -> Result<u64> {
        self.send_line("touch-all").await?;
        self.read_u64_response("TOUCHED_ALL").await
    }

    /// Pause a tube for `delay` seconds.
    /// Sends: `pause-tube <tube> <delay>\r\n` — expects: `PAUSED\r\n`
    pub async fn pause_tube(&mut self, tube: &str, delay: u32) -> Result<()> {
        check_name("tube name", tube)?;
        self.send_line(&format!("pause-tube {tube} {delay}")).await?;
        self.expect_response("PAUSED").await
    }

    /// The recurring schedule entries held, as `(owner, name)` pairs: all of
    /// them, or `owner`'s.
    /// Sends: `list-schedules [<owner>]\r\n` — expects: `OK <bytes>\r\n<yaml>\r\n`
    pub async fn list_schedules(&mut self, owner: Option<&str>) -> Result<Vec<(String, String)>> {
        match owner {
            Some(owner) => {
                check_name("schedule owner", owner)?;
                self.send_line(&format!("list-schedules {owner}")).await?;
            }
            None => self.send_line("list-schedules").await?,
        }
        let body = self.read_ok_body().await?;
        Ok(parse_yaml_list(&body)
            .iter()
            .filter_map(|l| l.split_once(' '))
            .map(|(owner, name)| (owner.to_string(), name.to_string()))
            .collect())
    }

    /// One schedule entry's definition, next fires, last fire and counters.
    /// Sends: `stats-schedule <owner> <name>\r\n` — expects: `OK <bytes>\r\n<yaml>\r\n`
    /// ([`Error::NotFound`] for no such entry)
    pub async fn stats_schedule(&mut self, owner: &str, name: &str) -> Result<ScheduleStats> {
        check_name("schedule owner", owner)?;
        check_name("schedule name", name)?;
        self.send_line(&format!("stats-schedule {owner} {name}")).await?;
        let body = self.read_ok_body().await?;
        Ok(ScheduleStats::from_yaml(&body))
    }

    /// Replace `owner`'s recurring schedule with `entries`, in one step.
    ///
    /// - **Always the complete set.** On commit the owner's entries become
    ///   exactly these: one not sent is deleted.
    /// - **An empty set deletes them all**, so it is refused unless
    ///   `allow_empty` is set. An app whose config failed to load must not
    ///   wipe its schedule.
    /// - **Sync at boot, not on reconnect.** During a rolling deploy an old
    ///   process re-syncing would put back what the new release removed.
    ///
    /// Every entry is checked before anything is sent. If tuber refuses one,
    /// the sync is aborted and the error names it; the owner's previous set
    /// keeps running. If the connection drops partway, tuber discards what was
    /// staged: re-run the whole sync on a new connection, which is safe, since
    /// an identical set changes nothing. Never resume one partway.
    ///
    /// A refusal from tuber comes back as [`Error::ScheduleSync`]. Nothing
    /// changed unless its [`is_live`](ScheduleSyncError::is_live) says the
    /// commit took but couldn't be written to disk.
    ///
    /// Sends: `schedule-begin <owner>`, a `schedule-put` per entry framed like
    /// `put`, then `schedule-commit` — expects: `SYNCED <added> <changed>
    /// <removed> <unchanged>\r\n`
    pub async fn sync_schedules(
        &mut self,
        owner: &str,
        entries: &[ScheduleEntry],
        allow_empty: bool,
    ) -> Result<SyncResult> {
        schedule::validate_sync(owner, entries, allow_empty)?;
        let refused = |entry: Option<&str>, status: String| {
            Error::ScheduleSync(ScheduleSyncError {
                owner: owner.to_string(),
                entry: entry.map(str::to_string),
                status,
            })
        };

        self.send_line(&format!("schedule-begin {owner}")).await?;
        let line = self.read_line().await?;
        if line != format!("SYNCING {owner}") {
            return Err(refused(None, line));
        }

        for entry in entries {
            self.send(&schedule::frame(entry)).await?;
            let line = self.read_line().await?;
            if line != "STAGED" {
                // The refusal has already doomed the commit; abort now rather
                // than leave the staged set open on a connection the caller
                // may go on using. Best effort: a connection that fails here
                // has discarded the staged set anyway, and the refusal is
                // what the caller needs to hear.
                if self.send_line("schedule-abort").await.is_ok() {
                    let _ = self.read_line().await;
                }
                return Err(refused(Some(&entry.name), line));
            }
        }

        self.send_line("schedule-commit").await?;
        let line = self.read_line().await?;
        SyncResult::parse(&line).ok_or_else(|| refused(None, line))
    }

    /// Expect a single-word response (e.g. "DELETED", "BURIED", "PAUSED").
    async fn expect_response(&mut self, expected: &str) -> Result<()> {
        let line = self.read_line().await?;
        if line == expected {
            Ok(())
        } else {
            Err(self.refusal(line).await)
        }
    }

    /// Read a response with a prefix and trailing u64 (e.g. "KICKED 5").
    async fn read_u64_response(&mut self, prefix: &str) -> Result<u64> {
        let line = self.read_line().await?;
        match line.strip_prefix(prefix).and_then(|s| s.strip_prefix(' ')) {
            Some(n) => self.parse_field(n, &line).await,
            None => Err(self.refusal(line).await),
        }
    }

    /// Read a reserve's reply: a job, or the reason there isn't one.
    async fn read_reserved(&mut self) -> Result<Reserved> {
        let line = self.read_line().await?;
        match line.as_str() {
            "TIMED_OUT" => Ok(Reserved::TimedOut),
            "DEADLINE_SOON" => Ok(Reserved::DeadlineSoon),
            _ => Ok(Reserved::Job(self.read_job_body("RESERVED", &line).await?)),
        }
    }

    /// Read a response with a job body (RESERVED/FOUND): `<PREFIX> <id> <bytes>\r\n<data>\r\n`
    async fn read_job_response(&mut self, prefix: &str) -> Result<Job> {
        let line = self.read_line().await?;
        self.read_job_body(prefix, &line).await
    }

    /// Read the body that `line`, a `<PREFIX> <id> <bytes>` header, announces.
    /// Any other line is the command's refusal.
    async fn read_job_body(&mut self, prefix: &str, line: &str) -> Result<Job> {
        let Some(header) = line.strip_prefix(prefix).and_then(|s| s.strip_prefix(' ')) else {
            return Err(self.refusal(line.to_string()).await);
        };
        let Some((id, bytes)) = header.split_once(' ') else {
            return Err(self.protocol(format!("unexpected reply {line:?}")).await);
        };
        let id: u64 = self.parse_field(id, line).await?;
        let bytes: usize = self.parse_field(bytes, line).await?;
        let body = self.read_body(bytes).await?;
        Ok(Job { id, body })
    }

    async fn read_ok_body(&mut self) -> Result<String> {
        let line = self.read_line().await?;
        let Some(bytes) = line.strip_prefix("OK ") else {
            return Err(self.refusal(line).await);
        };
        let bytes: usize = self.parse_field(bytes, &line).await?;
        let body = self.read_body(bytes).await?;
        match String::from_utf8(body) {
            Ok(body) => Ok(body),
            Err(_) => Err(self.protocol("a YAML reply that isn't UTF-8".to_string()).await),
        }
    }

    /// Read a `bytes`-long body and the CRLF that must follow it. The buffer
    /// grows as the bytes arrive, so a length is never allocated on the
    /// server's word alone.
    async fn read_body(&mut self, bytes: usize) -> Result<Vec<u8>> {
        if bytes > MAX_BODY {
            return Err(self.protocol(format!("a {bytes}-byte body, past any job's size")).await);
        }
        let mut body = Vec::with_capacity(bytes.min(64 * 1024) + 2);
        let deadline = self.deadline;
        let read = {
            let mut rest = (&mut self.reader).take(bytes as u64 + 2);
            within(deadline, rest.read_to_end(&mut body)).await
        };
        match read {
            Some(Ok(n)) if n == bytes + 2 => {}
            Some(Ok(_)) => {
                let eof = io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed mid-body");
                return Err(self.close(Error::Io(eof)).await);
            }
            Some(Err(e)) => return Err(self.close(Error::Io(e)).await),
            None => return Err(self.close(Error::Timeout).await),
        }
        if !body.ends_with(b"\r\n") {
            return Err(self.protocol(format!("no CRLF after a {bytes}-byte body")).await);
        }
        body.truncate(bytes);
        Ok(body)
    }

    /// Parse a number out of a reply. One that doesn't parse is a reply this
    /// client can't follow, so it closes the connection.
    async fn parse_field<T: std::str::FromStr>(&mut self, field: &str, line: &str) -> Result<T> {
        match field.parse() {
            Ok(n) => Ok(n),
            Err(_) => Err(self.protocol(format!("unexpected reply {line:?}")).await),
        }
    }

    /// The error for a reply that isn't the one expected: tuber's refusal, if
    /// it is one. Any other reply may carry a body this client won't read, so
    /// it is a protocol error, and closes the connection.
    async fn refusal(&mut self, line: String) -> Error {
        match Error::from_reply(&line) {
            // EXPECTED_CRLF: tuber has closed its end.
            Some(e) if e.is_fatal() => self.close(e).await,
            Some(e) => e,
            None => self.protocol(format!("unexpected reply {line:?}")).await,
        }
    }

    async fn protocol(&mut self, msg: String) -> Error {
        self.close(Error::Protocol(msg)).await
    }

    /// Give up on the connection after `err`: shut it, so tuber hands back
    /// any job it held, and refuse every later call.
    async fn close(&mut self, err: Error) -> Error {
        self.closed = true;
        let _ = self.writer.shutdown().await;
        err
    }

    /// Send one command, already framed, and start the clock on its reply.
    async fn send(&mut self, frame: &[u8]) -> Result<()> {
        if self.closed {
            return Err(Error::Closed);
        }
        self.deadline = self.timeout.map(|t| tokio::time::Instant::now() + t);
        let sent = async {
            self.writer.write_all(frame).await?;
            self.writer.flush().await
        };
        match within(self.deadline, sent).await {
            Some(Ok(())) => Ok(()),
            Some(Err(e)) => Err(self.close(Error::Io(e)).await),
            None => Err(self.close(Error::Timeout).await),
        }
    }

    /// Allow `secs` more for the reply to a command that asked the server to
    /// wait that long.
    fn wait_longer(&mut self, secs: u32) {
        self.deadline = self.deadline.map(|d| d + Duration::from_secs(secs.into()));
    }

    async fn send_line(&mut self, line: &str) -> Result<()> {
        self.send(format!("{line}\r\n").as_bytes()).await
    }

    /// Read one reply line, without its line ending.
    async fn read_line(&mut self) -> Result<String> {
        let mut line = Vec::new();
        match within(self.deadline, self.reader.read_until(b'\n', &mut line)).await {
            Some(Ok(_)) if line.ends_with(b"\n") => {}
            Some(Ok(_)) => {
                let eof = io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed");
                return Err(self.close(Error::Io(eof)).await);
            }
            Some(Err(e)) => return Err(self.close(Error::Io(e)).await),
            None => return Err(self.close(Error::Timeout).await),
        }
        line.pop();
        if line.ends_with(b"\r") {
            line.pop();
        }
        match String::from_utf8(line) {
            Ok(line) => Ok(line),
            Err(_) => Err(self.protocol("a reply line that isn't UTF-8".to_string()).await),
        }
    }
}

/// Run `f` to completion, or until `deadline`: `None` when it missed it.
async fn within<F: Future>(deadline: Option<tokio::time::Instant>, f: F) -> Option<F::Output> {
    match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, f).await.ok(),
        None => Some(f.await),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fresh_insert() {
        let r = parse_inserted("INSERTED 42").unwrap();
        assert_eq!(r.id, 42);
        assert_eq!(r.existing_state, None);
        assert!(!r.is_duplicate());
    }

    #[test]
    fn parses_idempotent_duplicate() {
        let r = parse_inserted("INSERTED 1 READY").unwrap();
        assert_eq!(r.id, 1);
        assert_eq!(r.existing_state.as_deref(), Some("READY"));
        assert!(r.is_duplicate());
    }

    /// A buried duplicate carries a trailing priority: `INSERTED <id> BURIED <pri>`.
    #[test]
    fn parses_duplicate_with_priority() {
        let r = parse_inserted("INSERTED 7 BURIED 1024").unwrap();
        assert_eq!(r.id, 7);
        assert_eq!(r.existing_state.as_deref(), Some("BURIED"));
        assert_eq!(r.raised_pri, Some(1024));
        assert_eq!(parse_inserted("INSERTED 7 READY").unwrap().raised_pri, None);
    }

    /// `delete-batch` with no ids is a malformed command, not a no-op.
    #[tokio::test]
    async fn an_empty_delete_batch_sends_nothing() {
        let (addr, server) = converse(&[]).await;
        let mut c = TuberClient::connect(&addr).await.unwrap();
        let err = c.delete_batch(&[]).await.unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
        drop(c);
        assert!(server.await.unwrap().is_empty());
    }

    #[test]
    fn rejects_non_inserted_replies() {
        for line in ["DRAINING", "BAD_FORMAT", "EXPECTED_CRLF", "JOB_TOO_BIG", "INSERTED", "BURIED"] {
            assert!(parse_inserted(line).is_none(), "{line} should be an error");
        }
    }

    /// A one-connection server that answers the n-th command with
    /// `replies[n]`, and hangs up on a command past the last. Its handle
    /// yields every line it was sent, with a `put`'s or `schedule-put`'s body
    /// as the line after it, so it resolves once the client hangs up too.
    async fn converse(replies: &'static [&'static [u8]]) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut r = BufReader::new(r);
            let mut sent = Vec::new();
            let mut replies = replies.iter();
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                let line = line.trim_end().to_string();
                // `<bytes>` is put's 5th word and schedule-put's 7th.
                let bytes_at = match line.split_whitespace().next() {
                    Some("put") => Some(4),
                    Some("schedule-put") => Some(6),
                    _ => None,
                };
                if let Some(i) = bytes_at {
                    let n: usize = line.split_whitespace().nth(i).unwrap().parse().unwrap();
                    let mut body = vec![0u8; n + 2];
                    r.read_exact(&mut body).await.unwrap();
                    body.truncate(n);
                    sent.push(line);
                    sent.push(String::from_utf8(body).unwrap());
                } else {
                    sent.push(line);
                }
                // Out of replies, hang up rather than leave the client waiting
                // on one, but go on reading so the handle reports what it sent.
                match replies.next() {
                    Some(reply) => {
                        w.write_all(reply).await.unwrap();
                        w.write_all(b"\r\n").await.unwrap();
                    }
                    None => w.shutdown().await.unwrap(),
                }
            }
            sent
        });
        (addr, server)
    }

    fn entry(name: &str, spec: &str) -> ScheduleEntry {
        ScheduleEntry {
            name: name.into(),
            tube: "splat.maintenance".into(),
            pri: 1024,
            delay: 0,
            ttr: 120,
            tags: vec!["con:maintenance".into()],
            spec: spec.into(),
            body: b"{}".to_vec(),
        }
    }

    fn sync_error(err: &Error) -> &ScheduleSyncError {
        match err {
            Error::ScheduleSync(e) => e,
            other => panic!("not a ScheduleSyncError: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_sync_is_begin_puts_commit() {
        let (addr, server) = converse(&[b"SYNCING splat", b"STAGED", b"STAGED", b"SYNCED 1 1 2 0"]).await;
        let mut client = TuberClient::connect(&addr).await.unwrap();
        let set = [entry("rollup", "5 * * * *"), entry("retention", "0 2 * * * Australia/Melbourne")];
        let result = client.sync_schedules("splat", &set, false).await.unwrap();
        assert_eq!(result, SyncResult { added: 1, changed: 1, removed: 2, unchanged: 0 });
        drop(client);
        assert_eq!(
            server.await.unwrap(),
            [
                "schedule-begin splat",
                "schedule-put rollup splat.maintenance 1024 0 120 2 con:maintenance 5 * * * *",
                "{}",
                "schedule-put retention splat.maintenance 1024 0 120 2 con:maintenance 0 2 * * * Australia/Melbourne",
                "{}",
                "schedule-commit",
            ]
        );
    }

    /// A refused line dooms the commit, so the rest of the set is not sent: the
    /// sync is aborted on the spot, and the error names the entry.
    #[tokio::test]
    async fn a_refused_entry_aborts_the_sync_and_is_named() {
        let (addr, server) = converse(&[b"SYNCING splat", b"STAGED", b"BAD_SCHEDULE", b"ABORTED"]).await;
        let mut client = TuberClient::connect(&addr).await.unwrap();
        let set = [
            entry("rollup", "5 * * * *"),
            entry("retention", "0 2 * * * Australia/Melborne"),
            entry("never_sent", "@daily"),
        ];
        let err = client.sync_schedules("splat", &set, false).await.unwrap_err();
        let e = sync_error(&err);
        assert_eq!((e.entry.as_deref(), e.status.as_str()), (Some("retention"), "BAD_SCHEDULE"));
        assert!(!e.is_live());
        drop(client);
        let sent = server.await.unwrap();
        assert_eq!(sent.last().map(String::as_str), Some("schedule-abort"));
        assert!(!sent.iter().any(|l| l.contains("never_sent")), "sent past the refusal: {sent:?}");
        assert!(!sent.iter().any(|l| l == "schedule-commit"), "committed: {sent:?}");
    }

    /// The refusal is what the caller needs to hear. A connection that drops
    /// before the abort is answered has discarded the staged set anyway, so
    /// losing it must not swap the entry's name for "connection closed".
    #[tokio::test]
    async fn a_refusal_is_reported_even_if_the_abort_fails() {
        // No reply scripted for schedule-abort: the server hangs up instead.
        let (addr, _server) = converse(&[b"SYNCING splat", b"BAD_SCHEDULE"]).await;
        let mut client = TuberClient::connect(&addr).await.unwrap();
        let err = client
            .sync_schedules("splat", &[entry("retention", "0 2 * * * Australia/Melborne")], false)
            .await
            .unwrap_err();
        let e = sync_error(&err);
        assert_eq!((e.entry.as_deref(), e.status.as_str()), (Some("retention"), "BAD_SCHEDULE"));
    }

    #[tokio::test]
    async fn a_commit_internal_error_is_live() {
        let (addr, _server) = converse(&[b"SYNCING splat", b"STAGED", b"INTERNAL_ERROR"]).await;
        let mut client = TuberClient::connect(&addr).await.unwrap();
        let err = client
            .sync_schedules("splat", &[entry("rollup", "5 * * * *")], false)
            .await
            .unwrap_err();
        let e = sync_error(&err);
        assert_eq!((e.entry.as_deref(), e.status.as_str()), (None, "INTERNAL_ERROR"));
        assert!(e.is_live());
    }

    #[tokio::test]
    async fn an_old_server_says_it_has_no_schedules() {
        let (addr, _server) = converse(&[b"UNKNOWN_COMMAND"]).await;
        let mut client = TuberClient::connect(&addr).await.unwrap();
        let err = client
            .sync_schedules("splat", &[entry("rollup", "5 * * * *")], false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no recurring schedules"), "{err}");
    }

    /// An invalid entry is refused before anything goes on the wire: the server
    /// here would fail the test by hanging up on the first command.
    #[tokio::test]
    async fn an_invalid_set_sends_nothing() {
        let (addr, server) = converse(&[]).await;
        let mut client = TuberClient::connect(&addr).await.unwrap();
        let bad = [entry("rollup", "5 * * * *\r\nflush-tube splat.maintenance")];
        let err = client.sync_schedules("splat", &bad, false).await.unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
        let err = client.sync_schedules("splat", &[], false).await.unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
        drop(client);
        assert!(server.await.unwrap().is_empty());
    }

    fn assert_refused<T: std::fmt::Debug>(what: &str, result: Result<T>) {
        let err = result.expect_err(what);
        assert!(matches!(err, Error::InvalidInput(_)), "{what}: {err:?}");
    }

    /// A name with a CRLF in it ends the command early and sends the rest as
    /// one of its own: `stats-tube default\r\nflush-tube victim` empties
    /// `victim`. So every name and tag is checked before anything goes on the
    /// wire; the server here would fail the test by hanging up on the first
    /// command.
    #[tokio::test]
    async fn a_name_that_could_smuggle_a_command_sends_nothing() {
        let (addr, server) = converse(&[]).await;
        let mut c = TuberClient::connect(&addr).await.unwrap();
        let long = "a".repeat(201);
        for bad in ["default\r\nflush-tube victim", "two words", "", "-leading", long.as_str()] {
            assert_refused(&format!("stats-tube {bad:?}"), c.stats_tube(bad).await);
            assert_refused(&format!("use {bad:?}"), c.use_tube(bad).await);
            assert_refused(&format!("watch {bad:?}"), c.watch(bad).await);
            assert_refused(&format!("ignore {bad:?}"), c.ignore(bad).await);
            assert_refused(&format!("watch_only {bad:?}"), c.watch_only(bad).await);
            assert_refused(&format!("stats-group {bad:?}"), c.stats_group(bad).await);
            assert_refused(&format!("flush-tube {bad:?}"), c.flush_tube(bad).await);
            assert_refused(&format!("flush-buried {bad:?}"), c.flush_buried(bad).await);
            assert_refused(&format!("pause-tube {bad:?}"), c.pause_tube(bad, 0).await);
        }
        drop(c);
        assert!(server.await.unwrap().is_empty());
    }

    /// Tags go on the `put` line, so one with a space or a CRLF in it can add a
    /// tag the caller never asked for, or end the line and start another.
    #[tokio::test]
    async fn a_tag_that_could_break_the_put_line_sends_nothing() {
        let (addr, server) = converse(&[]).await;
        let mut c = TuberClient::connect(&addr).await.unwrap();
        for bad in ["grp:a\r\nflush-tube victim", "grp:a aft:b", "idp:key\0", "", "con:\x7f"] {
            assert_refused(&format!("tag {bad:?}"), c.put_tagged(0, 0, 60, b"{}", &[bad.to_string()]).await);
        }
        drop(c);
        assert!(server.await.unwrap().is_empty());
    }

    /// A job body is bytes, not text. Refusing one that isn't UTF-8 lost its
    /// id with the job still reserved, and left the batch's later jobs on the
    /// wire for the next command to read as its reply.
    #[tokio::test]
    async fn a_body_that_is_not_utf8_is_returned_as_bytes() {
        let (addr, _server) = converse(&[
            b"RESERVED_BATCH 3\r\nRESERVED 1 1\r\n\xff\r\nRESERVED 2 3\r\nok1\r\nRESERVED 3 3\r\nok2",
            b"OK 4\r\n---\n",
        ])
        .await;
        let mut c = TuberClient::connect(&addr).await.unwrap();
        let jobs = c.reserve_batch(3, None).await.unwrap();
        let got: Vec<(u64, &[u8])> = jobs.iter().map(|j| (j.id, j.body.as_ref())).collect();
        assert_eq!(got, [(1, &b"\xff"[..]), (2, b"ok1"), (3, b"ok2")]);
        // The connection is still in step: stats reads its own reply.
        c.stats().await.unwrap();
    }

    /// Tubes come and go between `list-tubes` and the `stats-tube` that
    /// follows: one emptied in that gap answers NOT_FOUND. The snapshot leaves
    /// it out rather than failing whole, which the TUI showed as a lost
    /// connection.
    #[tokio::test]
    async fn a_tube_that_vanishes_mid_snapshot_is_left_out() {
        let (addr, server) = converse(&[
            b"OK 4\r\n---\n",
            b"OK 33\r\n---\n- default\n- gone\n- two words\n",
            b"OK 18\r\n---\nname: default\n",
            b"NOT_FOUND",
        ])
        .await;
        let mut c = TuberClient::connect(&addr).await.unwrap();
        let snap = c.fetch_snapshot().await.unwrap();
        let names: Vec<&str> = snap.tubes.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["default"]);
        drop(c);
        // A name a laxer server lists but this client won't send is skipped
        // too, without asking.
        assert_eq!(
            server.await.unwrap(),
            ["stats", "list-tubes", "stats-tube default", "stats-tube gone"]
        );
    }

    /// TIMED_OUT and DEADLINE_SOON are answers, not failures: no job came in
    /// time, or one this connection holds is about to run out of TTR.
    #[tokio::test]
    async fn a_reserve_that_gets_no_job_is_not_an_error() {
        let (addr, _server) = converse(&[b"TIMED_OUT", b"DEADLINE_SOON"]).await;
        let mut c = TuberClient::connect(&addr).await.unwrap();
        assert_eq!(c.reserve_with_timeout(1).await.unwrap(), Reserved::TimedOut);
        assert_eq!(c.reserve_with_timeout(1).await.unwrap(), Reserved::DeadlineSoon);
    }

    /// A batch worker waiting on `reserve-batch` is told DEADLINE_SOON when a
    /// job it holds is about to run out of TTR. That is its cue to touch-all,
    /// on the same connection: closing it would hand back every job it holds.
    #[tokio::test]
    async fn a_batch_reserve_told_deadline_soon_keeps_the_connection() {
        let (addr, _server) = converse(&[b"DEADLINE_SOON", b"TOUCHED_ALL 3"]).await;
        let mut c = TuberClient::connect(&addr).await.unwrap();
        let err = c.reserve_batch(10, Some(5)).await.unwrap_err();
        assert_eq!(err.reply(), Some("DEADLINE_SOON"), "{err:?}");
        assert!(!err.is_fatal(), "{err:?}");
        assert_eq!(c.touch_all().await.unwrap(), 3);
    }

    /// beanstalkd answers a put it couldn't queue for want of memory
    /// `BURIED <id>`: the job exists, buried, and the id is the caller's only
    /// handle on it.
    #[tokio::test]
    async fn a_put_buried_on_arrival_keeps_its_id() {
        let (addr, _server) = converse(&[b"BURIED 7"]).await;
        let mut c = TuberClient::connect(&addr).await.unwrap();
        let put = c.put_tagged(0, 0, 60, b"x", &[]).await.unwrap();
        assert_eq!(put.id, 7);
        assert!(put.buried && !put.is_duplicate(), "{put:?}");
    }

    /// A reply the client can't frame leaves it not knowing where the next
    /// one starts, so the connection is done with: the next command fails
    /// without being sent, rather than reading leftovers as its reply.
    #[tokio::test]
    async fn a_framing_error_closes_the_connection() {
        for bad in [
            &b"RESERVED 1 x"[..],              // no length
            b"RESERVED 1 3\r\nabcXY",            // no CRLF after the body
            b"OK 5\r\nhello",                    // a body where a job was expected
        ] {
            let replies: &'static [&'static [u8]] = Box::leak(vec![bad, &b"OK 4\r\n---\n"[..]].into_boxed_slice());
            let (addr, server) = converse(replies).await;
            let mut c = TuberClient::connect(&addr).await.unwrap();
            let what = String::from_utf8_lossy(bad);
            let err = c.reserve_with_timeout(0).await.unwrap_err();
            assert!(matches!(err, Error::Protocol(_)), "{what}: {err:?}");
            let err = c.stats().await.unwrap_err();
            assert!(matches!(err, Error::Closed), "{what}: the next command ran: {err:?}");
            drop(c);
            assert_eq!(server.await.unwrap(), ["reserve-with-timeout 0"], "{what}");
        }
    }

    /// A server that accepts the connection and then says nothing: up, but
    /// wedged, or not tuber.
    async fn silent_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        addr
    }

    /// A server that stops answering must not hang its client forever: a
    /// `tuber-cli stats` from cron would never exit.
    #[tokio::test]
    async fn a_silent_server_times_out() {
        let mut c = TuberClient::connect(&silent_server().await).await.unwrap();
        // From here the clock jumps to the next timer whenever nothing else
        // can run, so a timeout of any length takes no real time.
        tokio::time::pause();
        let reply = tokio::time::timeout(std::time::Duration::from_secs(3600), c.stats()).await;
        let err = reply.expect("still waiting after an hour").unwrap_err();
        assert!(matches!(err, Error::Timeout), "{err:?}");
        // A reply may yet arrive; reading it as the next command's would be
        // wrong, so the connection is done with.
        assert!(matches!(c.stats().await.unwrap_err(), Error::Closed));
    }

    /// A server that answers its first command `reply`, but only after
    /// `delay`.
    async fn slow_server(delay: std::time::Duration, reply: &'static [u8]) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut line = String::new();
            BufReader::new(r).read_line(&mut line).await.unwrap();
            tokio::time::sleep(delay).await;
            w.write_all(reply).await.unwrap();
            std::future::pending::<()>().await;
        });
        addr
    }

    /// The timeout is for the server's own work: a reserve that asks it to
    /// wait gets that long on top. Real time, not a paused clock: one jumps
    /// ahead before the server has read the command.
    #[tokio::test]
    async fn a_reserve_may_wait_as_long_as_it_asked_to() {
        let second = std::time::Duration::from_secs(1);
        let addr = slow_server(second, b"TIMED_OUT\r\n").await;
        let mut c = TuberClient::connect(&addr).await.unwrap();
        c.set_timeout(Some(std::time::Duration::from_millis(200)));
        assert_eq!(c.reserve_with_timeout(1).await.unwrap(), Reserved::TimedOut);

        let addr = slow_server(second, b"RESERVED_BATCH 0\r\n").await;
        let mut c = TuberClient::connect(&addr).await.unwrap();
        c.set_timeout(Some(std::time::Duration::from_millis(200)));
        assert_eq!(c.reserve_batch(10, Some(1)).await.unwrap(), []);
    }

    #[tokio::test]
    async fn a_blocking_reserve_waits_for_a_job() {
        let addr = slow_server(std::time::Duration::from_secs(1), b"RESERVED 9 2\r\nhi\r\n").await;
        let mut c = TuberClient::connect(&addr).await.unwrap();
        c.set_timeout(Some(std::time::Duration::from_millis(200)));
        let job = c.reserve().await.unwrap().job().unwrap();
        assert_eq!((job.id, job.body.as_slice()), (9, &b"hi"[..]));
    }

    /// The worker's commands: finish a job by releasing it, steer reserves by
    /// weight and mode, and look at what this connection holds and watches.
    #[tokio::test]
    async fn the_worker_commands_frame_and_parse() {
        let (addr, server) = converse(&[
            b"RELEASED",
            b"NOT_FOUND",
            b"USING weighted",
            b"WATCHING 2",
            b"FOUND 5 2\r\nhi",
            b"OK 23\r\n---\n- default\n- emails\n",
            b"USING emails",
        ])
        .await;
        let mut c = TuberClient::connect(&addr).await.unwrap();
        c.release(5, 1024, 30).await.unwrap();
        assert!(matches!(c.release(6, 0, 0).await.unwrap_err(), Error::NotFound));
        c.reserve_mode(ReserveMode::Weighted).await.unwrap();
        assert_eq!(c.watch_weighted("emails", 5).await.unwrap(), 2);
        assert_eq!(c.peek_reserved().await.unwrap(), Job { id: 5, body: b"hi".to_vec() });
        assert_eq!(c.list_tubes_watched().await.unwrap(), ["default", "emails"]);
        assert_eq!(c.list_tube_used().await.unwrap(), "emails");
        drop(c);
        assert_eq!(
            server.await.unwrap(),
            [
                "release 5 1024 30",
                "release 6 0 0",
                "reserve-mode weighted",
                "watch emails 5",
                "peek-reserved",
                "list-tubes-watched",
                "list-tube-used",
            ]
        );
    }

    /// beanstalkd answers a release it couldn't queue for want of memory
    /// `BURIED`: the job is buried rather than ready, and the connection fine.
    #[tokio::test]
    async fn a_release_buried_for_want_of_memory_is_out_of_memory() {
        let (addr, _server) = converse(&[b"BURIED", b"TOUCHED_ALL 0"]).await;
        let mut c = TuberClient::connect(&addr).await.unwrap();
        assert!(matches!(c.release(5, 0, 0).await.unwrap_err(), Error::OutOfMemory));
        assert_eq!(c.touch_all().await.unwrap(), 0);
    }

    /// A weight outside 1..=9999 is refused before it is sent, as tuber would.
    #[tokio::test]
    async fn a_watch_weight_out_of_range_sends_nothing() {
        let (addr, server) = converse(&[]).await;
        let mut c = TuberClient::connect(&addr).await.unwrap();
        for weight in [0, 10_000] {
            assert_refused(&format!("weight {weight}"), c.watch_weighted("emails", weight).await);
        }
        drop(c);
        assert!(server.await.unwrap().is_empty());
    }

    /// A body length is the server's word, and a lying or broken server
    /// once made the client allocate it whole: `FOUND 1 99999999999` asked
    /// for 100 GB and aborted. A length past tuber's 1 GiB job limit is a
    /// protocol error, and one at the top of u64 can't wrap the `+ 2` for
    /// the CRLF.
    #[tokio::test]
    async fn a_body_length_past_any_job_is_refused_not_allocated() {
        for reply in [&b"FOUND 1 99999999999"[..], b"FOUND 1 18446744073709551615", b"OK 99999999999"] {
            let replies: &'static [&'static [u8]] = Box::leak(vec![reply].into_boxed_slice());
            let (addr, _server) = converse(replies).await;
            let mut c = TuberClient::connect(&addr).await.unwrap();
            let what = String::from_utf8_lossy(reply);
            let err = if reply.starts_with(b"OK") {
                c.stats().await.map(|_| ()).unwrap_err()
            } else {
                c.peek(1).await.map(|_| ()).unwrap_err()
            };
            assert!(matches!(err, Error::Protocol(_)), "{what}: {err:?}");
        }
    }

    /// tuber closes the connection after EXPECTED_CRLF, so the client must
    /// count it as closed rather than send the next command into it.
    #[tokio::test]
    async fn expected_crlf_closes_the_connection() {
        let (addr, server) = converse(&[b"EXPECTED_CRLF", b"OK 4\r\n---\n"]).await;
        let mut c = TuberClient::connect(&addr).await.unwrap();
        let err = c.put(0, 0, 60, b"x").await.unwrap_err();
        assert!(matches!(err, Error::ExpectedCrlf) && err.is_fatal(), "{err:?}");
        assert!(matches!(c.stats().await.unwrap_err(), Error::Closed));
        drop(c);
        assert_eq!(server.await.unwrap(), ["put 0 0 60 1", "x"]);
    }

    /// `watch_only` is "reserve from this tube and no other". On a client
    /// that watched other tubes before, ignoring only `default` left them
    /// watched.
    #[tokio::test]
    async fn watch_only_ignores_every_other_tube() {
        let (addr, server) = converse(&[
            b"WATCHING 3",
            b"OK 29\r\n---\n- default\n- old\n- emails\n",
            b"WATCHING 2",
            b"WATCHING 1",
        ])
        .await;
        let mut c = TuberClient::connect(&addr).await.unwrap();
        c.watch_only("emails").await.unwrap();
        drop(c);
        assert_eq!(
            server.await.unwrap(),
            ["watch emails", "list-tubes-watched", "ignore default", "ignore old"]
        );
    }

    #[tokio::test]
    async fn lists_schedules_as_owner_name_pairs() {
        let (addr, server) = converse(&[b"OK 37\r\n---\n- splat rollup\n- splat retention\n"]).await;
        let mut client = TuberClient::connect(&addr).await.unwrap();
        let list = client.list_schedules(Some("splat")).await.unwrap();
        assert_eq!(
            list,
            [("splat".into(), "rollup".into()), ("splat".into(), "retention".into())]
        );
        drop(client);
        assert_eq!(server.await.unwrap(), ["list-schedules splat"]);
    }
}
