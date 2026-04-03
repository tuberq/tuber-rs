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

# Reserve a job
tuber-cli reserve --timeout 5

# Delete a job
tuber-cli delete 42

# Kick buried jobs
tuber-cli kick 10 --tube emails

# Peek at a job
tuber-cli peek 42

# Text output instead of JSON
tuber-cli stats --format text
```

### Options

| Option | Default | Description |
|---|---|---|
| `-a, --addr` | `localhost:11300` | Server address |
| `-f, --format` | `json` | Output format: `json` or `text` |
| `TUBER_ADDR` env var | — | Fallback server address (useful in `.envrc`) |

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
tuber-tui                        # connects to localhost:11300
tuber-tui staging.example.com    # custom host
tuber-tui :11301                 # custom port
tuber-tui -i 0.5                 # faster polling (0.5s)
```

| Option | Default | Description |
|---|---|---|
| `[HOST]` positional | `localhost:11300` | Server address |
| `-i, --interval` | `1.5` | Poll interval (seconds) |
| `TUBER_ADDR` env var | — | Fallback server address |

Press `q` to quit.

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
