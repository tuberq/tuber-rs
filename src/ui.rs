use crate::app::App;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(4),  // top bar
        Constraint::Min(5),    // tube chart
        Constraint::Length(5), // bottom panel
    ])
    .split(frame.area());

    render_top_bar(frame, app, chunks[0]);
    render_tube_chart(frame, app, chunks[1]);
    render_bottom_panel(frame, app, chunks[2]);
}

fn render_top_bar(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .title(" tuber-tui ");

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
        let p = Paragraph::new(" Connecting to tuber...").block(block);
        frame.render_widget(p, area);
        return;
    };

    let s = &snap.server;
    let uptime = format_uptime(s.uptime);

    let mut line1_spans = vec![
        Span::styled(" v", Style::default().fg(Color::DarkGray)),
        Span::raw(&s.version),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::raw(format!("up {uptime}")),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(
            "conns: {} (P:{} W:{} Wt:{})",
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

    let line2 = Line::from(vec![
        Span::styled(" CPU: ", Style::default().fg(Color::DarkGray)),
        Span::raw(format!("u={:.2} s={:.2}", s.rusage_utime, s.rusage_stime)),
        Span::styled(" | ", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(
            "jobs: {} ready, {} reserved, {} delayed, {} buried",
            s.current_jobs_ready, s.current_jobs_reserved, s.current_jobs_delayed, s.current_jobs_buried
        )),
    ]);

    let text = vec![Line::from(line1_spans), line2];
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

    // Available width for bars (after name + spacing + total number)
    let bar_width = inner.width.saturating_sub(max_name_len as u16 + 2 + 10) as usize;

    let mut lines = Vec::new();
    for tube in snap.tubes.iter().take(chart_height) {
        let name = if tube.name.len() > max_name_len {
            &tube.name[..max_name_len]
        } else {
            &tube.name
        };
        let padded_name = format!("{:>width$} ", name, width = max_name_len);

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

        // log10 scaling for bar segments
        let log_ready = (ready as f64 + 1.0).log10();
        let log_reserved = (reserved as f64 + 1.0).log10();
        let log_delayed = (delayed as f64 + 1.0).log10();
        let log_buried = (buried as f64 + 1.0).log10();
        // Subtract the baseline log10(1)=0 for zero counts
        let log_total = log_ready + log_reserved + log_delayed + log_buried;

        let (w_ready, w_reserved, w_delayed, w_buried) = if log_total > 0.0 && bar_width > 0 {
            let scale = bar_width as f64 / log_total;
            let wr = (log_ready * scale).round() as usize;
            let wres = (log_reserved * scale).round() as usize;
            let wd = (log_delayed * scale).round() as usize;
            let wb = bar_width.saturating_sub(wr + wres + wd);
            (wr, wres, wd, wb)
        } else {
            (bar_width, 0, 0, 0)
        };

        let mut spans = vec![Span::styled(padded_name, Style::default().fg(Color::White))];

        if w_ready > 0 {
            spans.push(Span::styled(
                "█".repeat(w_ready),
                Style::default().fg(Color::Green),
            ));
        }
        if w_reserved > 0 {
            spans.push(Span::styled(
                "█".repeat(w_reserved),
                Style::default().fg(Color::Yellow),
            ));
        }
        if w_delayed > 0 {
            spans.push(Span::styled(
                "█".repeat(w_delayed),
                Style::default().fg(Color::Blue),
            ));
        }
        if w_buried > 0 {
            spans.push(Span::styled(
                "█".repeat(w_buried),
                Style::default().fg(Color::Red),
            ));
        }

        spans.push(Span::raw(total_display));

        // EWMA if available
        if tube.processing_time_ewma > 0.0 {
            spans.push(Span::styled(
                format!(" ({:.1}ms)", tube.processing_time_ewma),
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
    ]);

    let line2 = Line::from(vec![
        Span::styled(" Timeouts: ", Style::default().fg(Color::DarkGray)),
        Span::raw(format!("{:.1}/s", timeouts_s)),
        Span::styled("   EWMA: ", Style::default().fg(Color::DarkGray)),
        Span::raw(
            snap.tubes
                .iter()
                .filter(|t| t.processing_time_ewma > 0.0)
                .map(|t| format!("{} {:.1}ms", t.name, t.processing_time_ewma))
                .collect::<Vec<_>>()
                .join(", "),
        ),
    ]);

    let total_buried: u64 = snap.tubes.iter().map(|t| t.current_jobs_buried).sum();
    let buried_style = if total_buried > 0 {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Green)
    };
    let line3 = Line::from(vec![
        Span::styled(" Buried: ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{total_buried} total"), buried_style),
    ]);

    let text = vec![line1, line2, line3];
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

fn format_number(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
