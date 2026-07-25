use crate::model::{GroupStats, JobStats, ServerStats, Snapshot, TubeStats};
use crate::parse::parse_yaml_list;
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
        self.send_line(&format!("stats-tube {tube}")).await?;
        let body = self.read_ok_body().await?;
        Ok(TubeStats::from_yaml(&body))
    }

    pub async fn fetch_snapshot(&mut self) -> io::Result<Snapshot> {
        let server = self.stats().await?;
        let tube_names = self.list_tubes().await?;
        let mut tubes = Vec::with_capacity(tube_names.len());
        for name in &tube_names {
            tubes.push(self.stats_tube(name).await?);
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
    /// limit). Tags are passed through verbatim.
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
    pub async fn reserve(&mut self, timeout: u32) -> io::Result<(u64, String)> {
        self.send_line(&format!("reserve-with-timeout {timeout}")).await?;
        self.read_job_response("RESERVED").await
    }

    /// Reserve one specific job by ID, regardless of tube or watch list. The
    /// job must be ready, delayed, or buried — not already reserved.
    /// Sends: `reserve-job <id>\r\n` — expects: `RESERVED <id> <bytes>\r\n<data>\r\n`
    pub async fn reserve_job(&mut self, id: u64) -> io::Result<(u64, String)> {
        self.send_line(&format!("reserve-job {id}")).await?;
        self.read_job_response("RESERVED").await
    }

    /// Reserve up to `count` jobs in a single command. With `timeout = Some(n)`
    /// where `n > 0`, blocks up to `n` seconds for at least one job to appear;
    /// `None` or `Some(0)` returns immediately (possibly empty). Reserves from
    /// the watch list, so use [`watch_only`](Self::watch_only) to target a tube.
    /// Sends: `reserve-batch <count> [timeout]\r\n`
    /// Expects: `RESERVED_BATCH <n>\r\n` then n × `RESERVED <id> <bytes>\r\n<data>\r\n`
    pub async fn reserve_batch(&mut self, count: u32, timeout: Option<u32>) -> io::Result<Vec<(u64, String)>> {
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
        self.send_line(&format!("watch {tube}")).await?;
        self.read_u64_response("WATCHING").await
    }

    /// Remove a tube from the watch set.
    /// Sends: `ignore <tube>\r\n` — expects: `WATCHING <count>\r\n` (or `NOT_IGNORED` if it was the last)
    pub async fn ignore(&mut self, tube: &str) -> io::Result<u64> {
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
    pub async fn peek(&mut self, id: u64) -> io::Result<(u64, String)> {
        self.send_line(&format!("peek {id}")).await?;
        self.read_job_response("FOUND").await
    }

    /// Peek at the next ready job in the currently used tube.
    pub async fn peek_ready(&mut self) -> io::Result<(u64, String)> {
        self.send_line("peek-ready").await?;
        self.read_job_response("FOUND").await
    }

    /// Peek at the next buried job in the currently used tube.
    pub async fn peek_buried(&mut self) -> io::Result<(u64, String)> {
        self.send_line("peek-buried").await?;
        self.read_job_response("FOUND").await
    }

    /// Peek at the next delayed job in the currently used tube.
    pub async fn peek_delayed(&mut self) -> io::Result<(u64, String)> {
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
        self.send_line(&format!("stats-group {name}")).await?;
        let body = self.read_ok_body().await?;
        Ok(GroupStats::from_yaml(&body))
    }

    /// Flush all jobs from a tube.
    /// Sends: `flush-tube <tube>\r\n` — expects: `FLUSHED <count>\r\n`
    pub async fn flush_tube(&mut self, tube: &str) -> io::Result<u64> {
        self.send_line(&format!("flush-tube {tube}")).await?;
        self.read_u64_response("FLUSHED").await
    }

    /// Flush only the buried jobs from a tube; ready, delayed, and reserved
    /// jobs are left untouched.
    /// Sends: `flush-buried <tube>\r\n` — expects: `FLUSHED <count>\r\n`
    pub async fn flush_buried(&mut self, tube: &str) -> io::Result<u64> {
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
        self.send_line(&format!("pause-tube {tube} {delay}")).await?;
        self.expect_response("PAUSED").await
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
    async fn read_job_response(&mut self, prefix: &str) -> io::Result<(u64, String)> {
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
        let mut buf = vec![0u8; bytes];
        self.reader.read_exact(&mut buf).await?;
        // Consume trailing \r\n
        let mut trail = [0u8; 2];
        self.reader.read_exact(&mut trail).await?;
        let body = String::from_utf8(buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok((id, body))
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
}
