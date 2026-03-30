use crate::app::App;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

pub fn render(frame: &mut Frame, app: &App) {
    // Bottom panel: 3 fixed lines + 1 per tube with timing data + 2 for border/padding
    let tubes_with_timing = app
        .current
        .as_ref()
        .map(|s| s.tubes.iter().filter(|t| t.processing_time_ewma > 0.0).count())
        .unwrap_or(0);
    let header_row = if tubes_with_timing > 0 { 1 } else { 0 };
    let bottom_height = 5 + header_row + tubes_with_timing as u16;

    let chunks = Layout::vertical([
        Constraint::Length(4),         // top bar
        Constraint::Min(5),            // tube chart
        Constraint::Length(bottom_height), // bottom panel
    ])
    .split(frame.area());

    render_top_bar(frame, app, chunks[0]);
    render_tube_chart(frame, app, chunks[1]);
    render_bottom_panel(frame, app, chunks[2]);
}

fn render_top_bar(frame: &mut Frame, app: &App, area: Rect) {
    let title = format!(" tuber-tui v{} ", env!("CARGO_PKG_VERSION"));

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .title(title);

    if let Some(ref err) = app.error {
        let text = vec![
            Line::from(Span::styled(
                format!(" Error: {err}"),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )),
            Line::from(" Reconnecting..."),
        ];
        let p = Paragraph::new(text).block(block);
        frame.render_widget(p, area);
        return;
    }

    let Some(snap) = &app.current else {
        let p = Paragraph::new(" Connecting...").block(block);
        frame.render_widget(p, area);
        return;
    };

    let s = &snap.server;
    let uptime = format_uptime(s.uptime);

    let server_version = if s.version.starts_with("tuber") {
        s.version.clone()
    } else {
        format!("beanstalkd {}", s.version)
    };

    let server_label = match snap.server.name.as_str() {
        "" => server_version,
        name => format!("{server_version} — {name}"),
    };

    let mut line1_spans = vec![
        Span::styled(" ", Style::default().fg(Color::DarkGray)),
        Span::raw(server_label),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::raw(format!("up {uptime}")),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(
            "conns: {} (producers:{} workers:{} waiting:{})",
            s.current_connections, s.current_producers, s.current_workers, s.current_waiting
        )),
    ];

    if s.draining {
        line1_spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
        line1_spans.push(Span::styled(
            "DRAINING",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }

    let mut line2_spans = vec![
        Span::styled(" CPU: ", Style::default().fg(Color::DarkGray)),
        Span::raw(format!("u={:.2} s={:.2}", s.rusage_utime, s.rusage_stime)),
    ];

    if s.rusage_maxrss > 0 {
        line2_spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
        line2_spans.push(Span::styled("RSS: ", Style::default().fg(Color::DarkGray)));
        line2_spans.push(Span::raw(format_bytes(s.rusage_maxrss)));
    }

    line2_spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
    line2_spans.push(Span::raw(format!(
        "jobs: {} ready, {} reserved, {} delayed, {} buried",
        s.current_jobs_ready, s.current_jobs_reserved, s.current_jobs_delayed, s.current_jobs_buried
    )));

    if s.binlog_enabled {
        line2_spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
        line2_spans.push(Span::styled("WAL: ", Style::default().fg(Color::DarkGray)));
        line2_spans.push(Span::raw(format!(
            "{} ({} files)",
            format_bytes(s.binlog_total_bytes),
            s.binlog_file_count
        )));
    }

    let text = vec![Line::from(line1_spans), Line::from(line2_spans)];
    let p = Paragraph::new(text).block(block);
    frame.render_widget(p, area);
}

fn render_tube_chart(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .title(" Tubes ");

    let Some(snap) = &app.current else {
        frame.render_widget(block, area);
        return;
    };

    if snap.tubes.is_empty() {
        let p = Paragraph::new(" No tubes").block(block);
        frame.render_widget(p, area);
        return;
    }

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Reserve last line for legend
    let chart_height = inner.height.saturating_sub(1) as usize;
    let chart_area = Rect {
        height: chart_height as u16,
        ..inner
    };
    let legend_area = Rect {
        y: inner.y + chart_height as u16,
        height: 1,
        ..inner
    };

    // Find max name length for alignment
    let max_name_len = snap
        .tubes
        .iter()
        .map(|t| t.name.len())
        .max()
        .unwrap_or(0)
        .min(20);

    // Available width for bars (name + space + total + ewma suffix)
    // Reserve 20 extra chars for " (12.3ms)" suffix and padding
    let bar_width = inner.width.saturating_sub(max_name_len as u16 + 2 + 8 + 12) as usize;

    let mut sorted_tubes: Vec<&_> = snap.tubes.iter().collect();
    sorted_tubes.sort_by(|a, b| {
        let total_a = a.current_jobs_ready + a.current_jobs_reserved + a.current_jobs_delayed + a.current_jobs_buried;
        let total_b = b.current_jobs_ready + b.current_jobs_reserved + b.current_jobs_delayed + b.current_jobs_buried;
        total_b.cmp(&total_a)
    });

    let mut lines = Vec::new();
    for tube in sorted_tubes.iter().take(chart_height) {
        let padded_name = format!("{:>width$} ", truncate_name(&tube.name, max_name_len), width = max_name_len);

        let ready = tube.current_jobs_ready;
        let reserved = tube.current_jobs_reserved;
        let delayed = tube.current_jobs_delayed;
        let buried = tube.current_jobs_buried;
        let total = ready + reserved + delayed + buried;

        let total_display = format!(" {:>7}", format_number(total));

        if total == 0 {
            let mut spans = vec![
                Span::styled(padded_name, Style::default().fg(Color::White)),
                Span::styled(
                    "·".repeat(bar_width.min(3)),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            spans.push(Span::styled(total_display, Style::default().fg(Color::DarkGray)));
            lines.push(Line::from(spans));
            continue;
        }

        // Build per-status data: (count, label, fg_color, bg_color)
        let statuses: [(u64, Color, Color); 4] = [
            (ready, Color::Black, Color::Green),
            (reserved, Color::Black, Color::Yellow),
            (delayed, Color::White, Color::Blue),
            (buried, Color::White, Color::Red),
        ];

        // For each non-zero status, compute label and minimum width
        let mut segments: Vec<(String, usize, Color, Color)> = Vec::new();
        let mut log_values: Vec<f64> = Vec::new();
        for &(count, fg, bg) in &statuses {
            if count > 0 {
                let label = format_number(count);
                let min_w = label.len();
                segments.push((label, min_w, fg, bg));
                log_values.push((count as f64 + 1.0).log10());
            }
        }

        // Allocate widths: each segment gets at least min_width, remaining space distributed by log10
        let total_min: usize = segments.iter().map(|(_, mw, _, _)| *mw).sum();
        let mut widths: Vec<usize> = segments.iter().map(|(_, mw, _, _)| *mw).collect();

        if bar_width > total_min {
            let remaining = bar_width - total_min;
            let log_total: f64 = log_values.iter().sum();
            if log_total > 0.0 {
                let scale = remaining as f64 / log_total;
                let mut distributed = 0usize;
                for (i, lv) in log_values.iter().enumerate() {
                    if i < widths.len() - 1 {
                        let extra = (lv * scale).round() as usize;
                        widths[i] += extra;
                        distributed += extra;
                    }
                }
                // Give remainder to last segment
                if let Some(last) = widths.last_mut() {
                    *last += remaining.saturating_sub(distributed);
                }
            }
        } else if bar_width < total_min && !segments.is_empty() {
            // Truncate proportionally but ensure at least 1 char per segment
            let scale = bar_width as f64 / total_min as f64;
            let mut used = 0usize;
            for (i, w) in widths.iter_mut().enumerate() {
                if i < segments.len() - 1 {
                    *w = (*w as f64 * scale).round().max(1.0) as usize;
                    used += *w;
                }
            }
            if let Some(last) = widths.last_mut() {
                *last = bar_width.saturating_sub(used).max(1);
            }
        }

        let mut spans = vec![Span::styled(padded_name, Style::default().fg(Color::White))];

        for (i, (label, _min_w, fg, bg)) in segments.iter().enumerate() {
            let w = widths[i];
            let text = if w >= label.len() {
                let pad_total = w - label.len();
                let pad_left = pad_total / 2;
                let pad_right = pad_total - pad_left;
                format!("{}{}{}", " ".repeat(pad_left), label, " ".repeat(pad_right))
            } else {
                // Segment too narrow for full label, show what fits
                label[..w].to_string()
            };
            spans.push(Span::styled(
                text,
                Style::default().fg(*fg).bg(*bg),
            ));
        }

        spans.push(Span::raw(total_display));

        // EWMA if available
        if tube.processing_time_ewma > 0.0 {
            spans.push(Span::styled(
                format!(" ({})", format_duration(tube.processing_time_ewma)),
                Style::default().fg(Color::DarkGray),
            ));
        }

        lines.push(Line::from(spans));
    }

    let chart = Paragraph::new(lines);
    frame.render_widget(chart, chart_area);

    // Legend
    let legend = Line::from(vec![
        Span::styled(" █", Style::default().fg(Color::Green)),
        Span::raw(" Ready  "),
        Span::styled("█", Style::default().fg(Color::Yellow)),
        Span::raw(" Reserved  "),
        Span::styled("█", Style::default().fg(Color::Blue)),
        Span::raw(" Delayed  "),
        Span::styled("█", Style::default().fg(Color::Red)),
        Span::raw(" Buried"),
    ]);
    frame.render_widget(Paragraph::new(vec![legend]), legend_area);
}

fn render_bottom_panel(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::TOP).title(" Stats ");

    let Some(snap) = &app.current else {
        frame.render_widget(block, area);
        return;
    };

    let (puts_s, reserves_s, deletes_s, timeouts_s) = app.server_rates();

    let line1 = Line::from(vec![
        Span::styled(" Throughput: ", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(
            "{:.1} puts/s  {:.1} reserves/s  {:.1} deletes/s",
            puts_s, reserves_s, deletes_s
        )),
        Span::styled("  (", Style::default().fg(Color::DarkGray)),
        Span::raw(format!("{} total", format_number(snap.server.total_jobs))),
        Span::styled(")", Style::default().fg(Color::DarkGray)),
    ]);

    // Single pass: collect buried total, timing tubes, and max name length
    let mut total_buried: u64 = 0;
    let mut max_name_len: usize = 0;
    let mut timing_tubes: Vec<&crate::model::TubeStats> = Vec::new();
    for t in &snap.tubes {
        total_buried += t.current_jobs_buried;
        if t.processing_time_ewma > 0.0 {
            max_name_len = max_name_len.max(t.name.len());
            timing_tubes.push(t);
        }
    }
    max_name_len = max_name_len.min(20);

    let buried_style = if total_buried > 0 {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Green)
    };

    let line2 = Line::from(vec![
        Span::styled(" Timeouts: ", Style::default().fg(Color::DarkGray)),
        Span::raw(format!("{:.1}/s", timeouts_s)),
        Span::styled("  Buried: ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{total_buried} total"), buried_style),
    ]);

    let mut text = vec![line1, line2];

    if !timing_tubes.is_empty() {
        let thresh = format_duration(snap.server.processing_time_fast_threshold);
        let header = Line::from(vec![
            Span::styled(
                format!(" {:>width$} ", "ewma", width = max_name_len),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("<{thresh} | >{thresh}"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled("    p50 |   p95 |   p99", Style::default().fg(Color::DarkGray)),
        ]);
        text.push(header);

        const DASH: &str = "    \u{2014}"; // right-aligned em-dash, 5 display columns
        for tube in &timing_tubes {
            let mut spans = vec![
                Span::styled(
                    format!(" {:>width$} ", truncate_name(&tube.name, max_name_len), width = max_name_len),
                    Style::default().fg(Color::White),
                ),
            ];

            // Bimodal EWMA
            let fast = if tube.processing_time_samples_fast > 0 {
                format_duration(tube.processing_time_ewma_fast)
            } else {
                DASH.to_string()
            };
            let slow = if tube.processing_time_samples_slow > 0 {
                format_duration(tube.processing_time_ewma_slow)
            } else {
                DASH.to_string()
            };
            spans.push(Span::raw(format!("{} | {}", fast, slow)));

            // Percentiles
            if tube.processing_time_p50 > 0.0 {
                spans.push(Span::raw(format!(
                    "  {} | {} | {}",
                    format_duration(tube.processing_time_p50),
                    format_duration(tube.processing_time_p95),
                    format_duration(tube.processing_time_p99),
                )));
            }

            text.push(Line::from(spans));
        }
    }

    let p = Paragraph::new(text).block(block);
    frame.render_widget(p, area);
}

fn format_uptime(seconds: u64) -> String {
    let days = seconds / 86400;
    let hours = (seconds % 86400) / 3600;
    let mins = (seconds % 3600) / 60;
    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1}GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1}MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}B")
    }
}

fn truncate_name(name: &str, max_len: usize) -> &str {
    if name.len() > max_len {
        &name[..max_len]
    } else {
        name
    }
}

fn format_duration(seconds: f64) -> String {
    let raw = if seconds < 1.0 {
        format!("{:.0}ms", seconds * 1000.0)
    } else {
        format!("{:.1}s", seconds)
    };
    format!("{:>5}", raw)
}

fn format_number(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
