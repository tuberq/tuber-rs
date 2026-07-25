use crate::app::{format_secs, parse_duration, App, Mode};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

/// Chart rows consumed by its own chrome: bottom border + legend line.
const CHART_OVERHEAD: usize = 2;
/// Tube rows the chart keeps even when the timing table wants more space.
const CHART_MIN_ROWS: usize = 4;
/// Stats panel rows that are always present: top border, two summary lines,
/// and the padding the original layout allowed for.
const STATS_FIXED: usize = 5;

/// Split the space below the top bar between the tube chart and the timing
/// table. The chart is the primary widget and is served first; the timing
/// table takes what is left over. Returns `(bottom_height, timing_rows)`.
///
/// This used to be the other way round — the timing table asked for one row
/// per tube as a fixed `Length`, so on a short terminal it consumed the screen
/// and squeezed the chart to its minimum.
fn split_bottom(avail: usize, visible_tubes: usize, timing_tubes: usize) -> (usize, usize) {
    let chart_floor = CHART_MIN_ROWS + CHART_OVERHEAD;

    // The chart is served first, but it cannot claim the whole screen just
    // because it has a lot of tubes — it scrolls, so it does not need a row
    // per tube. Two thirds, then the timing table gets the remainder.
    let chart_cap = (avail * 2 / 3).max(chart_floor);
    let chart_want = (visible_tubes + CHART_OVERHEAD).clamp(chart_floor, chart_cap);

    // The chart never gives up its floor, so this is a hard ceiling.
    let ceiling = avail.saturating_sub(chart_floor);
    if ceiling < STATS_FIXED {
        return (ceiling, 0);
    }

    let leftover = avail.saturating_sub(chart_want);
    let mut rows = timing_tubes.min(leftover.saturating_sub(STATS_FIXED));
    // One or two rows plus a "+N more" marker tells you nothing; spend the
    // space on the chart instead.
    if rows < 3 && rows < timing_tubes {
        rows = 0;
    }
    let header = usize::from(rows > 0);
    let height = (STATS_FIXED + header + rows).min(ceiling);
    let rows = rows.min(height.saturating_sub(STATS_FIXED + header));
    (height, rows)
}

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let has_storage_line = app
        .current
        .as_ref()
        .map(|s| {
            let srv = &s.server;
            srv.binlog_enabled
                || srv.toast_segments > 0
                || srv.toast_total_bytes > 0
                || srv.max_storage_bytes > 0
        })
        .unwrap_or(false);
    let top_height = if has_storage_line { 5 } else { 4 };

    let (bottom_height, timing_rows) = split_bottom(
        area.height.saturating_sub(top_height) as usize,
        app.visible_tubes().len(),
        app.timing_tubes().len(),
    );

    let chunks = Layout::vertical([
        Constraint::Length(top_height),              // top bar
        Constraint::Min(CHART_MIN_ROWS as u16),      // tube chart
        Constraint::Length(bottom_height as u16),    // bottom panel
    ])
    .split(area);

    render_top_bar(frame, app, chunks[0]);
    render_tube_chart(frame, app, chunks[1]);
    render_bottom_panel(frame, app, chunks[2], timing_rows);
    render_prompt_overlay(frame, app, area);
}

/// The pause prompt and command results share the last row of the frame, drawn
/// over whatever is beneath so opening the prompt does not reflow the layout.
fn render_prompt_overlay(frame: &mut Frame, app: &App, area: Rect) {
    let line = match &app.mode {
        Mode::PausePrompt { tube, buf } => {
            let mut spans = vec![
                Span::styled(
                    " Pause ",
                    Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(" {tube} for: ")),
                Span::styled(
                    buf.clone(),
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                ),
                Span::styled("\u{2588}", Style::default().fg(Color::DarkGray)),
            ];
            // The hint is the only feedback on a bad duration — Enter on
            // unparseable input is a deliberate no-op, so this must say why.
            let (hint, hint_style) = match parse_duration(buf) {
                Some(0) => ("  = unpause now".to_string(), Color::DarkGray),
                Some(secs) => (format!("  = {}", format_secs(secs as u64)), Color::DarkGray),
                None if buf.is_empty() => ("  seconds, or 5m / 2h".to_string(), Color::DarkGray),
                None => ("  not a duration".to_string(), Color::Red),
            };
            spans.push(Span::styled(hint, Style::default().fg(hint_style)));
            spans.push(Span::styled(
                "   Enter confirm  Esc cancel",
                Style::default().fg(Color::DarkGray),
            ));
            Some(Line::from(spans))
        }
        Mode::Normal => app.live_status().map(|status| {
            let style = if status.error {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Green)
            };
            Line::from(vec![Span::styled(format!(" {}", status.msg), style)])
        }),
    };

    let Some(line) = line else { return };
    if area.height == 0 {
        return;
    }
    let row = Rect {
        x: area.x,
        y: area.y + area.height - 1,
        width: area.width,
        height: 1,
    };
    frame.render_widget(Clear, row);
    frame.render_widget(Paragraph::new(vec![line]), row);
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

    if s.max_jobs_size > 0 {
        let pct = s.current_jobs_size as f64 / s.max_jobs_size as f64 * 100.0;
        let color = if pct >= 90.0 {
            Color::Red
        } else if pct >= 70.0 {
            Color::Yellow
        } else {
            Color::Green
        };
        line2_spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
        line2_spans.push(Span::styled("Mem: ", Style::default().fg(Color::DarkGray)));
        line2_spans.push(Span::styled(
            format!("{} / {} ({:.1}%)", format_bytes(s.current_jobs_size), format_bytes(s.max_jobs_size), pct),
            Style::default().fg(color),
        ));
    } else if s.current_jobs_size > 0 {
        line2_spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
        line2_spans.push(Span::styled("Mem: ", Style::default().fg(Color::DarkGray)));
        line2_spans.push(Span::raw(format_bytes(s.current_jobs_size)));
    }

    line2_spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
    line2_spans.push(Span::raw(format!(
        "jobs: {} ready, {} reserved, {} delayed, {} buried",
        s.current_jobs_ready, s.current_jobs_reserved, s.current_jobs_delayed, s.current_jobs_buried
    )));

    let mut line3_spans: Vec<Span> = Vec::new();

    if s.binlog_enabled {
        line3_spans.push(Span::styled(" WAL: ", Style::default().fg(Color::DarkGray)));
        line3_spans.push(Span::raw(format!(
            "{} ({} files)",
            format_bytes(s.binlog_total_bytes),
            s.binlog_file_count
        )));
    }

    if s.toast_segments > 0 || s.toast_total_bytes > 0 {
        if line3_spans.is_empty() {
            line3_spans.push(Span::styled(" TOAST: ", Style::default().fg(Color::DarkGray)));
        } else {
            line3_spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
            line3_spans.push(Span::styled("TOAST: ", Style::default().fg(Color::DarkGray)));
        }
        let live_frag = if s.toast_total_bytes > 0 {
            let pct = s.toast_live_bytes as f64 / s.toast_total_bytes as f64 * 100.0;
            format!(
                "{} / {} ({:.0}% live, {} segs)",
                format_bytes(s.toast_live_bytes),
                format_bytes(s.toast_total_bytes),
                pct,
                s.toast_segments
            )
        } else {
            format!("{} segs", s.toast_segments)
        };
        line3_spans.push(Span::raw(live_frag));
    }

    if s.max_storage_bytes > 0 {
        let used = s.binlog_total_bytes + s.toast_total_bytes;
        let pct = used as f64 / s.max_storage_bytes as f64 * 100.0;
        let color = if pct >= 90.0 {
            Color::Red
        } else if pct >= 70.0 {
            Color::Yellow
        } else {
            Color::Green
        };
        if line3_spans.is_empty() {
            line3_spans.push(Span::raw(" "));
        } else {
            line3_spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
        }
        line3_spans.push(Span::styled("Disk: ", Style::default().fg(Color::DarkGray)));
        line3_spans.push(Span::styled(
            format!(
                "{} / {} ({:.1}%)",
                format_bytes(used),
                format_bytes(s.max_storage_bytes),
                pct
            ),
            Style::default().fg(color),
        ));
    }

    let mut text = vec![Line::from(line1_spans), Line::from(line2_spans)];
    if !line3_spans.is_empty() {
        text.push(Line::from(line3_spans));
    }
    let p = Paragraph::new(text).block(block);
    frame.render_widget(p, area);
}

fn render_tube_chart(frame: &mut Frame, app: &App, area: Rect) {
    // Key hints live in the title rather than the legend: the legend is nearly
    // 52 columns of colour swatches on its own and would push them off an
    // 80-column terminal, where they are needed most. Kept tight enough that
    // the whole title still fits in 80 columns with a count present.
    let hidden = app.hidden_count();
    let sort = app.sort.label();
    // The label says what the key will do next, not what state we are in.
    let toggle = if app.hide_empty {
        "e:show empty"
    } else {
        "e:hide empty"
    };
    let scope = if hidden > 0 {
        format!("{sort} \u{00b7} {hidden} hidden")
    } else {
        sort.to_string()
    };
    let title =
        format!(" Tubes ({scope}) \u{2191}\u{2193} p:pause u:unpause s:sort {toggle} q:quit ");
    let block = Block::default().borders(Borders::BOTTOM).title(title);

    let tubes = app.visible_tubes();

    if app.current.is_none() {
        frame.render_widget(block, area);
        return;
    }

    if tubes.is_empty() {
        let msg = if app.hide_empty {
            " No tubes with jobs (press e to show empty)"
        } else {
            " No tubes"
        };
        let p = Paragraph::new(msg).block(block);
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

    // Scroll the window just far enough to keep the cursor on screen. Derived
    // from the selection rather than stored, so it cannot drift out of sync
    // with a list that is refiltered and resorted on every poll.
    let selected = app.selected_index();
    let offset = match selected {
        Some(i) if chart_height > 0 && i >= chart_height => i + 1 - chart_height,
        _ => 0,
    };

    // Find max name length for alignment
    let max_name_len = tubes.iter().map(|t| t.name.len()).max().unwrap_or(0).min(20);

    // Reserve space for the pause badge only when something is actually paused,
    // so the common case keeps its full bar width.
    let pause_reserve = if tubes.iter().any(|t| t.is_paused()) { 12 } else { 0 };

    // Available width for bars: cursor + name + space + total + ewma suffix.
    // Reserve 12 extra chars for the " (12.3ms)" suffix and padding.
    let bar_width = inner
        .width
        .saturating_sub(CURSOR_WIDTH + max_name_len as u16 + 2 + 8 + 12 + pause_reserve)
        as usize;

    let mut lines = Vec::new();
    for (row, tube) in tubes.iter().enumerate().skip(offset).take(chart_height) {
        let is_selected = selected == Some(row);
        let cursor = Span::styled(
            if is_selected { "\u{258c}" } else { " " },
            Style::default().fg(Color::Cyan),
        );
        let name_style = if is_selected {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let padded_name = format!("{:>width$} ", truncate_name(&tube.name, max_name_len), width = max_name_len);

        let urgent = tube.current_jobs_urgent;
        let ready = tube.current_jobs_ready.saturating_sub(urgent);
        let reserved = tube.current_jobs_reserved;
        let delayed = tube.current_jobs_delayed;
        let buried = tube.current_jobs_buried;
        let total = tube.current_total();

        let total_display = format!(" {:>7}", format_number(total));

        if total == 0 {
            let mut spans = vec![
                cursor,
                Span::styled(padded_name, name_style),
                Span::styled(
                    "·".repeat(bar_width.min(3)),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            spans.push(Span::styled(total_display, Style::default().fg(Color::DarkGray)));
            if let Some(prev_total) = app.previous_tube_total(&tube.name) {
                let (chevron, color) = queue_change_indicator(prev_total, total);
                spans.push(Span::styled(format!(" {chevron}"), Style::default().fg(color)));
            }
            if let Some(badge) = pause_badge(tube) {
                spans.push(badge);
            }
            lines.push(Line::from(spans));
            continue;
        }

        // Build per-status data: (count, label, fg_color, bg_color)
        let orange = Color::Rgb(255, 165, 0);
        let statuses: [(u64, Color, Color); 5] = [
            (ready, Color::Black, Color::Green),
            (urgent, Color::Black, orange),
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

        let mut spans = vec![cursor, Span::styled(padded_name, name_style)];

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

        // Queue growth indicator
        if let Some(prev_total) = app.previous_tube_total(&tube.name) {
            let (chevron, color) = queue_change_indicator(prev_total, total);
            if !chevron.is_empty() {
                spans.push(Span::styled(format!(" {chevron}"), Style::default().fg(color)));
            }
        }

        // EWMA if available
        if tube.processing_time_ewma > 0.0 {
            spans.push(Span::styled(
                format!(" ({})", format_duration(tube.processing_time_ewma)),
                Style::default().fg(Color::DarkGray),
            ));
        }

        if let Some(badge) = pause_badge(tube) {
            spans.push(badge);
        }

        lines.push(Line::from(spans));
    }

    let chart = Paragraph::new(lines);
    frame.render_widget(chart, chart_area);

    let mut legend = vec![
        Span::styled(" █", Style::default().fg(Color::Green)),
        Span::raw(" Ready  "),
        Span::styled("█", Style::default().fg(Color::Rgb(255, 165, 0))),
        Span::raw(" Urgent  "),
        Span::styled("█", Style::default().fg(Color::Yellow)),
        Span::raw(" Reserved  "),
        Span::styled("█", Style::default().fg(Color::Blue)),
        Span::raw(" Delayed  "),
        Span::styled("█", Style::default().fg(Color::Red)),
        Span::raw(" Buried"),
    ];

    // Scroll position, when the list does not all fit.
    if tubes.len() > chart_height {
        legend.push(Span::styled(
            format!(
                "  [{}-{} of {}]",
                offset + 1,
                (offset + chart_height).min(tubes.len()),
                tubes.len()
            ),
            Style::default().fg(Color::DarkGray),
        ));
    }

    frame.render_widget(Paragraph::new(vec![Line::from(legend)]), legend_area);
}

/// Width of the selection cursor column.
const CURSOR_WIDTH: u16 = 1;

/// `PAUSED 12m` for a tube whose pause has not yet elapsed.
fn pause_badge(tube: &tuber_lib::model::TubeStats) -> Option<Span<'static>> {
    if !tube.is_paused() {
        return None;
    }
    Some(Span::styled(
        format!(" PAUSED {}", format_secs(tube.pause_time_left)),
        Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
    ))
}

fn render_bottom_panel(frame: &mut Frame, app: &App, area: Rect, timing_rows: usize) {
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

    let total_buried: u64 = snap.tubes.iter().map(|t| t.current_jobs_buried).sum();
    let paused_count = snap.tubes.iter().filter(|t| t.is_paused()).count();

    // Sorted to match the chart, so a tube sits at a comparable position in
    // both panels, and trimmed to the row budget the layout allowed us.
    let all_timing = app.timing_tubes();
    let truncated = all_timing.len() > timing_rows;
    // Give up a row to the "+N more" marker when we cannot show everything.
    let shown = if truncated {
        timing_rows.saturating_sub(1)
    } else {
        timing_rows
    };
    let timing_tubes = &all_timing[..shown.min(all_timing.len())];
    let max_name_len = timing_tubes
        .iter()
        .map(|t| t.name.len())
        .max()
        .unwrap_or(0)
        .min(20);

    let buried_style = if total_buried > 0 {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Green)
    };

    let mut line2_spans = vec![
        Span::styled(" Timeouts: ", Style::default().fg(Color::DarkGray)),
        Span::raw(format!("{:.1}/s", timeouts_s)),
        Span::styled("  Buried: ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{total_buried} total"), buried_style),
    ];

    if paused_count > 0 {
        line2_spans.push(Span::styled("  Paused: ", Style::default().fg(Color::DarkGray)));
        line2_spans.push(Span::styled(
            format!("{paused_count} tube{}", if paused_count == 1 { "" } else { "s" }),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    }

    let mut text = vec![line1, Line::from(line2_spans)];

    // The layout dropped the timing table entirely to keep the chart usable.
    // Say so, rather than letting it look as though no tube has timing data.
    if timing_rows == 0 && !all_timing.is_empty() {
        text.push(Line::from(Span::styled(
            format!(
                " {} tube{} with recent activity have timing data \u{2014} resize taller to show",
                all_timing.len(),
                if all_timing.len() == 1 { "" } else { "s" }
            ),
            Style::default().fg(Color::DarkGray),
        )));
    }

    if !timing_tubes.is_empty() {
        let thresh_ms = (snap.server.processing_time_fast_threshold * 1000.0) as u64;
        let header = Line::from(vec![
            Span::styled(
                format!(" {:>width$} ", "ewma", width = max_name_len),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("{:>6} | {:>6}", format!("<{}ms", thresh_ms), format!(">{}ms", thresh_ms)),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                format!(" | {:>6} | {:>6} | {:>6}", "p50", "p95", "p99"),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(format!(" | {:>6}", "tiq"), Style::default().fg(Color::DarkGray)),
        ]);
        text.push(header);

        const DASH: &str = "     \u{2014}"; // right-aligned em-dash, 6 display columns
        for tube in timing_tubes {
            let selected = app.selected.as_deref() == Some(tube.name.as_str());
            let name_style = if selected {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let mut spans = vec![
                Span::styled(
                    format!(" {:>width$} ", truncate_name(&tube.name, max_name_len), width = max_name_len),
                    name_style,
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
                    " | {} | {} | {}",
                    format_duration(tube.processing_time_p50),
                    format_duration(tube.processing_time_p95),
                    format_duration(tube.processing_time_p99),
                )));
            }

            // Time in queue
            if tube.queue_time_ewma > 0.0 {
                spans.push(Span::raw(format!(" | {}", format_duration(tube.queue_time_ewma))));
            }

            text.push(Line::from(spans));
        }

        if truncated {
            let more = all_timing.len() - timing_tubes.len();
            text.push(Line::from(Span::styled(
                format!(" +{more} more (resize to see)"),
                Style::default().fg(Color::DarkGray),
            )));
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

fn queue_change_indicator(prev_total: u64, curr_total: u64) -> (&'static str, Color) {
    let diff = curr_total as i64 - prev_total as i64;
    let abs_diff = diff.unsigned_abs();
    let base = prev_total.max(curr_total).max(1) as f64;
    let change_pct = (diff as f64 / base * 100.0).abs();

    let is_slow = change_pct <= 1.0 && abs_diff <= 10;
    let is_fast = (change_pct > 10.0 || abs_diff > 100) && base > 20.0;

    match (diff.signum(), is_fast, is_slow) {
        (0, _, _)      => ("—", Color::Blue),
        (1, true, _)   => ("⇈", Color::Red),
        (1, _, true)   => ("↑", Color::Blue),
        (1, _, _)      => ("↑", Color::Yellow),
        (_, true, _)   => ("⇊", Color::Green),
        (_, _, true)   => ("↓", Color::Blue),
        _              => ("↓", Color::Green),
    }
}

fn format_duration(seconds: f64) -> String {
    let raw = if seconds < 1.0 {
        format!("{:.0}ms", seconds * 1000.0)
    } else if seconds < 600.0 {
        format!("{:.1}s", seconds)
    } else if seconds < 3600.0 {
        format!("{:.1}m", seconds / 60.0)
    } else {
        format!("{:.1}h", seconds / 3600.0)
    };
    format!("{:>6}", raw)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows the chart is left with, given the split.
    fn chart_rows(avail: usize, bottom: usize) -> usize {
        avail.saturating_sub(bottom).saturating_sub(CHART_OVERHEAD)
    }

    #[test]
    fn tall_terminal_fits_everything() {
        // 50-row terminal, 5-row top bar: 19 tubes, 6 with timing data.
        let (bottom, rows) = split_bottom(45, 19, 6);
        assert_eq!(rows, 6, "all timing tubes shown");
        assert!(chart_rows(45, bottom) >= 19, "all tubes shown without scrolling");
    }

    #[test]
    fn short_terminal_keeps_the_chart_usable() {
        // 80x24 with a 5-row top bar. This is the case the old layout broke:
        // the timing table took 19 rows and left the chart at its floor.
        let (bottom, rows) = split_bottom(19, 19, 6);
        assert!(
            chart_rows(19, bottom) >= 10,
            "chart got {} rows, expected it to keep the majority",
            chart_rows(19, bottom)
        );
        assert_eq!(rows, 0, "a 1-2 row timing table is not worth the space");
    }

    #[test]
    fn medium_terminal_splits_and_truncates() {
        let (bottom, rows) = split_bottom(29, 19, 6);
        assert!(rows > 0 && rows < 6, "partial timing table, got {rows}");
        assert!(chart_rows(29, bottom) >= 14);
    }

    #[test]
    fn chart_always_keeps_its_floor() {
        for avail in 0..40 {
            let (bottom, rows) = split_bottom(avail, 30, 30);
            assert!(bottom <= avail, "bottom {bottom} exceeds avail {avail}");
            if avail >= CHART_MIN_ROWS + CHART_OVERHEAD + STATS_FIXED {
                assert!(
                    chart_rows(avail, bottom) >= CHART_MIN_ROWS,
                    "avail={avail} left the chart {} rows",
                    chart_rows(avail, bottom)
                );
            }
            // The panel must always have room for the rows it claims.
            assert!(bottom >= STATS_FIXED + usize::from(rows > 0) + rows || rows == 0);
        }
    }

    #[test]
    fn no_timing_tubes_means_no_rows() {
        let (bottom, rows) = split_bottom(45, 8, 0);
        assert_eq!(rows, 0);
        assert_eq!(bottom, STATS_FIXED);
    }

    #[test]
    fn few_tubes_do_not_stretch_the_chart() {
        // 2 tubes should not reserve chart space it cannot use.
        let (_, rows) = split_bottom(45, 2, 6);
        assert_eq!(rows, 6);
    }
}
