use crate::app::{
    format_secs, kick_plan, kick_target, parse_count, parse_duration, App, Mode, PromptKind,
};
use tuber_lib::model::TubeStats;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

/// Chart rows consumed by its own chrome: bottom border + column header line.
const CHART_OVERHEAD: usize = 2;
/// Tube rows the chart keeps even when the timing table wants more space.
const CHART_MIN_ROWS: usize = 4;
/// Stats panel rows that are always present: top border, the two summary
/// lines, and two spare — the spares carry the optional "+N more" / "resize
/// taller" note and the blank tail the original layout allowed for.
const STATS_FIXED: usize = 5;
/// Most of the space below the top bar the chart may claim before the timing
/// table gets a look in.
const CHART_MAX_SHARE: (usize, usize) = (2, 3);
/// Width of the selection cursor column.
const CURSOR_WIDTH: u16 = 1;
/// Space between the blocks of a chart row.
const GAP: usize = 2;
/// Width of one job-state count column. `format_number` never needs more than
/// six columns, and the widest state header ("buried") is six as well.
const COUNT_WIDTH: usize = 6;
/// Width of the total column, and of the trend chevron that qualifies it —
/// reserved even on the first poll, when there is no previous total to compare
/// against, so the columns after it do not shift once one arrives.
const TOTAL_WIDTH: usize = 7;
const TREND_WIDTH: usize = 2;
/// Width of the trailing `(  2.4s)` timing suffix.
const EWMA_WIDTH: usize = 9;
/// Bounds on the size bar. The cap is deliberately well short of the terminal
/// width: the counts carry the precision, so the bar only has to say roughly how
/// big and roughly what mix, and twenty cells over six decades of jobs says that
/// clearly. Letting it stretch would spread a row back across the screen, which
/// is the sprawl this layout exists to undo. Below the floor it cannot show a
/// proportion at all, and the width goes to the counts instead.
const BAR_MAX: usize = 20;
const BAR_MIN: usize = 6;
/// Shortest the name column is allowed to get before the row simply overflows.
/// Below this a truncated name identifies nothing, so there is no point trading
/// further width for it.
const NAME_MIN: usize = 6;

/// The job states a tube reports, in bar order: header label and colour. The
/// colour is used for the header, the count and the bar fill, which is what
/// lets the header line double as the legend.
const STATES: [(&str, Color); 5] = [
    ("ready", Color::Green),
    ("urgnt", Color::Rgb(255, 165, 0)),
    ("resvd", Color::Yellow),
    ("delay", Color::Blue),
    ("buried", Color::Red),
];

/// A tube's counts, indexed to match [`STATES`].
///
/// Ready is net of urgent: `current-jobs-urgent` is a subset of
/// `current-jobs-ready`, so the two columns would otherwise double-count it and
/// stop summing to the total.
fn state_counts(tube: &TubeStats) -> [u64; 5] {
    [
        tube.current_jobs_ready.saturating_sub(tube.current_jobs_urgent),
        tube.current_jobs_urgent,
        tube.current_jobs_reserved,
        tube.current_jobs_delayed,
        tube.current_jobs_buried,
    ]
}

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
    // per tube. It takes at most its share, then the timing table gets the rest.
    let (share_num, share_den) = CHART_MAX_SHARE;
    let chart_cap = (avail * share_num / share_den).max(chart_floor);
    let chart_want = (visible_tubes + CHART_OVERHEAD).clamp(chart_floor, chart_cap);

    // The chart never gives up its floor, so this is a hard ceiling.
    let ceiling = avail.saturating_sub(chart_floor);
    if ceiling < STATS_FIXED {
        return (ceiling, 0);
    }

    // Budget the header row up front, so `rows` is decided once rather than
    // computed against one limit and then re-clamped against another.
    let budget = avail
        .saturating_sub(chart_want)
        .min(ceiling)
        .saturating_sub(STATS_FIXED + 1);
    let mut rows = timing_tubes.min(budget);
    // One or two rows plus a "+N more" marker tells you nothing; spend the
    // space on the chart instead.
    if rows < 3 && rows < timing_tubes {
        rows = 0;
    }
    (STATS_FIXED + usize::from(rows > 0) + rows, rows)
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

    // Both lists are computed once per frame and passed down. Recomputing them
    // in each panel would re-filter and re-sort, and would make the two panels
    // agree only by coincidence rather than by construction.
    let visible = app.visible_tubes();
    let timing = app.timing_tubes();

    let (bottom_height, timing_rows) = split_bottom(
        area.height.saturating_sub(top_height) as usize,
        visible.len(),
        timing.len(),
    );

    let chunks = Layout::vertical([
        Constraint::Length(top_height),              // top bar
        Constraint::Min(CHART_MIN_ROWS as u16),      // tube chart
        Constraint::Length(bottom_height as u16),    // bottom panel
    ])
    .split(area);

    render_top_bar(frame, app, chunks[0]);
    render_tube_chart(frame, app, chunks[1], &visible);
    render_bottom_panel(frame, app, chunks[2], &timing, timing_rows);
    render_prompt_overlay(frame, app, area);
}

/// The prompts and command results share the last row of the frame, drawn over
/// whatever is beneath so opening a prompt does not reflow the layout.
fn render_prompt_overlay(frame: &mut Frame, app: &App, area: Rect) {
    let line = match &app.mode {
        Mode::Prompt { kind, tube, buf } => Some(prompt_line(app, *kind, tube, buf)),
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

/// One prompt row: a coloured badge, what is being asked, the buffer with a
/// cursor, and a hint reading back what Enter will do.
fn prompt_line(app: &App, kind: PromptKind, tube: &str, buf: &str) -> Line<'static> {
    let (badge, badge_bg, ask) = match kind {
        PromptKind::Pause => (" Pause ", Color::Yellow, "for"),
        PromptKind::Kick => (" Kick ", Color::Cyan, "up to"),
    };

    let mut spans = vec![
        Span::styled(
            badge,
            Style::default().fg(Color::Black).bg(badge_bg).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {tube} {ask}: ")),
        Span::styled(
            buf.to_string(),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
        Span::styled("\u{2588}", Style::default().fg(Color::DarkGray)),
    ];

    // The hint is the only feedback on bad input — Enter on something
    // unparseable is a deliberate no-op, so this must say why.
    let (hint, hint_style) = match kind {
        PromptKind::Pause => match parse_duration(buf) {
            Some(0) => ("  = unpause now".to_string(), Color::DarkGray),
            Some(secs) => (format!("  = {}", format_secs(secs as u64)), Color::DarkGray),
            None if buf.is_empty() => ("  seconds, or 5m / 2h".to_string(), Color::DarkGray),
            None => ("  not a duration".to_string(), Color::Red),
        },
        // Read live from the snapshot rather than captured when the prompt
        // opened: a poll can land mid-typing, and the plan should describe the
        // tube as it is now, not as it was.
        PromptKind::Kick => {
            let (available, state) = app
                .tube_by_name(tube)
                .map(|t| kick_target(t.current_jobs_buried, t.current_jobs_delayed))
                .unwrap_or((0, "buried"));
            match parse_count(buf) {
                Some(n) => (
                    format!("  = {}", kick_plan(n, available, state)),
                    Color::DarkGray,
                ),
                None if buf.is_empty() => {
                    (format!("  how many, of {available} {state}"), Color::DarkGray)
                }
                None => ("  not a job count".to_string(), Color::Red),
            }
        }
    };
    spans.push(Span::styled(hint, Style::default().fg(hint_style)));
    spans.push(Span::styled(
        "   Enter confirm  Esc cancel",
        Style::default().fg(Color::DarkGray),
    ));
    Line::from(spans)
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

/// The chart title, and with it the key hints — the widest form that fits the
/// frame. The full list no longer fits 80 columns alongside a hidden count, and
/// letting ratatui chop it mid-word loses a whole key rather than the least
/// important one, so drop hints deliberately instead: `u` and the toggle's
/// wording go first, since a paused tube already wears a badge and `e` is
/// discoverable from the "N hidden" count next to it.
fn chart_title(scope: &str, toggle: &str, width: u16) -> String {
    let arrows = "\u{2191}\u{2193}";
    let candidates = [
        format!(" Tubes ({scope}) {arrows} p:pause u:unpause K:kick s:sort {toggle} q:quit "),
        format!(" Tubes ({scope}) {arrows} p:pause K:kick s:sort e:empty q:quit "),
        format!(" Tubes ({scope}) {arrows} p K s e q "),
        format!(" Tubes ({scope}) "),
    ];
    candidates
        .iter()
        .find(|t| t.chars().count() <= width as usize)
        .unwrap_or(candidates.last().expect("candidates is non-empty"))
        .clone()
}

fn render_tube_chart(frame: &mut Frame, app: &App, area: Rect, tubes: &[&TubeStats]) {
    // Key hints live in the title, which leaves the header line free to carry
    // the column labels. Those are drawn in their state colours, so they are
    // the legend as well — the swatch legend this replaced spent a whole row on
    // 52 columns saying only what the headers now say in passing.
    //
    // Derived from the list rather than recounted, so the number can never
    // disagree with what was actually filtered out.
    let hidden = app.tube_count().saturating_sub(tubes.len());
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
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .title(chart_title(&scope, toggle, area.width));

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

    // Reserve the first line for the column headers.
    let header_height = inner.height.min(1);
    let chart_height = inner.height.saturating_sub(header_height) as usize;
    let header_area = Rect {
        height: header_height,
        ..inner
    };
    let chart_area = Rect {
        y: inner.y + header_height,
        height: chart_height as u16,
        ..inner
    };

    // Scroll the window just far enough to keep the cursor on screen. Derived
    // from the selection rather than stored, so it cannot drift out of sync
    // with a list that is refiltered and resorted on every poll.
    let selected = app.index_of_selected(tubes);
    let offset = match selected {
        Some(i) if chart_height > 0 && i >= chart_height => i + 1 - chart_height,
        _ => 0,
    };

    // Find max name length for alignment
    let max_name_len = tubes.iter().map(|t| t.name.len()).max().unwrap_or(0).min(20);

    // Reserve space for the pause badge only when something is actually paused,
    // so the common case keeps its full bar width.
    let pause_reserve = if tubes.iter().any(|t| t.is_paused()) { 12 } else { 0 };

    // A state earns a column only when some visible tube has jobs in it.
    // Decided once for the whole frame rather than per row, which is the point:
    // every row keeps the same columns, so the numbers line up and a column's
    // position identifies its state — while a state nobody is using spends no
    // width at all.
    let columns = active_columns(tubes);
    let (name_width, bar_width, show_ewma) = row_widths(
        inner.width as usize,
        max_name_len,
        column_block_width(&columns),
        pause_reserve,
    );

    // The busiest visible tube sets the bar scale, so bar lengths are
    // comparable down the column.
    let max_total = tubes.iter().map(|t| t.current_total()).max().unwrap_or(0);

    let mut lines = Vec::new();
    for (row, tube) in tubes.iter().enumerate().skip(offset).take(chart_height) {
        let is_selected = selected == Some(row);
        let total = tube.current_total();
        let counts = state_counts(tube);

        let mut spans = vec![
            Span::styled(
                if is_selected { "\u{258c}" } else { " " },
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                format!("{:>width$}", truncate_name(&tube.name, name_width), width = name_width),
                if is_selected {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ),
        ];

        // Total leads the breakdown, so the row reads whole-then-parts: "908
        // total, made of 5 ready, 10 delayed, 893 buried".
        let total_display = format!("{:>width$}", format_number(total), width = GAP + TOTAL_WIDTH);
        spans.push(if total == 0 {
            Span::styled(total_display, Style::default().fg(Color::DarkGray))
        } else {
            Span::raw(total_display)
        });

        // Growth chevron, qualifying the total it follows. queue_change_indicator
        // yields a glyph on every arm, so this needs no emptiness guard — but
        // the width is held even before there is a previous total to compare.
        match app.previous_tube_total(&tube.name) {
            Some(prev_total) => {
                let (chevron, color) = queue_change_indicator(prev_total, total);
                spans.push(Span::styled(format!(" {chevron}"), Style::default().fg(color)));
            }
            None => spans.push(Span::raw(" ".repeat(TREND_WIDTH))),
        }

        // Each count carries its state's colour, so the number is its own key.
        // A state holding nothing gets a dim placeholder rather than a zero: the
        // column has to keep its place, but a screenful of zeros reads as data.
        for (n, &state) in columns.iter().enumerate() {
            let (text, color) = match counts[state] {
                0 => ("\u{00b7}".to_string(), Color::DarkGray),
                count => (format_number(count), STATES[state].1),
            };
            spans.push(Span::styled(
                format!("{:>width$}", text, width = count_field_width(n)),
                Style::default().fg(color),
            ));
        }

        if bar_width > 0 {
            spans.push(Span::raw(" ".repeat(GAP)));
            spans.extend(size_bar_spans(&counts, total, max_total, bar_width));
        }

        // Timing suffix, for tubes actually holding work — an empty tube's row
        // ends at its bar.
        if show_ewma && total > 0 && tube.processing_time_ewma > 0.0 {
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

    let mut header = vec![
        Span::raw(" ".repeat(CURSOR_WIDTH as usize + name_width)),
        Span::styled(
            format!("{:>width$}", "total", width = GAP + TOTAL_WIDTH),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" ".repeat(TREND_WIDTH)),
    ];
    for (n, &state) in columns.iter().enumerate() {
        header.push(Span::styled(
            format!("{:>width$}", STATES[state].0, width = count_field_width(n)),
            Style::default().fg(STATES[state].1),
        ));
    }
    if bar_width > 0 {
        header.push(Span::styled(
            format!("{}{:<bar$}", " ".repeat(GAP), "size", bar = bar_width),
            Style::default().fg(Color::DarkGray),
        ));
    }
    frame.render_widget(Paragraph::new(vec![Line::from(header)]), header_area);

    // Scroll position, when the list does not all fit. Right-aligned over the
    // header rather than inline, so it cannot push the column labels out of
    // line with the rows beneath them.
    if tubes.len() > chart_height {
        let pos = Span::styled(
            format!(
                "[{}-{} of {}] ",
                offset + 1,
                (offset + chart_height).min(tubes.len()),
                tubes.len()
            ),
            Style::default().fg(Color::DarkGray),
        );
        frame.render_widget(
            Paragraph::new(vec![Line::from(pos)]).alignment(Alignment::Right),
            header_area,
        );
    }
}

/// Divide a row's width between the parts that can give way, returning
/// `(name_width, bar_width, show_ewma)`.
///
/// The total and the count columns are never traded away: they are the only
/// numbers on the row that appear nowhere else on the screen, and clipping them
/// would take `buried` — the one that matters most — off the right edge first,
/// because it sits at the end of the block. So the bar goes, then the timing
/// suffix (which the Stats panel repeats in more detail anyway), and the name
/// truncates before either of those is allowed to push the counts off.
fn row_widths(
    width: usize,
    max_name_len: usize,
    counts_width: usize,
    pause_reserve: usize,
) -> (usize, usize, bool) {
    let core = CURSOR_WIDTH as usize + GAP + TOTAL_WIDTH + TREND_WIDTH + counts_width + pause_reserve;

    // The name identifies the row, so it is served first — but it shrinks to
    // whatever is left rather than holding its full width at the counts' expense.
    let name_width = width
        .saturating_sub(core)
        .clamp(NAME_MIN.min(max_name_len), max_name_len);

    // The bar claims only its floor at first, so that on a terminal with room
    // for one or the other the timing suffix is not starved by a bar that had
    // already grown to its cap. Once both are placed, the bar absorbs the slack.
    let mut spare = width.saturating_sub(core + name_width);
    let mut bar_width = if spare >= GAP + BAR_MIN { BAR_MIN } else { 0 };
    if bar_width > 0 {
        spare -= GAP + bar_width;
    }
    let show_ewma = spare >= EWMA_WIDTH;
    if show_ewma {
        spare -= EWMA_WIDTH;
    }
    if bar_width > 0 {
        bar_width = (bar_width + spare).min(BAR_MAX);
    }
    (name_width, bar_width, show_ewma)
}

/// Indices into [`STATES`] that any of `tubes` has jobs in.
fn active_columns(tubes: &[&TubeStats]) -> Vec<usize> {
    (0..STATES.len())
        .filter(|&i| tubes.iter().any(|t| state_counts(t)[i] > 0))
        .collect()
}

/// Width of the `n`th count field, gap included. The first field carries the
/// full gap that separates it from the trend chevron; the rest sit one column
/// apart. Header and row use this same function, which is what keeps them
/// aligned.
fn count_field_width(n: usize) -> usize {
    COUNT_WIDTH + if n == 0 { GAP } else { 1 }
}

/// Total width of the count block, gaps included.
fn column_block_width(columns: &[usize]) -> usize {
    (0..columns.len()).map(count_field_width).sum()
}

/// How much of a `width`-cell bar a tube fills, and how those cells divide
/// between its states.
///
/// Length is the tube's total on a log scale against the busiest visible tube,
/// so lengths are comparable down the column. The fill is split *linearly*,
/// because within one row the question is what share each state holds, and a log
/// split would answer a different one.
///
/// The bar this replaced filled the full width on every row and log-scaled the
/// segments within it, which made a tube holding one job look as busy as one
/// holding a million, and a handful of buried jobs look like a third of the
/// queue.
fn bar_cells(counts: &[u64; 5], total: u64, max_total: u64, width: usize) -> ([usize; 5], usize) {
    let mut cells = [0usize; 5];
    if total == 0 || max_total == 0 || width == 0 {
        return (cells, 0);
    }

    // A tube with jobs always gets at least one cell — rounding it away would
    // render it identical to an empty tube.
    let scale = (total as f64 + 1.0).log10() / (max_total as f64 + 1.0).log10();
    let filled = ((scale * width as f64).round() as usize).clamp(1, width);

    // Largest remainder, so the cells add up to `filled` exactly and no state is
    // rounded up to a cell it has not earned. Deliberately no floor of one cell
    // per state: that would put back the distortion this bar exists to remove,
    // and the count columns already name every state that has jobs in it.
    let mut used = 0usize;
    let mut remainders: Vec<(f64, usize)> = Vec::new();
    for (i, &count) in counts.iter().enumerate() {
        if count == 0 {
            continue;
        }
        let exact = count as f64 / total as f64 * filled as f64;
        cells[i] = exact.floor() as usize;
        used += cells[i];
        remainders.push((exact.fract(), i));
    }
    remainders.sort_by(|a, b| b.0.total_cmp(&a.0));
    for &(_, i) in remainders.iter().take(filled - used) {
        cells[i] += 1;
    }

    (cells, filled)
}

/// The size bar: coloured fill for the jobs a tube holds, dim track for the rest
/// of the scale.
fn size_bar_spans(
    counts: &[u64; 5],
    total: u64,
    max_total: u64,
    width: usize,
) -> Vec<Span<'static>> {
    let (cells, filled) = bar_cells(counts, total, max_total, width);
    let mut spans: Vec<Span<'static>> = cells
        .iter()
        .enumerate()
        .filter(|(_, &n)| n > 0)
        .map(|(i, &n)| {
            Span::styled("\u{2588}".repeat(n), Style::default().fg(STATES[i].1))
        })
        .collect();
    if filled < width {
        spans.push(Span::styled(
            "\u{2591}".repeat(width - filled),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans
}

/// `PAUSED 12m` for a tube whose pause has not yet elapsed.
fn pause_badge(tube: &TubeStats) -> Option<Span<'static>> {
    if !tube.is_paused() {
        return None;
    }
    Some(Span::styled(
        format!(" PAUSED {}", format_secs(tube.pause_time_left)),
        Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
    ))
}

fn render_bottom_panel(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    all_timing: &[&TubeStats],
    timing_rows: usize,
) {
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

    let (total_buried, paused_count) = snap.tubes.iter().fold((0u64, 0usize), |(b, p), t| {
        (b + t.current_jobs_buried, p + usize::from(t.is_paused()))
    });

    // Trimmed to the row budget the layout allowed us, giving up a row to the
    // "+N more" marker when we cannot show everything.
    let truncated = all_timing.len() > timing_rows;
    let shown = if truncated {
        timing_rows.saturating_sub(1)
    } else {
        all_timing.len()
    };
    let timing_tubes = &all_timing[..shown];
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
            format!("{paused_count} tube{}", plural(paused_count)),
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
                plural(all_timing.len())
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

/// Clip a name to `max_len` display columns.
///
/// Truncates on a character boundary, not a byte one. beanstalkd restricts tube
/// names to ASCII, so this has never mattered — but the name column now shrinks
/// to fit a narrow terminal rather than always taking its full width, so this
/// truncates far more often than it used to, and a byte slice through a
/// multibyte name from a laxer server would panic mid-frame.
fn truncate_name(name: &str, max_len: usize) -> &str {
    match name.char_indices().nth(max_len) {
        Some((byte, _)) => &name[..byte],
        None => name,
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

/// Never wider than six columns, which is what the count columns are sized to.
///
/// Each threshold sits just under its power of ten rather than on it, because
/// the tier below rounds to one decimal: on a boundary of 1_000_000 the value
/// 999_999 stays in the K tier and formats as "1000.0K", seven columns wide,
/// which knocks every column after it out of line.
fn format_number(n: u64) -> String {
    let (divisor, unit) = match n {
        // Past a thousand trillion jobs the exact figure is noise, and the field
        // it would need is not worth widening every column for.
        _ if n >= 999_950_000_000_000 => return ">999T".to_string(),
        _ if n >= 999_950_000_000 => (1e12, "T"),
        _ if n >= 999_950_000 => (1e9, "G"),
        _ if n >= 999_950 => (1e6, "M"),
        _ if n >= 1_000 => (1e3, "K"),
        _ => return n.to_string(),
    };
    format!("{:.1}{unit}", n as f64 / divisor)
}

/// Pluralising suffix for a count.
fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::Instant;
    use tuber_lib::model::{ServerStats, Snapshot};

    /// Rows the chart is left with, given the split.
    fn chart_rows(avail: usize, bottom: usize) -> usize {
        avail.saturating_sub(bottom).saturating_sub(CHART_OVERHEAD)
    }

    #[test]
    fn the_title_never_overflows_the_frame() {
        // The 80-column case is the one that matters: it is where the hints are
        // needed most and where the full list no longer fits.
        for width in 1..=120u16 {
            for scope in ["name", "size \u{00b7} 11 hidden", "size \u{00b7} 1234 hidden"] {
                let title = chart_title(scope, "e:show empty", width);
                assert!(
                    title.chars().count() <= width as usize || width < 30,
                    "width {width} scope {scope:?} -> {title:?}"
                );
            }
        }
    }

    #[test]
    fn eighty_columns_still_offers_the_kick_hint() {
        let title = chart_title("name \u{00b7} 11 hidden", "e:show empty", 80);
        assert!(title.contains("K:kick"), "{title:?}");
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
    fn a_claimed_row_always_renders_a_tube() {
        // render_bottom_panel gives a row up to the "+N more" marker when it
        // cannot show everything, so returning rows=1 with more to show would
        // reserve panel height and then render no timing rows at all — and no
        // note either, since that is gated on rows == 0.
        for avail in 0..200 {
            for visible in 0..60 {
                for timing in 0..60 {
                    let (height, rows) = split_bottom(avail, visible, timing);
                    if rows == 0 {
                        continue;
                    }
                    let truncated = timing > rows;
                    let shown = if truncated { rows - 1 } else { timing };
                    assert!(
                        shown > 0,
                        "avail={avail} visible={visible} timing={timing} -> rows={rows} \
                         reserves height {height} but renders no timing rows"
                    );
                    assert!(
                        height >= STATS_FIXED + 1 + rows,
                        "avail={avail} timing={timing} -> height {height} too small for {rows} rows"
                    );
                }
            }
        }
    }

    /// The tubes from a real server, as they looked when this layout was
    /// designed — a wide spread of totals, two states used by only one tube
    /// each, and a tube whose queue is almost entirely buried.
    fn sample_tubes() -> Vec<TubeStats> {
        [
            ("book", 10_000, 0, 0, 0, 145),
            ("contributor", 1_700, 0, 0, 3, 511),
            ("covers", 1_900_000, 135, 0, 0, 184),
            ("covers-download", 191_300, 207, 0, 14, 463),
            ("maintenance", 0, 0, 1, 0, 0),
            ("series", 4, 0, 0, 1, 0),
            ("work", 5, 0, 0, 10, 893),
        ]
        .into_iter()
        .map(|(name, ready, urgent, reserved, delayed, buried)| {
            TubeStats::from_yaml(&format!(
                "name: {name}\n\
                 current-jobs-ready: {}\n\
                 current-jobs-urgent: {urgent}\n\
                 current-jobs-reserved: {reserved}\n\
                 current-jobs-delayed: {delayed}\n\
                 current-jobs-buried: {buried}\n\
                 processing-time-ewma: 2.4\n",
                ready + urgent,
            ))
        })
        .collect()
    }

    fn sample_app(tubes: Vec<TubeStats>) -> App {
        let mut app = App::new();
        app.update(Snapshot {
            server: ServerStats::from_yaml("version: tuber 0.13.0\n"),
            tubes,
            fetched_at: Instant::now(),
        });
        app
    }

    /// Index of the column header within a rendered chart: line 0 carries the
    /// block title, which ratatui draws on the top row.
    const HEADER_LINE: usize = 1;

    /// Render the chart at `width` and give back its lines as plain text.
    fn draw_chart(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_tube_chart(frame, app, area, &app.visible_tubes());
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// The whole point of the layout: every row's numbers sit in the same
    /// columns, so a count can be read down the screen as well as across.
    #[test]
    fn counts_line_up_down_the_column() {
        let app = sample_app(sample_tubes());
        let lines = draw_chart(&app, 130, 10);
        for line in &lines {
            println!("{line}");
        }

        // Line 0 is the block title; the column header is the first inner row.
        // Every state is in play here, reserved included — maintenance has one.
        let header = &lines[HEADER_LINE];
        for state in ["total", "ready", "urgnt", "resvd", "delay", "buried", "size"] {
            assert!(header.contains(state), "header {header:?} lacks {state}");
        }

        // Every state's column ends where its header does, on every row. Indexed
        // by character rather than byte: the cursor glyph and the `·`
        // placeholder are multibyte, so byte offsets would not be columns.
        let rows: Vec<Vec<char>> = lines[HEADER_LINE + 1..HEADER_LINE + 8]
            .iter()
            .map(|l| l.chars().collect())
            .collect();
        for state in ["ready", "urgnt", "resvd", "delay", "buried"] {
            // The header is all ASCII, so a byte offset into it is a column.
            let end = header.find(state).unwrap() + state.len();
            for row in &rows {
                let cell: String = row[end - COUNT_WIDTH..end].iter().collect();
                assert!(
                    !cell.trim().is_empty(),
                    "{state} column is blank at {end} on {:?} — a placeholder was expected",
                    row.iter().collect::<String>()
                );
                // Right-aligned means the value ends exactly at the header's
                // last character, and does not run past it.
                assert_ne!(
                    row[end - 1],
                    ' ',
                    "{state} column holds {cell:?}, not flush with the header"
                );
                assert_eq!(
                    row[end], ' ',
                    "{state} column {cell:?} spills past its width on {:?}",
                    row.iter().collect::<String>()
                );
            }
        }
    }

    #[test]
    fn a_name_truncates_on_a_character_boundary() {
        assert_eq!(truncate_name("covers-download", 6), "covers");
        assert_eq!(truncate_name("work", 6), "work");
        assert_eq!(truncate_name("work", 4), "work");
        // Would panic on a byte slice.
        assert_eq!(truncate_name("kaffe\u{301}-jobs", 6), "kaffe\u{301}");
        assert_eq!(truncate_name("\u{4f5c}\u{696d}\u{5217}", 2), "\u{4f5c}\u{696d}");
    }

    /// A state no visible tube is using spends no width.
    #[test]
    fn an_unused_state_gets_no_column() {
        let quiet: Vec<TubeStats> = sample_tubes()
            .into_iter()
            .filter(|t| t.name == "book" || t.name == "series")
            .collect();
        let app = sample_app(quiet);
        let lines = draw_chart(&app, 130, 5);
        let header = &lines[HEADER_LINE];
        assert!(header.contains("ready") && header.contains("delay") && header.contains("buried"));
        assert!(!header.contains("urgnt"), "no tube has urgent jobs: {header:?}");
        assert!(!header.contains("resvd"), "no tube has reserved jobs: {header:?}");
    }

    /// On a narrow terminal the bar is what gives way — the counts are the part
    /// that cannot be inferred from anything else on the row.
    #[test]
    fn a_narrow_terminal_drops_the_bar_and_keeps_the_counts() {
        let app = sample_app(sample_tubes());

        // Nothing ever overflows, at any width — the row is built from fixed
        // fields plus whatever the bar can have, so this is the check that the
        // arithmetic adds up.
        for width in 20..=140u16 {
            for line in draw_chart(&app, width, 10) {
                assert!(
                    line.chars().count() <= width as usize,
                    "width {width} overflowed: {line:?}"
                );
            }
        }

        // With all five states in play the counts alone need 72 columns, so an
        // 80-column terminal keeps a six-cell bar and anything narrower spends
        // the width on the numbers instead.
        for width in [60u16, 70] {
            let header = &draw_chart(&app, width, 10)[HEADER_LINE];
            assert!(!header.contains("size"), "width {width} kept the bar: {header:?}");
            assert!(header.contains("total"), "width {width} lost the total");
            assert!(header.contains("buried"), "width {width} lost the counts");
        }
        for width in [80u16, 130] {
            let header = &draw_chart(&app, width, 10)[HEADER_LINE];
            assert!(header.contains("size"), "width {width} dropped the bar: {header:?}");
        }
    }

    /// Header and rows are laid out by the same width functions, so a drift
    /// between them would be a drift in these.
    #[test]
    fn count_block_width_matches_its_fields() {
        for len in 0..=STATES.len() {
            let columns: Vec<usize> = (0..len).collect();
            let summed: usize = (0..len).map(count_field_width).sum();
            assert_eq!(column_block_width(&columns), summed, "len={len}");
        }
        assert_eq!(count_field_width(0), COUNT_WIDTH + GAP, "first field owns the gap");
        assert_eq!(count_field_width(1), COUNT_WIDTH + 1);
    }

    #[test]
    fn bar_cells_always_add_up_to_the_fill() {
        // The fill is drawn as one span per state plus a track span sized
        // `width - filled`, so cells that do not sum to `filled` would render a
        // bar of the wrong length and misalign everything after it.
        for width in 1..=BAR_MAX {
            for &total in &[1u64, 5, 908, 10_145, 192_000, 1_900_000] {
                for &max_total in &[908u64, 10_145, 1_900_000] {
                    let counts = [total / 2, total / 8, total / 8, total / 8, total / 8];
                    let sum: u64 = counts.iter().sum();
                    if sum == 0 {
                        continue;
                    }
                    let (cells, filled) = bar_cells(&counts, sum, max_total, width);
                    assert_eq!(
                        cells.iter().sum::<usize>(),
                        filled,
                        "width={width} total={sum} max={max_total} cells={cells:?}"
                    );
                    assert!(filled <= width, "filled {filled} exceeds width {width}");
                }
            }
        }
    }

    #[test]
    fn bar_length_tracks_total() {
        let max = 1_900_000;
        let len = |total: u64| {
            let counts = [total, 0, 0, 0, 0];
            bar_cells(&counts, total, max, 12).1
        };
        // The busiest tube fills the bar; a quiet one gets a stub; nothing at
        // all gets nothing. This is the whole point of the rewrite — the bar it
        // replaced was full width on every row.
        assert_eq!(len(max), 12);
        assert_eq!(len(0), 0);
        assert!(len(1) >= 1, "one job must be visible, not rounded away");

        // Monotonic, so a longer bar always means a busier tube.
        let mut prev = 0;
        for total in [0u64, 1, 5, 100, 908, 10_145, 192_000, 1_000_000, max] {
            let n = len(total);
            assert!(n >= prev, "total={total} drew {n} after {prev}");
            prev = n;
        }

        // Strictly longer once the totals are a decade or more apart. Closer
        // than that they can share a cell — twelve cells over six decades is
        // half a decade each — which is why the count column carries the number.
        for (small, big) in [(1, 908), (908, 192_000), (192_000, max)] {
            assert!(
                len(small) < len(big),
                "{small} should draw shorter than {big}"
            );
        }
    }

    #[test]
    fn fill_does_not_promote_a_negligible_state() {
        // book from the screenshot: 10.0K ready, 145 buried. Buried is 1.4% of
        // the tube, so it must not claim a cell of a 12-wide bar — the red count
        // column is what makes it visible. Rounding it up to one cell would say
        // 8%, which is the distortion the old bar had.
        let counts = [10_000, 0, 0, 0, 145];
        let (cells, filled) = bar_cells(&counts, 10_145, 10_145, 12);
        assert_eq!(filled, 12);
        assert_eq!(cells[4], 0, "1.4% must not round up to a cell");
        assert_eq!(cells[0], 12);

        // A state with a real share does get cells.
        let work = [5, 0, 0, 10, 893];
        let (cells, _) = bar_cells(&work, 908, 1_900_000, 12);
        assert!(cells[4] > 0, "98% buried must dominate the fill: {cells:?}");
    }

    #[test]
    fn format_number_fits_a_count_column() {
        for n in [0u64, 1, 999, 1_000, 999_999, 1_000_000, 999_999_999, u64::MAX] {
            assert!(
                format_number(n).len() <= COUNT_WIDTH,
                "{n} formats to {:?}, wider than a column",
                format_number(n)
            );
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
