use crate::model::{ServerStats, Snapshot, TubeStats};
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
