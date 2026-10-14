use crate::model::{GroupStats, JobStats, ServerStats, Snapshot, TubeStats};
use crate::name::{check_name, check_tag};
use crate::parse::parse_yaml_list;
use crate::schedule::{self, ScheduleEntry, ScheduleStats, ScheduleSyncError, SyncResult};
use std::io;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// Outcome of a `put`. When `existing_state` is `Some`, an `idp:` key matched
/// a job that was already there: nothing was enqueued, `id` is that job's id,
/// and the state is where it currently sits (`READY`, `BURIED`, `DELETED`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutResult {
    pub id: u64,
    pub existing_state: Option<String>,
}

impl PutResult {
    /// True when the put was deduplicated against an existing job.
    pub fn is_duplicate(&self) -> bool {
        self.existing_state.is_some()
    }
}

/// Parse a `put` reply. `INSERTED <id>` is a fresh insert; a trailing state
/// (`INSERTED <id> <state> [<pri>]`) means an `idp:` key matched an existing
/// job and nothing was enqueued.
fn parse_inserted(line: &str) -> io::Result<PutResult> {
    let Some(rest) = line.strip_prefix("INSERTED ") else {
        return Err(io::Error::other(line.to_string()));
    };
    let mut fields = rest.split_whitespace();
    let id: u64 = fields
        .next()
        .unwrap_or_default()
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(PutResult {
        id,
        existing_state: fields.next().map(str::to_string),
    })
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

pub struct TuberClient {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
}

impl TuberClient {
    pub async fn connect(addr: &str) -> io::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        stream.set_nodelay(true)?;
        let (reader, writer) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(reader),
            writer,
        })
    }

    pub async fn stats(&mut self) -> io::Result<ServerStats> {
        self.send_line("stats").await?;
        let body = self.read_ok_body().await?;
        Ok(ServerStats::from_yaml(&body))
    }

    pub async fn list_tubes(&mut self) -> io::Result<Vec<String>> {
        self.send_line("list-tubes").await?;
        let body = self.read_ok_body().await?;
        Ok(parse_yaml_list(&body))
    }

    pub async fn stats_tube(&mut self, tube: &str) -> io::Result<TubeStats> {
        check_name("tube name", tube)?;
        self.send_line(&format!("stats-tube {tube}")).await?;
        let body = self.read_ok_body().await?;
        Ok(TubeStats::from_yaml(&body))
    }

    pub async fn fetch_snapshot(&mut self) -> io::Result<Snapshot> {
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
                Err(e) if e.to_string() == "NOT_FOUND" => {}
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
    pub async fn use_tube(&mut self, tube: &str) -> io::Result<String> {
        check_name("tube name", tube)?;
        self.send_line(&format!("use {tube}")).await?;
        let line = self.read_line().await?;
        if let Some(name) = line.strip_prefix("USING ") {
            Ok(name.to_string())
        } else {
            Err(io::Error::other(line))
        }
    }

    /// Put a job into the currently used tube.
    /// Sends: `put <pri> <delay> <ttr> <bytes>\r\n<data>\r\n` — expects: `INSERTED <id>\r\n`
    pub async fn put(&mut self, priority: u32, delay: u32, ttr: u32, data: &[u8]) -> io::Result<u64> {
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
    pub async fn put_tagged(
        &mut self,
        priority: u32,
        delay: u32,
        ttr: u32,
        data: &[u8],
        tags: &[String],
    ) -> io::Result<PutResult> {
        for tag in tags {
            check_tag(tag)?;
        }
        let mut cmd = format!("put {} {} {} {}", priority, delay, ttr, data.len());
        for tag in tags {
            cmd.push(' ');
            cmd.push_str(tag);
        }
        cmd.push_str("\r\n");
        self.writer.write_all(cmd.as_bytes()).await?;
        self.writer.write_all(data).await?;
        self.writer.write_all(b"\r\n").await?;
        self.writer.flush().await?;
        let line = self.read_line().await?;
        parse_inserted(&line)
    }

    /// Reserve the next available job, waiting up to `timeout` seconds.
    /// Sends: `reserve-with-timeout <seconds>\r\n` — expects: `RESERVED <id> <bytes>\r\n<data>\r\n`
    pub async fn reserve(&mut self, timeout: u32) -> io::Result<Job> {
        self.send_line(&format!("reserve-with-timeout {timeout}")).await?;
        self.read_job_response("RESERVED").await
    }

    /// Reserve one specific job by ID, regardless of tube or watch list. The
    /// job must be ready, delayed, or buried — not already reserved.
    /// Sends: `reserve-job <id>\r\n` — expects: `RESERVED <id> <bytes>\r\n<data>\r\n`
    pub async fn reserve_job(&mut self, id: u64) -> io::Result<Job> {
        self.send_line(&format!("reserve-job {id}")).await?;
        self.read_job_response("RESERVED").await
    }

    /// Reserve up to `count` jobs in a single command. With `timeout = Some(n)`
    /// where `n > 0`, blocks up to `n` seconds for at least one job to appear;
    /// `None` or `Some(0)` returns immediately (possibly empty). Reserves from
    /// the watch list, so use [`watch_only`](Self::watch_only) to target a tube.
    /// Sends: `reserve-batch <count> [timeout]\r\n`
    /// Expects: `RESERVED_BATCH <n>\r\n` then n × `RESERVED <id> <bytes>\r\n<data>\r\n`
    pub async fn reserve_batch(&mut self, count: u32, timeout: Option<u32>) -> io::Result<Vec<Job>> {
        let cmd = match timeout {
            Some(t) => format!("reserve-batch {count} {t}"),
            None => format!("reserve-batch {count}"),
        };
        self.send_line(&cmd).await?;
        let line = self.read_line().await?;
        let n: usize = line
            .strip_prefix("RESERVED_BATCH ")
            .ok_or_else(|| io::Error::other(line.clone()))?
            .trim()
            .parse()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut jobs = Vec::with_capacity(n);
        for _ in 0..n {
            jobs.push(self.read_job_response("RESERVED").await?);
        }
        Ok(jobs)
    }

    /// Add a tube to the watch set for subsequent reserves.
    /// Sends: `watch <tube>\r\n` — expects: `WATCHING <count>\r\n`
    pub async fn watch(&mut self, tube: &str) -> io::Result<u64> {
        check_name("tube name", tube)?;
        self.send_line(&format!("watch {tube}")).await?;
        self.read_u64_response("WATCHING").await
    }

    /// Remove a tube from the watch set.
    /// Sends: `ignore <tube>\r\n` — expects: `WATCHING <count>\r\n` (or `NOT_IGNORED` if it was the last)
    pub async fn ignore(&mut self, tube: &str) -> io::Result<u64> {
        check_name("tube name", tube)?;
        self.send_line(&format!("ignore {tube}")).await?;
        self.read_u64_response("WATCHING").await
    }

    /// Watch only `tube` for subsequent reserves: watch it, then ignore the
    /// `default` tube (unless `tube` is itself `default`). `reserve` and
    /// `reserve-batch` pull from the watch list — not the used tube — so this
    /// is how you target a specific tube for reserving.
    pub async fn watch_only(&mut self, tube: &str) -> io::Result<()> {
        self.watch(tube).await?;
        if tube != "default" {
            self.ignore("default").await?;
        }
        Ok(())
    }

    /// Delete a job by ID.
    /// Sends: `delete <id>\r\n` — expects: `DELETED\r\n`
    pub async fn delete(&mut self, id: u64) -> io::Result<()> {
        self.send_line(&format!("delete {id}")).await?;
        self.expect_response("DELETED").await
    }

    /// Kick at most `bound` buried or delayed jobs in the currently used tube.
    /// Sends: `kick <bound>\r\n` — expects: `KICKED <count>\r\n`
    pub async fn kick(&mut self, bound: u32) -> io::Result<u64> {
        self.send_line(&format!("kick {bound}")).await?;
        self.read_u64_response("KICKED").await
    }

    /// Kick one specific buried or delayed job back to ready.
    /// Sends: `kick-job <id>\r\n` — expects: `KICKED\r\n`
    pub async fn kick_job(&mut self, id: u64) -> io::Result<()> {
        self.send_line(&format!("kick-job {id}")).await?;
        self.expect_response("KICKED").await
    }

    /// Enter drain mode: new `put`s are rejected with `DRAINING` while
    /// in-flight work is allowed to finish. Server-wide and persistent until
    /// [`undrain`](Self::undrain). Also triggered by `SIGUSR1`.
    /// Sends: `drain\r\n` — expects: `DRAINING\r\n`
    pub async fn drain(&mut self) -> io::Result<()> {
        self.send_line("drain").await?;
        self.expect_response("DRAINING").await
    }

    /// Leave drain mode and resume accepting `put`s. Idempotent.
    /// Sends: `undrain\r\n` — expects: `NOT_DRAINING\r\n`
    pub async fn undrain(&mut self) -> io::Result<()> {
        self.send_line("undrain").await?;
        self.expect_response("NOT_DRAINING").await
    }

    /// Peek at a job by ID without reserving it.
    /// Sends: `peek <id>\r\n` — expects: `FOUND <id> <bytes>\r\n<data>\r\n`
    pub async fn peek(&mut self, id: u64) -> io::Result<Job> {
        self.send_line(&format!("peek {id}")).await?;
        self.read_job_response("FOUND").await
    }

    /// Peek at the next ready job in the currently used tube.
    pub async fn peek_ready(&mut self) -> io::Result<Job> {
        self.send_line("peek-ready").await?;
        self.read_job_response("FOUND").await
    }

    /// Peek at the next buried job in the currently used tube.
    pub async fn peek_buried(&mut self) -> io::Result<Job> {
        self.send_line("peek-buried").await?;
        self.read_job_response("FOUND").await
    }

    /// Peek at the next delayed job in the currently used tube.
    pub async fn peek_delayed(&mut self) -> io::Result<Job> {
        self.send_line("peek-delayed").await?;
        self.read_job_response("FOUND").await
    }

    /// Show statistics for a job by ID.
    /// Sends: `stats-job <id>\r\n` — expects: `OK <bytes>\r\n<yaml>\r\n`
    pub async fn stats_job(&mut self, id: u64) -> io::Result<JobStats> {
        self.send_line(&format!("stats-job {id}")).await?;
        let body = self.read_ok_body().await?;
        Ok(JobStats::from_yaml(&body))
    }

    /// Show statistics for a job group.
    /// Sends: `stats-group <name>\r\n` — expects: `OK <bytes>\r\n<yaml>\r\n`
    pub async fn stats_group(&mut self, name: &str) -> io::Result<GroupStats> {
        check_name("group name", name)?;
        self.send_line(&format!("stats-group {name}")).await?;
        let body = self.read_ok_body().await?;
        Ok(GroupStats::from_yaml(&body))
    }

    /// Flush all jobs from a tube.
    /// Sends: `flush-tube <tube>\r\n` — expects: `FLUSHED <count>\r\n`
    pub async fn flush_tube(&mut self, tube: &str) -> io::Result<u64> {
        check_name("tube name", tube)?;
        self.send_line(&format!("flush-tube {tube}")).await?;
        self.read_u64_response("FLUSHED").await
    }

    /// Flush only the buried jobs from a tube; ready, delayed, and reserved
    /// jobs are left untouched.
    /// Sends: `flush-buried <tube>\r\n` — expects: `FLUSHED <count>\r\n`
    pub async fn flush_buried(&mut self, tube: &str) -> io::Result<u64> {
        check_name("tube name", tube)?;
        self.send_line(&format!("flush-buried {tube}")).await?;
        self.read_u64_response("FLUSHED").await
    }

    /// Delete multiple jobs in a single command.
    /// Sends: `delete-batch <id1> <id2> ...\r\n` — expects: `DELETED_BATCH <deleted> <not_found>\r\n`
    pub async fn delete_batch(&mut self, ids: &[u64]) -> io::Result<(u64, u64)> {
        let id_strs: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        self.send_line(&format!("delete-batch {}", id_strs.join(" "))).await?;
        let line = self.read_line().await?;
        if let Some(rest) = line.strip_prefix("DELETED_BATCH ") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() == 2 {
                let deleted: u64 = parts[0].parse().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                let not_found: u64 = parts[1].parse().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                return Ok((deleted, not_found));
            }
        }
        Err(io::Error::other(line))
    }

    /// Bury a reserved job.
    /// Sends: `bury <id> <priority>\r\n` — expects: `BURIED\r\n`
    pub async fn bury(&mut self, id: u64, priority: u32) -> io::Result<()> {
        self.send_line(&format!("bury {id} {priority}")).await?;
        self.expect_response("BURIED").await
    }

    /// Reset the TTR timer on a single reserved job.
    /// Sends: `touch <id>\r\n` — expects: `TOUCHED\r\n`
    pub async fn touch(&mut self, id: u64) -> io::Result<()> {
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
    pub async fn touch_all(&mut self) -> io::Result<u64> {
        self.send_line("touch-all").await?;
        self.read_u64_response("TOUCHED_ALL").await
    }

    /// Pause a tube for `delay` seconds.
    /// Sends: `pause-tube <tube> <delay>\r\n` — expects: `PAUSED\r\n`
    pub async fn pause_tube(&mut self, tube: &str, delay: u32) -> io::Result<()> {
        check_name("tube name", tube)?;
        self.send_line(&format!("pause-tube {tube} {delay}")).await?;
        self.expect_response("PAUSED").await
    }

    /// The recurring schedule entries held, as `(owner, name)` pairs: all of
    /// them, or `owner`'s.
    /// Sends: `list-schedules [<owner>]\r\n` — expects: `OK <bytes>\r\n<yaml>\r\n`
    pub async fn list_schedules(&mut self, owner: Option<&str>) -> io::Result<Vec<(String, String)>> {
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
    /// (`NOT_FOUND` comes back as the error's message, as for `stats-job`)
    pub async fn stats_schedule(&mut self, owner: &str, name: &str) -> io::Result<ScheduleStats> {
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
    /// A refusal from tuber comes back as a [`ScheduleSyncError`] inside the
    /// `io::Error`. Nothing changed unless its
    /// [`is_live`](ScheduleSyncError::is_live) says the commit took but
    /// couldn't be written to disk.
    ///
    /// Sends: `schedule-begin <owner>`, a `schedule-put` per entry framed like
    /// `put`, then `schedule-commit` — expects: `SYNCED <added> <changed>
    /// <removed> <unchanged>\r\n`
    pub async fn sync_schedules(
        &mut self,
        owner: &str,
        entries: &[ScheduleEntry],
        allow_empty: bool,
    ) -> io::Result<SyncResult> {
        schedule::validate_sync(owner, entries, allow_empty)?;
        let refused = |entry: Option<&str>, status: String| {
            io::Error::other(ScheduleSyncError {
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
            self.writer.write_all(&schedule::frame(entry)).await?;
            self.writer.flush().await?;
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
    async fn expect_response(&mut self, expected: &str) -> io::Result<()> {
        let line = self.read_line().await?;
        if line == expected {
            Ok(())
        } else {
            Err(io::Error::other(line))
        }
    }

    /// Read a response with a prefix and trailing u64 (e.g. "KICKED 5").
    async fn read_u64_response(&mut self, prefix: &str) -> io::Result<u64> {
        let line = self.read_line().await?;
        if let Some(val_str) = line.strip_prefix(prefix).and_then(|s| s.strip_prefix(' ')) {
            val_str.parse().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        } else {
            Err(io::Error::other(line))
        }
    }

    /// Read a response with a job body (RESERVED/FOUND): `<PREFIX> <id> <bytes>\r\n<data>\r\n`
    async fn read_job_response(&mut self, prefix: &str) -> io::Result<Job> {
        let line = self.read_line().await?;
        if !line.starts_with(prefix) {
            return Err(io::Error::other(line));
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unexpected response: {line}")));
        }
        let id: u64 = parts[1].parse().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let bytes: usize = parts[2].parse().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut body = vec![0u8; bytes];
        self.reader.read_exact(&mut body).await?;
        // Consume trailing \r\n
        let mut trail = [0u8; 2];
        self.reader.read_exact(&mut trail).await?;
        Ok(Job { id, body })
    }

    async fn read_ok_body(&mut self) -> io::Result<String> {
        let line = self.read_line().await?;
        if !line.starts_with("OK ") {
            return Err(io::Error::other(line));
        }
        let bytes: usize = line[3..]
            .trim()
            .parse()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut buf = vec![0u8; bytes + 2];
        self.reader.read_exact(&mut buf).await?;
        buf.truncate(bytes);
        String::from_utf8(buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    async fn send_line(&mut self, line: &str) -> io::Result<()> {
        self.writer
            .write_all(format!("{line}\r\n").as_bytes())
            .await?;
        self.writer.flush().await
    }

    async fn read_line(&mut self) -> io::Result<String> {
        let mut line = String::new();
        self.reader.read_line(&mut line).await?;
        if line.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed",
            ));
        }
        Ok(line.trim_end().to_string())
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
    }

    #[test]
    fn rejects_non_inserted_replies() {
        for line in ["DRAINING", "BAD_FORMAT", "EXPECTED_CRLF", "JOB_TOO_BIG"] {
            assert!(parse_inserted(line).is_err(), "{line} should be an error");
        }
    }

    /// A one-connection server that answers the n-th command with
    /// `replies[n]`, and hangs up on a command past the last. Its handle
    /// yields every line it was sent, with a `schedule-put`'s body as the line
    /// after it, so it resolves once the client hangs up too.
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
                if line.starts_with("schedule-put ") {
                    let n: usize = line.split_whitespace().nth(6).unwrap().parse().unwrap();
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

    fn sync_error(err: &io::Error) -> &ScheduleSyncError {
        err.get_ref()
            .and_then(|e| e.downcast_ref::<ScheduleSyncError>())
            .unwrap_or_else(|| panic!("not a ScheduleSyncError: {err}"))
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
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let err = client.sync_schedules("splat", &[], false).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        drop(client);
        assert!(server.await.unwrap().is_empty());
    }

    fn assert_refused<T: std::fmt::Debug>(what: &str, result: io::Result<T>) {
        let err = result.expect_err(what);
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{what}: {err}");
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
