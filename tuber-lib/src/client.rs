use crate::model::{GroupStats, JobStats, ServerStats, Snapshot, TubeStats};
use crate::parse::parse_yaml_list;
use std::io;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

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
        // Sort tubes by total_jobs descending
        tubes.sort_by(|a, b| b.total_jobs.cmp(&a.total_jobs));
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
        let cmd = format!("put {} {} {} {}\r\n", priority, delay, ttr, data.len());
        self.writer.write_all(cmd.as_bytes()).await?;
        self.writer.write_all(data).await?;
        self.writer.write_all(b"\r\n").await?;
        self.writer.flush().await?;
        let line = self.read_line().await?;
        if let Some(id_str) = line.strip_prefix("INSERTED ") {
            id_str.parse().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        } else {
            Err(io::Error::other(line))
        }
    }

    /// Reserve the next available job, waiting up to `timeout` seconds.
    /// Sends: `reserve-with-timeout <seconds>\r\n` — expects: `RESERVED <id> <bytes>\r\n<data>\r\n`
    pub async fn reserve(&mut self, timeout: u32) -> io::Result<(u64, String)> {
        self.send_line(&format!("reserve-with-timeout {timeout}")).await?;
        self.read_job_response("RESERVED").await
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

    /// Bury a reserved job.
    /// Sends: `bury <id> <priority>\r\n` — expects: `BURIED\r\n`
    pub async fn bury(&mut self, id: u64, priority: u32) -> io::Result<()> {
        self.send_line(&format!("bury {id} {priority}")).await?;
        self.expect_response("BURIED").await
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
