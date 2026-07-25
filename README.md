# tuber-rs

Rust client tools for [tuber](https://github.com/tuberq/tuber), a fast work queue server (beanstalkd-compatible).

This workspace contains three crates:

- **tuber-cli** — command-line client with JSON output, designed for scripting and AI agents
- **tuber-tui** — real-time terminal dashboard for monitoring queues
- **tuber-lib** — shared protocol client library (internal)

## tuber-cli

An agent-friendly CLI for interacting with tuber/beanstalkd queues. Outputs JSON by default.

### Install

```
brew install tuberq/tuber/tuber-cli
```

Or build from source:

```
cargo install --path tuber-cli
```

### Usage

```bash
# Server stats
tuber-cli stats

# List all tubes
tuber-cli list-tubes

# Tube stats
tuber-cli stats-tube emails

# Put a job
tuber-cli put --tube emails "send user@example.com"

# Put from stdin
echo '{"user": "alice"}' | tuber-cli put --tube notifications

# Tuber extensions: idempotent put, job groups, fan-in, concurrency limits
tuber-cli put --tube emails --idempotent welcome-42 "send welcome"
tuber-cli put --tube work --group batch-1 "shard 1"
tuber-cli put --tube work --after batch-1 "cleanup"
tuber-cli put --tube work --concurrency user-42 "resize avatar"

# Reserve a job
tuber-cli reserve --timeout 5

# Delete a job
tuber-cli delete 42

# Kick buried jobs (all, or one by ID)
tuber-cli kick 10 --tube emails
tuber-cli kick-job 42

# Peek at a job
tuber-cli peek 42

# Drain mode: reject new puts, let in-flight work finish
tuber-cli drain
tuber-cli undrain

# Text output instead of JSON
tuber-cli stats --format text
```

Note that a reservation belongs to the connection that made it, and each
`tuber-cli` run is a new connection. So `reserve` then `bury` as two commands
won't work — the reserve is dropped on exit. Use `tuber-lib` (or one `nc`
session) to reserve and finish a job together; see `skill/SKILL.md`.

### Options

| Option | Default | Description |
|---|---|---|
| `-a, --addr` | `$TUBER_ADDR` or `localhost:11300` | Server address |
| `-f, --format` | `json` | Output format: `json` or `text` |

## tuber-tui

A real-time terminal dashboard for monitoring tuber queues.

![tuber-tui screenshot](screenshots/tui.png)

### Features

- Live server stats: version, uptime, connections, CPU usage, drain status
- Per-tube stacked bar chart with log-scaled segments for ready, reserved, delayed, and buried jobs
- Throughput rates: puts/s, reserves/s, deletes/s, timeouts/s
- Bimodal processing time EWMA, percentiles (p50/p95/p99), and queue time per tube
- Queue growth indicators
- Buried job highlighting
- Auto-reconnect on connection loss

### Install

```
brew install tuberq/tuber/tuber-tui
```

Or build from source:

```
cargo install --path tuber-tui
```

### Usage

```bash
tuber-tui                        # connects to TUBER_TUI or localhost:11300
tuber-tui staging.example.com    # custom host
tuber-tui :11301                 # custom port
tuber-tui -i 0.5                 # faster polling (0.5s)
```

| Option | Default | Description |
|---|---|---|
| `[HOST]` positional | `$TUBER_ADDR` or `localhost:11300` | Server address |
| `-i, --interval` | `1.5` | Poll interval (seconds) |

### Keys

| Key | Action |
|---|---|
| `↑` `↓` / `k` `j` | Move the tube cursor |
| `p` | Pause the selected tube — prompts for a duration |
| `u` | Unpause the selected tube immediately |
| `s` | Cycle sort: by name (default), then by size |
| `e` | Show/hide empty tubes (no jobs in any state) |
| `q` | Quit |

Empty tubes are hidden on startup, since most tubes on a busy server are empty
most of the time. The title reports how many are out of view — ` Tubes (name ·
11 hidden) ` — and `e` brings them back.

Pause durations are seconds by default, matching `tuber-cli pause --delay`.
`m` and `h` suffixes scale from there, so `90`, `5m` and `2h` are all valid, and
`0` unpauses. A paused tube carries a `PAUSED 12m` badge showing the time left,
and stays on screen even when it has drained and empty tubes are hidden — a
paused tube at zero is exactly the one you need to see.

### Layout

```
+------------------------------------------------------+
| Top bar: version, uptime, connections, CPU, drain    |
+------------------------------------------------------+
| Tube bar chart (log-scaled, color-coded)             |
|                                                      |
|  emails   ████████████████████████           12,345  |
|  webhooks ██████████████                        567  |
|  default  ████                                   23  |
|                                                      |
|  █ Ready  █ Reserved  █ Delayed  █ Buried            |
+------------------------------------------------------+
| Throughput, EWMA, percentiles, buried count          |
+------------------------------------------------------+
```

## Requirements

- A running [tuber](https://github.com/tuberq/tuber) server (or any beanstalkd-compatible server)
- Rust 1.75+ (for building from source)

## License

MIT
