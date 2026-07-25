use crate::app::{
    format_secs, kick_plan, kick_target, parse_count, parse_duration, plural, App, Mode,
    PromptKind, Selection,
};
use tuber_lib::model::{ServerStats, TubeStats};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

/// Rows the server totals footer occupies: the rule, and the totals row itself.
const FOOTER_ROWS: usize = 2;
/// Tube rows the chart keeps before it will spend any on the footer. Reserving
/// the footer unconditionally left a short frame showing totals and no tubes.
const ROWS_BEFORE_FOOTER: usize = 2;
/// Chart rows consumed by its own chrome: bottom border, column header line,
/// and the footer.
const CHART_OVERHEAD: usize = 2 + FOOTER_ROWS;
/// Tube rows the chart keeps even when the timing table wants more space.
const CHART_MIN_ROWS: usize = 4;
/// Stats panel rows that are always present: top border and the two
/// server-wide summary lines. Everything below them is the selected tube's
/// detail, which is budgeted separately.
const STATS_FIXED: usize = 3;
/// Width of the dim label starting each detail line.
const DETAIL_LABEL: usize = 9;
/// What separates the top bar's fields. Wider than the detail pane's middot
/// because the bar's fields are whole clauses rather than value-and-unit pairs.
const BAR_SEP: &str = " | ";
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
/// Width of the trailing ewma column: a separating space and `format_duration`'s
/// six. It used to be nine, holding `(  2.4s)` — the brackets were standing in
/// for the column label it did not have.
const EWMA_WIDTH: usize = 7;
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
/// Longest the name column may grow to. Past this a name is buying width from
/// the columns that carry the numbers, which is the wrong trade.
const NAME_MAX: usize = 20;
/// Width the pause badge needs — ` PAUSED 12m` and a column's slack — held back
/// from the bar only while some visible tube is actually paused.
const PAUSE_RESERVE: usize = 12;

/// The job states a tube reports, in bar order: header label, the same state
/// spelled out, and colour. The colour is used for the header, the count and the
/// bar fill, which is what lets the header line double as the legend.
///
/// The long name is here rather than in a parallel array because the two are
/// only ever indexed together: the chart's headers are abbreviated to fit a
/// six-column field, and the detail pane's running text is not.
const STATES: [(&str, &str, Color); 5] = [
    ("ready", "ready", Color::Green),
    ("urgnt", "urgent", Color::Rgb(255, 165, 0)),
    ("resvd", "reserved", Color::Yellow),
    ("delay", "delayed", Color::Blue),
    ("buried", "buried", Color::Red),
];

/// Counts indexed to match [`STATES`].
///
/// Ready is net of urgent: `current-jobs-urgent` is a subset of
/// `current-jobs-ready`, so the two columns would otherwise double-count it and
/// stop summing to the total. Written once and fed by both callers below, so a
/// tube's columns and the server's cannot come to disagree about that.
fn state_counts(ready: u64, urgent: u64, reserved: u64, delayed: u64, buried: u64) -> [u64; 5] {
    [
        ready.saturating_sub(urgent),
        urgent,
        reserved,
        delayed,
        buried,
    ]
}

/// A tube's counts.
fn tube_counts(tube: &TubeStats) -> [u64; 5] {
    state_counts(
        tube.current_jobs_ready,
        tube.current_jobs_urgent,
        tube.current_jobs_reserved,
        tube.current_jobs_delayed,
        tube.current_jobs_buried,
    )
}

/// The server's own totals.
///
/// Deliberately the server's figures rather than the sum of the rows on screen:
/// the chart hides empty tubes and scrolls, so a column sum would change as you
/// navigate, which is useless for the number you are watching. `current-jobs-urgent`
/// is on the wire in the global stats — it was simply never parsed.
fn server_counts(server: &ServerStats) -> [u64; 5] {
    state_counts(
        server.current_jobs_ready,
        server.current_jobs_urgent,
        server.current_jobs_reserved,
        server.current_jobs_delayed,
        server.current_jobs_buried,
    )
}

/// How many rows the totals footer gets in a chart body of `body` rows.
///
/// The tubes are the point of the chart, so the footer only claims its rows
/// while the list keeps a couple of its own. `split_bottom` budgets against
/// [`CHART_OVERHEAD`] and the renderer asks this, so the two agree about the
/// chrome by construction rather than by both counting to four.
fn footer_rows(body: usize) -> usize {
    match body >= FOOTER_ROWS + ROWS_BEFORE_FOOTER {
        true => FOOTER_ROWS,
        false => 0,
    }
}

/// Split the space below the top bar between the tube chart and the Stats
/// panel. The chart is the primary widget and is served first; the panel takes
/// what is left over. Returns `(bottom_height, detail_rows)`.
///
/// This used to be the other way round — the panel asked for one row per tube
/// as a fixed `Length`, so on a short terminal it consumed the screen and
/// squeezed the chart to its minimum.
///
/// `detail_lines` is how many lines the selected tube's detail wants. Unlike the
/// timing table this replaced, a partial pane is still worth showing: the lines
/// are ordered so that the ones that survive a squeeze are the ones you would
/// have read first.
fn split_bottom(avail: usize, visible_tubes: usize, detail_lines: usize) -> (usize, usize) {
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

    // Budget the tube's heading row up front, so `rows` is decided once rather
    // than computed against one limit and then re-clamped against another.
    let budget = avail
        .saturating_sub(chart_want)
        .min(ceiling)
        .saturating_sub(STATS_FIXED + 1);
    let rows = detail_lines.min(budget);
    (STATS_FIXED + usize::from(rows > 0) + rows, rows)
}

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Built before the layout so the bar's height follows its content: a server
    // reporting no storage limits at all has nothing for the second line, and a
    // row of blanks is worse than no row.
    let top_lines = top_bar_lines(app);
    let top_height = (top_lines.len() + 2) as u16;

    // Computed once per frame and passed down. Recomputing it in each panel
    // would re-filter and re-sort, and would make the chart and the detail pane
    // agree on what is selected only by coincidence rather than by construction.
    let visible = app.visible_tubes();

    // Derived once from the two snapshots and passed to both consumers, which
    // cannot then report different figures for the same interval.
    let rates = app.server_rates();

    // The pane's content decides its height, so it is built before the layout
    // rather than after — the lines are the measurement. Which set of lines
    // depends only on where the cursor is; there is no mode to be in, which is
    // why this is one match on the selection rather than a probe per case.
    let detail = match &app.selected {
        Some(Selection::Server) => match app.current.as_ref() {
            Some(snap) => server_detail_lines(&snap.server, rates),
            None => Vec::new(),
        },
        Some(Selection::Tube(_)) => match app.selected_tube() {
            Some(tube) => tube_detail_lines(tube),
            None => Vec::new(),
        },
        None => Vec::new(),
    };

    let (bottom_height, detail_rows) = split_bottom(
        area.height.saturating_sub(top_height) as usize,
        visible.len(),
        detail.len(),
    );

    let chunks = Layout::vertical([
        Constraint::Length(top_height),              // top bar
        Constraint::Min(CHART_MIN_ROWS as u16),      // tube chart
        Constraint::Length(bottom_height as u16),    // bottom panel
    ])
    .split(area);

    render_top_bar(frame, app, chunks[0], top_lines);
    render_tube_chart(frame, app, chunks[1], &visible);
    render_bottom_panel(frame, app, chunks[2], rates, detail, detail_rows);
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

fn render_top_bar(frame: &mut Frame, app: &App, area: Rect, lines: Vec<Line<'static>>) {
    // The address is the client's own answer to "which server is this". The
    // server's reported hostname is on its detail row, but inside a container
    // that is the container ID, so it identifies the box rather than the server.
    let title = match app.addr.is_empty() {
        true => format!(" tuber-tui v{} ", env!("CARGO_PKG_VERSION")),
        false => format!(" tuber-tui v{} \u{2014} {} ", env!("CARGO_PKG_VERSION"), app.addr),
    };
    let block = Block::default().borders(Borders::BOTTOM).title(title);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// The top bar's content.
///
/// Deliberately short. Everything that used to be here with no other home —
/// CPU, RSS, the WAL and TOAST breakdowns, the server-wide job counts — is now
/// either the chart's footer row or the server's detail pane, so the bar keeps
/// only what you would want to be interrupted by while watching something else.
/// It was three lines, the widest of them 126 columns, clipping mid-number on a
/// 94-column terminal.
fn top_bar_lines(app: &App) -> Vec<Line<'static>> {
    if let Some(err) = &app.error {
        return vec![
            Line::from(Span::styled(
                format!(" Error: {err}"),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )),
            Line::from(" Reconnecting..."),
        ];
    }

    let Some(snap) = &app.current else {
        return vec![Line::from(" Connecting...")];
    };
    let s = &snap.server;

    let server_version = if s.version.starts_with("tuber") {
        s.version.clone()
    } else {
        format!("beanstalkd {}", s.version)
    };
    let server_label = match s.name.as_str() {
        "" => server_version,
        name => format!("{server_version} \u{2014} {name}"),
    };

    let identity = join_with(
        BAR_SEP,
        vec![
            vec![Span::raw(format!(" {server_label}"))],
            vec![Span::raw(format!("up {}", format_uptime(s.uptime)))],
            vec![Span::raw(format!(
                "conns: {} (producers:{} workers:{} waiting:{})",
                s.current_connections, s.current_producers, s.current_workers, s.current_waiting
            ))],
            match s.draining {
                true => vec![Span::styled(
                    "DRAINING",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )],
                false => Vec::new(),
            },
        ],
    );

    // Headroom: the numbers that turn into an outage if you look away. Absolute
    // figures ride along because there is room for them once the detail has gone.
    let mut headroom: Vec<Vec<Span<'static>>> = Vec::new();
    if s.max_jobs_size > 0 {
        let pct = percent(s.current_jobs_size, s.max_jobs_size);
        headroom.push(labelled(
            "Mem",
            format!(
                "{:.1}% ({} / {})",
                pct,
                format_bytes(s.current_jobs_size),
                format_bytes(s.max_jobs_size)
            ),
            usage_colour(pct),
        ));
    } else if s.current_jobs_size > 0 {
        headroom.push(labelled("Mem", format_bytes(s.current_jobs_size), Color::Reset));
    }
    if s.toast_total_bytes > 0 {
        // Live share, not usage: a low figure is fragmentation awaiting
        // compaction, so the usual thresholds would colour it backwards.
        let pct = percent(s.toast_live_bytes, s.toast_total_bytes);
        headroom.push(labelled("TOAST", format!("{pct:.0}% live"), Color::Reset));
    }
    if s.max_storage_bytes > 0 {
        let used = s.binlog_total_bytes + s.toast_total_bytes;
        let pct = percent(used, s.max_storage_bytes);
        headroom.push(labelled(
            "Disk",
            format!(
                "{:.1}% ({} / {})",
                pct,
                format_bytes(used),
                format_bytes(s.max_storage_bytes)
            ),
            usage_colour(pct),
        ));
    }

    let mut lines = vec![Line::from(identity)];
    if !headroom.is_empty() {
        let mut spans = vec![Span::raw(" ")];
        spans.extend(join_with(BAR_SEP, headroom));
        lines.push(Line::from(spans));
    }
    lines
}

/// `Label: value`, the label dimmed and the value in `colour`.
fn labelled(label: &str, value: String, colour: Color) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("{label}: "), Style::default().fg(Color::DarkGray)),
        Span::styled(value, Style::default().fg(colour)),
    ]
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
    // The shortest form is the fallback, so there is always an answer.
    let fits = candidates
        .iter()
        .position(|t| t.chars().count() <= width as usize)
        .unwrap_or(candidates.len() - 1);
    candidates.into_iter().nth(fits).expect("position is in range")
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

    // First line is the column headers; the last two are the rule and the
    // server totals footer. What is left in between scrolls.
    let header_height = inner.height.min(1);
    let body = inner.height.saturating_sub(header_height);
    let footer_height = footer_rows(body as usize) as u16;
    let chart_height = body.saturating_sub(footer_height) as usize;
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

    // Everything the layout needs to know about the list, in one pass over it.
    let metrics = ChartMetrics::measure(tubes);

    // Every row carries every state, so a column's position identifies it and
    // the numbers line up both down the screen and between polls.
    let columns = active_columns();
    // Space for the pause badge is reserved only when something is actually
    // paused, so the common case keeps its full bar width.
    let (name_width, bar_width, show_ewma) = row_widths(
        inner.width as usize,
        metrics.max_name_len,
        column_block_width(&columns),
        if metrics.any_paused { PAUSE_RESERVE } else { 0 },
    );
    let max_total = metrics.max_total;

    let mut lines = Vec::new();
    for (row, tube) in tubes.iter().enumerate().skip(offset).take(chart_height) {
        let is_selected = selected == Some(row);
        let total = tube.current_total();
        let counts = tube_counts(tube);

        // Total leads the breakdown, so the row reads whole-then-parts: "908
        // total, made of 5 ready, 10 delayed, 893 buried". The growth chevron
        // qualifies the total it follows; queue_change_indicator yields a glyph
        // on every arm, so there is one whenever a previous total exists.
        let mut spans = row_prefix(
            is_selected,
            &tube.name,
            name_width,
            if is_selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
            total,
            if total == 0 {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            },
            app.previous_tube_total(&tube.name)
                .map(|prev_total| queue_change_indicator(prev_total, total)),
        );

        // Each count carries its state's colour, so the number is its own key.
        spans.extend(count_spans(&counts, &columns, Modifier::empty()));

        if bar_width > 0 {
            spans.push(Span::raw(" ".repeat(GAP)));
            spans.extend(size_bar_spans(&counts, total, max_total, bar_width));
        }

        // The tube's overall processing ewma — the combined figure, not the
        // bimodal pair, which the detail pane splits out. Rendered even when
        // absent so the column holds its place, as the counts do.
        if show_ewma {
            let text = match total > 0 && tube.processing_time_ewma > 0.0 {
                true => format_duration(tube.processing_time_ewma),
                false => "\u{00b7}".to_string(),
            };
            spans.push(Span::styled(
                format!("{:>width$}", text, width = EWMA_WIDTH),
                Style::default().fg(Color::DarkGray),
            ));
        }

        if let Some(badge) = pause_badge(tube) {
            spans.push(badge);
        }

        lines.push(Line::from(spans));
    }

    let rendered_rows = lines.len();
    let chart = Paragraph::new(lines);
    // The totals follow the last row rather than sitting at the bottom of the
    // chart's region. The region absorbs whatever the layout has spare — it is
    // the flexible one — so pinning the footer to its foot left the totals
    // stranded a dozen blank rows below the tubes they total.
    let footer_area = Rect {
        y: chart_area.y + rendered_rows as u16,
        height: footer_height,
        ..inner
    };

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
            Style::default().fg(STATES[state].2),
        ));
    }
    if bar_width > 0 {
        header.push(Span::styled(
            format!("{}{:<bar$}", " ".repeat(GAP), "size", bar = bar_width),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if show_ewma {
        header.push(Span::styled(
            format!("{:>width$}", "ewma", width = EWMA_WIDTH),
            Style::default().fg(Color::DarkGray),
        ));
    }
    // Measured before the spans are handed over, so the scroll indicator below
    // can place itself without keeping a copy of the whole header alive.
    let header_width: usize = header.iter().map(|s| s.content.chars().count()).sum();
    frame.render_widget(Paragraph::new(vec![Line::from(header)]), header_area);

    if footer_height > 0 {
        render_totals_footer(frame, app, footer_area, &columns, name_width);
    }

    // Scroll position, when the list does not all fit. Right-aligned over the
    // header rather than inline, so it cannot push the column labels out of
    // line with the rows beneath them. A frame with no room for any row at all
    // has no range to report — it used to render "[1-0 of 2]".
    if chart_height > 0 && tubes.len() > chart_height {
        let pos = format!(
            "[{}-{} of {}] ",
            offset + 1,
            (offset + chart_height).min(tubes.len()),
            tubes.len()
        );
        // Drawn over the header, so it must only go where the header is not.
        // On a narrow terminal the labels reach the right edge and this used to
        // paint straight over `buried` — losing the column that matters most to
        // report a scroll position you can infer from the cursor anyway.
        if header_width + pos.chars().count() <= inner.width as usize {
            frame.render_widget(
                Paragraph::new(vec![Line::from(Span::styled(
                    pos,
                    Style::default().fg(Color::DarkGray),
                ))])
                .alignment(Alignment::Right),
                header_area,
            );
        }
    }
}

/// The server's totals, in the columns the rows established, under a rule that
/// spans just the numeric block.
///
/// This is the breakdown the top bar used to carry as 61 columns of
/// `jobs: 1789923 ready, 381 reserved, ...` — as a footer it costs no width at
/// all and needs no labels, because the columns above it already have them.
///
/// It takes the cursor like any other row, because it *is* selectable like any
/// other — that is what turns the detail pane over to the server. It carries no
/// bar or ewma: neither means anything for a server as a whole.
fn render_totals_footer(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    columns: &[usize],
    name_width: usize,
) {
    let Some(server) = app.current.as_ref().map(|s| &s.server) else {
        return;
    };
    let totals = server_counts(server);

    let numeric = GAP + TOTAL_WIDTH + TREND_WIDTH + column_block_width(columns);
    frame.render_widget(
        Paragraph::new(vec![Line::from(vec![
            Span::raw(" ".repeat(CURSOR_WIDTH as usize + name_width + 1)),
            Span::styled(
                "\u{2500}".repeat(numeric.saturating_sub(1)),
                Style::default().fg(Color::DarkGray),
            ),
        ])]),
        Rect { height: 1, ..area },
    );

    let on_server = app.server_selected();
    let mut row = row_prefix(
        on_server,
        Selection::Server.label(),
        name_width,
        if on_server {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        },
        totals.iter().sum(),
        Style::default().add_modifier(Modifier::BOLD),
        None,
    );
    row.extend(count_spans(&totals, columns, Modifier::BOLD));

    frame.render_widget(
        Paragraph::new(vec![Line::from(row)]),
        Rect { y: area.y + 1, height: 1, ..area },
    );
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

/// The state columns, which is all of them, always.
///
/// This used to drop any state that neither the visible tubes nor the server had
/// jobs in, on the reasoning that alignment should not cost width it was not
/// using. That was the wrong axis to optimise. The columns held still down the
/// screen but moved *between polls*: a tube's last delayed job finishing removed
/// the `delay` column and shifted everything after it sideways, so on a live
/// dashboard the numbers you were reading jumped under you. Stability over time
/// is worth more than the seven columns, and all five still fit an 80-column
/// terminal with the bar at its floor.
fn active_columns() -> Vec<usize> {
    (0..STATES.len()).collect()
}

/// What one look at the visible tubes tells the layout.
///
/// Gathered in a single pass rather than one question at a time: these three
/// answers used to cost three separate scans of the list, on every frame.
struct ChartMetrics {
    /// Widest name on screen, capped — past `NAME_MAX` a name is buying width
    /// from the columns that carry the numbers.
    max_name_len: usize,
    any_paused: bool,
    /// The busiest visible tube, which sets the bar scale so that bar lengths
    /// are comparable down the column.
    max_total: u64,
}

impl ChartMetrics {
    fn measure(tubes: &[&TubeStats]) -> Self {
        let mut metrics = ChartMetrics {
            // The totals row is a row, so its label negotiates for the name
            // column like any tube's. Left out, a list of short names sized the
            // column under "server" — which then either overflowed, pushing the
            // footer's numbers out of line with the columns they total, or
            // truncated to "s".
            max_name_len: Selection::Server.label().chars().count(),
            any_paused: false,
            max_total: 0,
        };
        for tube in tubes {
            // Counted in characters, not bytes, to match `truncate_name`.
            metrics.max_name_len = metrics.max_name_len.max(tube.name.chars().count());
            metrics.any_paused |= tube.is_paused();
            metrics.max_total = metrics.max_total.max(tube.current_total());
        }
        metrics.max_name_len = metrics.max_name_len.min(NAME_MAX);
        metrics
    }
}

/// Width of the `n`th count field, gap included. The first field carries the
/// full gap that separates it from the trend chevron; the rest sit one column
/// apart. Header and row use this same function, which is what keeps them
/// aligned.
fn count_field_width(n: usize) -> usize {
    COUNT_WIDTH + if n == 0 { GAP } else { 1 }
}

/// The count block of a chart row: one right-aligned field per active column,
/// each in its state's colour.
///
/// A state holding nothing gets a dim placeholder rather than a zero: the column
/// has to keep its place, but a screenful of zeros reads as data. Shared by the
/// tube rows and the totals footer so the two cannot drift out of alignment —
/// which is the whole point of the columns.
fn count_spans(counts: &[u64; 5], columns: &[usize], emphasis: Modifier) -> Vec<Span<'static>> {
    columns
        .iter()
        .enumerate()
        .map(|(n, &state)| {
            let (text, style) = match counts[state] {
                0 => ("\u{00b7}".to_string(), Style::default().fg(Color::DarkGray)),
                count => (
                    format_number(count),
                    Style::default().fg(STATES[state].2).add_modifier(emphasis),
                ),
            };
            Span::styled(format!("{:>width$}", text, width = count_field_width(n)), style)
        })
        .collect()
}

/// The leading block of a chart row: cursor, right-aligned name, total, and the
/// trend column.
///
/// The trend's width is held even when there is no chevron for it — on the first
/// poll, or on the totals row — so the counts after it start in the same place
/// on every row.
fn row_prefix(
    selected: bool,
    name: &str,
    name_width: usize,
    name_style: Style,
    total: u64,
    total_style: Style,
    trend: Option<(&'static str, Color)>,
) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            if selected { "\u{258c}" } else { " " },
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            format!("{:>width$}", truncate_name(name, name_width), width = name_width),
            name_style,
        ),
        Span::styled(
            format!("{:>width$}", format_number(total), width = GAP + TOTAL_WIDTH),
            total_style,
        ),
        match trend {
            Some((chevron, colour)) => {
                Span::styled(format!(" {chevron}"), Style::default().fg(colour))
            }
            None => Span::raw(" ".repeat(TREND_WIDTH)),
        },
    ]
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
            Span::styled("\u{2588}".repeat(n), Style::default().fg(STATES[i].2))
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
    rates: (f64, f64, f64, f64),
    detail: Vec<DetailLine>,
    detail_rows: usize,
) {
    let block = Block::default().borders(Borders::TOP).title(" Stats ");

    let Some(snap) = &app.current else {
        frame.render_widget(block, area);
        return;
    };

    let (puts_s, reserves_s, deletes_s, timeouts_s) = rates;

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

    // Whatever the cursor is on, trimmed to the rows the layout allowed. The
    // lines are already in reading order, so a truncated pane keeps its most
    // useful half rather than an arbitrary one.
    if detail_rows > 0 {
        if let Some(name) = app.selected.as_ref().map(Selection::label) {
            let rule = area.width as usize;
            let heading = format!(" {name} ");
            // A dropped line is worth saying so — an absent `oldest` row reads
            // as "no old jobs" rather than "no room" — but it rides the
            // heading's rule rather than taking a row of its own. Spending one
            // of two available lines on a note about the other four would be a
            // poor trade, and a row budgeted for the note is a row the pane
            // cannot use when nothing is dropped.
            let note = match detail.len() - detail_rows {
                0 => String::new(),
                n => format!(" +{n} more (resize taller) "),
            };
            let rule = rule
                .saturating_sub(heading.chars().count())
                .saturating_sub(note.chars().count());
            text.push(Line::from(vec![
                Span::styled(
                    heading,
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::styled("\u{2500}".repeat(rule), Style::default().fg(Color::DarkGray)),
                Span::styled(note, Style::default().fg(Color::DarkGray)),
            ]));
        }
        for line in detail.into_iter().take(detail_rows) {
            text.push(line.into_line());
        }
    }

    let p = Paragraph::new(text).block(block);
    frame.render_widget(p, area);
}

/// One row of the detail pane: a dim label and its content.
///
/// Kept as a struct rather than a finished `Line` so the pane can be built and
/// counted before the layout is decided, then rendered afterwards — the height
/// depends on the content, so the content has to come first.
struct DetailLine {
    label: &'static str,
    spans: Vec<Span<'static>>,
}

impl DetailLine {
    fn into_line(self) -> Line<'static> {
        let mut spans = vec![Span::styled(
            format!(" {:<width$}", self.label, width = DETAIL_LABEL),
            Style::default().fg(Color::DarkGray),
        )];
        spans.extend(self.spans);
        Line::from(spans)
    }
}

/// Join `parts` with a dim `sep`, dropping any that are empty.
///
/// Dropping them is the point: a fragment is omitted by building it empty, so
/// callers can list every field they might show without also having to work out
/// which separators survive.
fn join_with(sep: &str, parts: Vec<Vec<Span<'static>>>) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    for part in parts.into_iter().filter(|p| !p.is_empty()) {
        if !out.is_empty() {
            out.push(Span::styled(
                sep.to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }
        out.extend(part);
    }
    out
}

/// Join `parts` with a dim middot — the detail pane's separator.
fn joined(parts: Vec<Vec<Span<'static>>>) -> Vec<Span<'static>> {
    join_with(" \u{00b7} ", parts)
}

/// `value label` with the value plain and the unit dimmed. An empty label emits
/// no separator — it used to leave a stray space before the next middot.
fn pair(value: String, label: &str) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw(value)];
    if !label.is_empty() {
        spans.push(Span::styled(
            format!(" {label}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    spans
}

/// Everything worth knowing about one tube, in reading order — the lines that
/// survive a squeeze are the ones you would have read first.
///
/// This replaced a table of every tube's timing. A table could compare tubes but
/// had room for six columns and nothing else, which left 19 of `TubeStats`'
/// fields — the oldest job ages, the bury rate, the min/max processing times,
/// the watcher counts — parsed on every poll and never displayed.
fn tube_detail_lines(tube: &TubeStats) -> Vec<DetailLine> {
    let mut lines = Vec::new();
    let counts = tube_counts(tube);

    // Jobs held, by state, in the same colours and order as the chart's columns
    // — spelled out here, where a sentence has the room the headers do not.
    let jobs = joined(
        (0..STATES.len())
            .filter(|&i| counts[i] > 0)
            .map(|i| {
                vec![
                    Span::styled(format_number(counts[i]), Style::default().fg(STATES[i].2)),
                    Span::styled(
                        format!(" {}", STATES[i].1),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]
            })
            .collect(),
    );
    lines.push(DetailLine {
        label: "jobs",
        spans: if jobs.is_empty() {
            vec![Span::styled("empty", Style::default().fg(Color::DarkGray))]
        } else {
            jobs
        },
    });

    // Processing time. The bimodal pair is tuber's distinctive signal, so it
    // leads.
    //
    // The threshold splitting fast from slow is deliberately not repeated here.
    // It is server configuration — the same value for every tube and every poll
    // — so spelling it out on each row cost twelve columns to restate a constant,
    // and the obvious compact form (`79ms < 100ms ewma > 862ms`) reads as a pair
    // of inequalities, one of which happens to be true. It belongs with the rest
    // of the server's settings.
    let mut timing: Vec<Vec<Span<'static>>> = Vec::new();
    if tube.processing_time_samples_fast > 0 {
        timing.push(pair(format_duration(tube.processing_time_ewma_fast), "fast"));
    }
    if tube.processing_time_samples_slow > 0 {
        timing.push(pair(format_duration(tube.processing_time_ewma_slow), "slow"));
    }
    if tube.processing_time_p50 > 0.0 {
        timing.push(pair(format_duration(tube.processing_time_p50), "p50"));
        timing.push(pair(format_duration(tube.processing_time_p95), "p95"));
        timing.push(pair(format_duration(tube.processing_time_p99), "p99"));
    }
    if !timing.is_empty() {
        lines.push(DetailLine { label: "runtime", spans: joined(timing) });
    }

    // How long work has been sitting. An age of zero means nothing in that
    // state, which the jobs line already said, so those are dropped.
    let ages = joined(
        [
            ("ready", tube.oldest_ready_age),
            ("delayed", tube.oldest_delayed_age),
            ("buried", tube.oldest_buried_age),
        ]
        .into_iter()
        .filter(|(_, age)| *age > 0)
        .map(|(label, age)| pair(format_age(age), label))
        .collect(),
    );
    if !ages.is_empty() {
        lines.push(DetailLine { label: "oldest", spans: ages });
    }

    // Waiting time and the spread behind the averages above.
    let mut queue: Vec<Vec<Span<'static>>> = Vec::new();
    if tube.queue_time_ewma > 0.0 {
        queue.push(pair(format_duration(tube.queue_time_ewma), "in queue"));
    }
    if tube.processing_time_samples > 0 {
        queue.push(pair(
            format!(
                "{}\u{2013}{}",
                format_duration(tube.processing_time_min),
                format_duration(tube.processing_time_max)
            ),
            &format!("over {} runs", format_number(tube.processing_time_samples)),
        ));
    }
    if tube.bury_rate > 0.0 {
        queue.push(vec![Span::styled(
            format!("{:.2}/s buried", tube.bury_rate),
            Style::default().fg(Color::Red),
        )]);
    }
    if !queue.is_empty() {
        lines.push(DetailLine { label: "waiting", spans: joined(queue) });
    }

    // Who is attached, and what they have done.
    let clients = joined(vec![
        pair(tube.current_using.to_string(), "using"),
        pair(tube.current_watching.to_string(), "watching"),
        pair(tube.current_waiting.to_string(), "waiting"),
        pair(format_number(tube.total_reserves), "reserves"),
        if tube.total_timeouts > 0 {
            vec![Span::styled(
                format!("{} timeouts", format_number(tube.total_timeouts)),
                Style::default().fg(Color::Yellow),
            )]
        } else {
            Vec::new()
        },
    ]);
    lines.push(DetailLine { label: "clients", spans: clients });

    if tube.is_paused() {
        lines.push(DetailLine {
            label: "paused",
            spans: joined(vec![
                pair(format_secs(tube.pause_time_left), "left"),
                pair(format_secs(tube.pause), "requested"),
                pair(tube.cmd_pause_tube.to_string(), "pauses total"),
            ]),
        });
    }

    lines
}

/// Everything worth knowing about the server, in reading order — rates first,
/// then what could run out, then what has gone wrong, then trivia.
///
/// This is why there is no server page. The cursor already distinguishes a tube
/// from the totals row, so the pane it drives can describe either; a page would
/// have added a mode and a keybinding to say the same thing.
///
/// It also gives a home to the 16 `ServerStats` fields nothing rendered — the
/// WAL index range, the TOAST compaction counters, the corrupted and orphaned
/// body tallies, the command totals, the host triple — and to the fast/slow
/// threshold, which is server configuration and so belongs here rather than
/// restated on every tube's runtime line.
fn server_detail_lines(server: &ServerStats, rates: (f64, f64, f64, f64)) -> Vec<DetailLine> {
    let mut lines = Vec::new();
    let (puts_s, reserves_s, deletes_s, timeouts_s) = rates;

    lines.push(DetailLine {
        label: "rates",
        spans: joined(vec![
            pair(format!("{puts_s:.1}"), "puts/s"),
            pair(format!("{reserves_s:.1}"), "reserves/s"),
            pair(format!("{deletes_s:.1}"), "deletes/s"),
            if timeouts_s > 0.0 {
                vec![Span::styled(
                    format!("{timeouts_s:.1} timeouts/s"),
                    Style::default().fg(Color::Yellow),
                )]
            } else {
                Vec::new()
            },
        ]),
    });

    // What could run out. Percentages carry the same thresholds the top bar
    // uses, so a number that is fine there is not alarming here.
    let mut memory = vec![match server.max_jobs_size {
        0 => pair(format_bytes(server.current_jobs_size), "in jobs"),
        max => {
            let pct = percent(server.current_jobs_size, max);
            vec![
                Span::styled(
                    format!(
                        "{} / {} ({:.1}%)",
                        format_bytes(server.current_jobs_size),
                        format_bytes(max),
                        pct
                    ),
                    Style::default().fg(usage_colour(pct)),
                ),
                Span::styled(" in jobs", Style::default().fg(Color::DarkGray)),
            ]
        }
    }];
    if server.rusage_maxrss > 0 {
        memory.push(pair(format_bytes(server.rusage_maxrss), "peak RSS"));
    }
    if server.max_job_size > 0 {
        memory.push(pair(format_bytes(server.max_job_size), "max job"));
    }
    lines.push(DetailLine { label: "memory", spans: joined(memory) });

    if server.max_storage_bytes > 0 {
        let used = server.binlog_total_bytes + server.toast_total_bytes;
        let pct = percent(used, server.max_storage_bytes);
        lines.push(DetailLine {
            label: "disk",
            spans: vec![Span::styled(
                format!(
                    "{} / {} ({:.1}%)",
                    format_bytes(used),
                    format_bytes(server.max_storage_bytes),
                    pct
                ),
                Style::default().fg(usage_colour(pct)),
            )],
        });
    }

    if server.binlog_enabled {
        lines.push(DetailLine {
            label: "wal",
            spans: joined(vec![
                pair(format_bytes(server.binlog_total_bytes), "written"),
                pair(
                    format!("{} file{}", server.binlog_file_count, plural(server.binlog_file_count as usize)),
                    "on disk",
                ),
                // The index range says how far replay would have to reach.
                if server.binlog_current_index > 0 {
                    pair(
                        format!("{}\u{2013}{}", server.binlog_oldest_index, server.binlog_current_index),
                        "index",
                    )
                } else {
                    Vec::new()
                },
            ]),
        });
    }

    if server.toast_segments > 0 || server.toast_total_bytes > 0 {
        let mut toast = Vec::new();
        if server.toast_total_bytes > 0 {
            let pct = percent(server.toast_live_bytes, server.toast_total_bytes);
            // Abbreviated deliberately: with every counter present this line is
            // the widest the pane produces, and it has to fit 80 columns.
            toast.push(pair(
                format!(
                    "{} / {} {:.0}% live",
                    format_bytes(server.toast_live_bytes),
                    format_bytes(server.toast_total_bytes),
                    pct
                ),
                "",
            ));
        }
        toast.push(pair(format!("{} segs", server.toast_segments), ""));
        if server.toast_compactions_total > 0 {
            toast.push(pair(format_number(server.toast_compactions_total), "compacted"));
        }
        if server.toast_bodies_migrated_total > 0 {
            toast.push(pair(format_number(server.toast_bodies_migrated_total), "migrated"));
        }
        lines.push(DetailLine { label: "toast", spans: joined(toast) });
    }

    // Anything the server has had to repair or give up on. Absent when all
    // zero, which is the normal case — so its presence is itself the signal.
    let repairs = joined(
        [
            ("corrupted", server.toast_bodies_dropped_corrupted, Color::Red),
            ("recovered", server.recovered_missing_bodies, Color::Yellow),
            ("orphans", server.reclaimed_orphan_bodies, Color::Yellow),
            ("stranded", server.reclaimed_stranded_bodies, Color::Yellow),
        ]
        .into_iter()
        .filter(|(_, n, _)| *n > 0)
        .map(|(label, n, colour)| {
            vec![Span::styled(
                format!("{} {label}", format_number(n)),
                Style::default().fg(colour),
            )]
        })
        .collect(),
    );
    if !repairs.is_empty() {
        lines.push(DetailLine { label: "repairs", spans: repairs });
    }

    lines.push(DetailLine {
        label: "clients",
        spans: joined(vec![
            pair(server.current_connections.to_string(), "connections"),
            pair(server.current_producers.to_string(), "producers"),
            pair(server.current_workers.to_string(), "workers"),
            pair(server.current_waiting.to_string(), "waiting"),
        ]),
    });

    lines.push(DetailLine {
        label: "cpu",
        spans: joined(vec![
            pair(format!("{:.2}", server.rusage_utime), "user"),
            pair(format!("{:.2}", server.rusage_stime), "system"),
        ]),
    });

    lines.push(DetailLine {
        label: "commands",
        spans: joined(vec![
            pair(format_number(server.cmd_put), "puts"),
            pair(
                format_number(server.cmd_reserve + server.cmd_reserve_with_timeout),
                "reserves",
            ),
            pair(format_number(server.cmd_delete), "deletes"),
            if server.job_timeouts > 0 {
                vec![Span::styled(
                    format!("{} timeouts", format_number(server.job_timeouts)),
                    Style::default().fg(Color::Yellow),
                )]
            } else {
                Vec::new()
            },
        ]),
    });

    lines.push(DetailLine {
        label: "tuning",
        spans: joined(vec![
            pair(format_duration(server.processing_time_fast_threshold), "fast/slow split"),
            pair(format_number(server.total_jobs), "jobs ever"),
        ]),
    });

    let host = joined(
        [
            server.hostname.as_str(),
            server.os.as_str(),
            server.platform.as_str(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .map(|s| vec![Span::raw(s.to_string())])
        .collect(),
    );
    if !host.is_empty() {
        lines.push(DetailLine { label: "host", spans: host });
    }

    lines
}

/// `part` as a percentage of `whole`, for the used-of-available figures the top
/// bar and the server pane both report.
fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    part as f64 / whole as f64 * 100.0
}

/// The colour a used-of-available percentage earns. Shared so the top bar and
/// the server pane cannot disagree about what counts as alarming.
fn usage_colour(pct: f64) -> Color {
    if pct >= 90.0 {
        Color::Red
    } else if pct >= 70.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

/// Seconds as an age: `45s`, `4m12s`, `2h5m`, `3d4h`.
///
/// Distinct from `format_secs`, which rounds minutes *up* so a fresh pause reads
/// back the duration that was asked for. Rounding an age up would overstate it,
/// and an age wants the days that a pause countdown never reaches.
fn format_age(secs: u64) -> String {
    const MIN: u64 = 60;
    const HOUR: u64 = 60 * MIN;
    const DAY: u64 = 24 * HOUR;
    match secs {
        s if s < MIN => format!("{s}s"),
        s if s < HOUR => trim_zero(s / MIN, s % MIN, 'm', 's'),
        s if s < DAY => trim_zero(s / HOUR, (s % HOUR) / MIN, 'h', 'm'),
        s => trim_zero(s / DAY, (s % DAY) / HOUR, 'd', 'h'),
    }
}

/// `4m12s`, or just `4m` when the smaller unit is zero.
fn trim_zero(big: u64, small: u64, big_unit: char, small_unit: char) -> String {
    if small == 0 {
        format!("{big}{big_unit}")
    } else {
        format!("{big}{big_unit}{small}{small_unit}")
    }
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

/// A duration at the scale it reads best: `82ms`, `2.4s`, `13.0m`, `1.5h`.
///
/// Unpadded. Nine of its ten callers are running text in the detail pane, and
/// the tenth — the chart's ewma column — right-aligns it to `EWMA_WIDTH` along
/// with the placeholder that stands in for it, so the column's width is stated
/// once, where the column is built.
fn format_duration(seconds: f64) -> String {
    if seconds < 1.0 {
        format!("{:.0}ms", seconds * 1000.0)
    } else if seconds < 600.0 {
        format!("{:.1}s", seconds)
    } else if seconds < 3600.0 {
        format!("{:.1}m", seconds / 60.0)
    } else {
        format!("{:.1}h", seconds / 3600.0)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::Instant;
    use crate::app::Selection;
    use tuber_lib::model::Snapshot;

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
        assert_eq!(rows, 6, "all detail lines shown");
        assert!(chart_rows(45, bottom) >= 19, "all tubes shown without scrolling");
    }

    #[test]
    fn short_terminal_keeps_the_chart_usable() {
        // 80x24 with a 5-row top bar. This is the case the old layout broke:
        // the panel took 19 rows and left the chart at its floor.
        let (bottom, rows) = split_bottom(19, 19, 6);
        assert!(
            chart_rows(19, bottom) >= 8,
            "chart got {} rows, expected it to keep the majority",
            chart_rows(19, bottom)
        );
        // A partial pane is still worth showing, unlike the timing table this
        // replaced: its lines are in reading order, so the ones that survive are
        // the ones that matter. The table needed three rows to say anything.
        assert!(rows > 0 && rows < 6, "expected a partial pane, got {rows}");
    }

    #[test]
    fn medium_terminal_splits_and_truncates() {
        let (bottom, rows) = split_bottom(29, 19, 6);
        assert!(rows > 0 && rows <= 6, "partial pane, got {rows}");
        assert!(chart_rows(29, bottom) >= 12);
    }

    #[test]
    fn a_tall_terminal_shows_the_whole_pane() {
        // Six detail lines is the most a tube ever produces.
        let (_, rows) = split_bottom(45, 8, 6);
        assert_eq!(rows, 6, "all detail lines shown");
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
    fn a_claimed_row_is_always_backed_by_height() {
        // The panel renders a heading row plus `rows` detail lines, so a height
        // that does not cover both would clip the pane it just reserved space
        // for. Swept rather than spot-checked: the budget arithmetic subtracts
        // in two places and an off-by-one hides easily.
        for avail in 0..200 {
            for visible in 0..60 {
                for detail in 0..8 {
                    let (height, rows) = split_bottom(avail, visible, detail);
                    assert!(height <= avail, "avail={avail} -> height {height}");
                    assert!(rows <= detail, "invented {rows} lines from {detail}");
                    if rows == 0 {
                        continue;
                    }
                    assert!(
                        height >= STATS_FIXED + 1 + rows,
                        "avail={avail} detail={detail} -> height {height} too small \
                         for a heading plus {rows} lines"
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
        let mut app = App::new("test:11300".to_string());
        app.update(Snapshot {
            server: ServerStats::from_yaml(
                "version: tuber 0.13.0\ncurrent-jobs-ready: 2103009\n\
                 current-jobs-urgent: 342\ncurrent-jobs-reserved: 381\n\
                 current-jobs-delayed: 30\ncurrent-jobs-buried: 2196\n",
            ),
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
        let lines = draw_chart(&app, 130, 13);
        for line in &lines {
            println!("{line}");
        }

        // Line 0 is the block title; the column header is the first inner row.
        // Every state is in play here, reserved included — maintenance has one.
        let header = &lines[HEADER_LINE];
        for state in ["total", "ready", "urgnt", "resvd", "delay", "buried", "size"] {
            assert!(header.contains(state), "header {header:?} lacks {state}");
        }

        // Tube rows run from the header to the footer's rule. Found rather than
        // counted, so the test does not silently start asserting about the rule
        // or the totals row if the chrome changes again.
        let rule = lines
            .iter()
            .position(|l| !l.trim().is_empty() && l.trim().chars().all(|c| c == '\u{2500}'))
            .expect("no totals rule");
        let rows: Vec<Vec<char>> = lines[HEADER_LINE + 1..rule]
            .iter()
            .chain(std::iter::once(&lines[rule + 1])) // the totals row aligns too
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.chars().collect())
            .collect();
        assert!(rows.len() >= 8, "expected 7 tubes and a totals row, got {}", rows.len());
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
                // Nothing may follow the value inside its own field. The
                // totals row ends at its last column, so there may be no
                // character after it at all.
                if let Some(&next) = row.get(end) {
                    assert_eq!(
                        next, ' ',
                        "{state} column {cell:?} spills past its width on {:?}",
                        row.iter().collect::<String>()
                    );
                }
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

    /// A tube with every field the protocol offers, for the detail pane.
    fn rich_tube() -> TubeStats {
        TubeStats::from_yaml(
            "name: covers\ncurrent-jobs-ready: 1600000\ncurrent-jobs-urgent: 0\n\
             current-jobs-reserved: 0\ncurrent-jobs-delayed: 124\n\
             current-jobs-buried: 23\noldest-ready-age: 252\n\
             oldest-delayed-age: 63\noldest-buried-age: 183600\n\
             bury-rate: 0.02\nprocessing-time-ewma: 0.079\n\
             processing-time-ewma-fast: 0.079\nprocessing-time-samples-fast: 900000\n\
             processing-time-ewma-slow: 0.862\nprocessing-time-samples-slow: 300000\n\
             processing-time-p50: 0.100\nprocessing-time-p95: 1.3\nprocessing-time-p99: 1.6\n\
             processing-time-min: 0.008\nprocessing-time-max: 4.2\n\
             processing-time-samples: 1200000\nqueue-time-ewma: 2502.0\n\
             current-using: 3\ncurrent-watching: 5\ncurrent-waiting: 0\n\
             total-reserves: 1200000\ntotal-timeouts: 4\n",
        )
    }

    fn app_with_selection(tubes: Vec<TubeStats>, selected: &str) -> App {
        let mut app = sample_app(tubes);
        app.selected = Some(Selection::Tube(selected.to_string()));
        app
    }

    /// The content of a detail-pane line, found by its label. `None` when the
    /// pane has no such line. Matches on the label's own column rather than
    /// anywhere in the text: `clients` carries a "0 waiting" count, which a
    /// substring search would mistake for the `waiting` line.
    fn detail_line(lines: &[String], label: &str) -> Option<String> {
        let prefix = format!(" {:<width$}", label, width = DETAIL_LABEL);
        lines
            .iter()
            .find(|l| l.starts_with(&prefix))
            .map(|l| l[prefix.len()..].to_string())
    }

    /// Render the whole frame and give back its lines as plain text.
    fn draw_frame(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
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

    #[test]
    fn the_pane_shows_the_selected_tube_in_full() {
        let app = app_with_selection(vec![rich_tube()], "covers");
        let lines = draw_frame(&app, 130, 24);
        let pane = lines.join("\n");

        for label in ["jobs", "runtime", "oldest", "waiting", "clients"] {
            assert!(
                detail_line(&lines, label).is_some(),
                "pane has no {label} line:\n{pane}"
            );
        }
        // Fields that had nowhere to go before the pane existed.
        for value in ["2d3h", "8ms\u{2013}4.2s", "0.02/s", "5 watching", "4 timeouts"] {
            assert!(pane.contains(value), "pane is missing {value}:\n{pane}");
        }
        // The bimodal pair leads the runtime line. The threshold that splits
        // them is server configuration and is deliberately not restated here.
        let runtime = detail_line(&lines, "runtime").unwrap();
        assert!(runtime.starts_with("79ms fast \u{00b7} 862ms slow"), "runtime reads {runtime:?}");
        assert!(!runtime.contains("100ms fast"), "threshold restated: {runtime:?}");
        // The tube's name heads the pane.
        assert!(lines.iter().any(|l| l.starts_with(" covers \u{2500}")), "no heading:\n{pane}");
    }

    /// beanstalkd reports none of the percentiles, ages or rates that tuber
    /// does, so most of the pane has no data to show. Those lines must be
    /// dropped rather than rendered as a row of dashes.
    #[test]
    fn the_pane_drops_lines_it_has_no_data_for() {
        let bare = TubeStats::from_yaml(
            "name: q\ncurrent-jobs-ready: 2\ncurrent-jobs-urgent: 2\ncurrent-using: 1\n",
        );
        let app = app_with_selection(vec![bare], "q");
        let lines = draw_frame(&app, 130, 24);
        let pane = lines.join("\n");

        assert!(detail_line(&lines, "jobs").is_some(), "jobs is unconditional:\n{pane}");
        assert!(detail_line(&lines, "clients").is_some(), "clients is unconditional:\n{pane}");
        for absent in ["runtime", "oldest", "waiting"] {
            assert!(
                detail_line(&lines, absent).is_none(),
                "{absent} has no data but was rendered:\n{pane}"
            );
        }
        // Urgent is a subset of ready, so a wholly urgent tube shows no plain
        // ready count — the same netting the chart columns do.
        assert_eq!(detail_line(&lines, "jobs").unwrap(), "2 urgent");
    }

    #[test]
    fn nothing_in_the_frame_overflows_its_width() {
        let mut tubes = sample_tubes();
        tubes.push(rich_tube());
        let app = app_with_selection(tubes, "covers");
        for width in 60..=140u16 {
            for line in draw_frame(&app, width, 30) {
                assert!(
                    line.chars().count() <= width as usize,
                    "width {width} overflowed: {line:?}"
                );
            }
        }
    }

    /// The server pane produces the widest lines in the app, so it gets its own
    /// sweep — the tube-selection one above missed a `toast` line running to 94
    /// columns because it never selected the server row.
    #[test]
    fn the_server_pane_fits_eighty_columns() {
        let mut app = busy_server_app();
        app.selected = Some(Selection::Server);
        let lines = draw_frame(&app, 80, 34);
        let pane_labels = ["rates", "memory", "disk", "wal", "toast", "repairs", "clients"];
        for label in pane_labels {
            let content = detail_line(&lines, label)
                .unwrap_or_else(|| panic!("no {label} line:\n{}", lines.join("\n")));
            // The rendered line is a leading space, the label field, then this.
            let rendered = 1 + DETAIL_LABEL + content.chars().count();
            assert!(rendered <= 80, "{label} needs {rendered} columns: {content:?}");
        }
        // The fast/slow threshold has a home again, now that the runtime line
        // no longer restates it.
        assert!(
            detail_line(&lines, "tuning").unwrap().contains("100ms fast/slow split"),
            "threshold missing from the server pane"
        );
        // Fields that were parsed on every poll and rendered nowhere.
        assert!(detail_line(&lines, "wal").unwrap().contains("12\u{2013}46"), "no WAL index range");
        assert!(detail_line(&lines, "repairs").unwrap().contains("2 corrupted"), "no repairs");
        assert!(detail_line(&lines, "commands").unwrap().contains("2.2M puts"), "no commands");
    }

    /// A server with every optional field populated.
    fn busy_server_app() -> App {
        let server = ServerStats::from_yaml(
            "version: tuber 0.13.0\nname: gr\nuptime: 2460\nhostname: hetz01\n\
             os: Linux 6.1\nplatform: x86_64\ncurrent-connections: 105\n\
             current-producers: 68\ncurrent-workers: 35\ncurrent-waiting: 0\n\
             current-jobs-ready: 2103009\ncurrent-jobs-urgent: 342\n\
             current-jobs-reserved: 381\ncurrent-jobs-delayed: 30\n\
             current-jobs-buried: 2196\nrusage-utime: 59.76\nrusage-stime: 29.79\n\
             rusage-maxrss: 1181116006\ncurrent-jobs-size: 917901312\n\
             max-jobs-size: 1181116006\nmax-job-size: 65536\n\
             binlog-enabled: true\nbinlog-total-bytes: 470220800\n\
             binlog-file-count: 46\nbinlog-current-index: 46\nbinlog-oldest-index: 12\n\
             toast-total-bytes: 754974720\ntoast-live-bytes: 568377344\n\
             toast-segments: 12\ntoast-compactions-total: 318\n\
             toast-bodies-migrated-total: 41200\ntoast-bodies-dropped-corrupted: 2\n\
             reclaimed-orphan-bodies: 7\nmax-storage-bytes: 2147483648\n\
             cmd-put: 2200000\ncmd-reserve: 1900000\ncmd-delete: 2100000\n\
             job-timeouts: 14\ntotal-jobs: 2200000\n\
             processing-time-fast-threshold: 0.1\n",
        );
        let mut app = App::new("test:11300".to_string());
        app.update(Snapshot { server, tubes: sample_tubes(), fetched_at: Instant::now() });
        app
    }

    /// Down past the last tube lands on the totals row, and the tube commands
    /// have nothing to act on there.
    #[test]
    fn the_cursor_reaches_the_server_row() {
        let mut app = busy_server_app();
        let tubes = app.visible_tubes().len();
        app.selected = Some(Selection::Tube("book".to_string()));
        for _ in 0..tubes {
            app.move_selection(1);
        }
        assert!(app.server_selected(), "down past the last tube missed the server row");
        assert!(app.selected_tube().is_none(), "the server row is not a tube");
        // And back up again onto the last tube.
        app.move_selection(-1);
        assert!(!app.server_selected());
        assert!(app.selected_tube().is_some(), "up from the server row missed the tubes");
    }

    /// A squeezed pane keeps its first lines and says how many it dropped, in
    /// the heading rather than in a row of its own.
    #[test]
    fn a_squeezed_pane_reports_what_it_dropped() {
        let app = app_with_selection(vec![rich_tube()], "covers");
        let short_lines = draw_frame(&app, 80, 18);
        let short = short_lines.join("\n");
        assert!(short.contains("more (resize taller)"), "no note:\n{short}");
        assert!(detail_line(&short_lines, "jobs").is_some(), "dropped the first line");
        assert!(detail_line(&short_lines, "clients").is_none(), "kept the last line");

        // With room, no note and every line present.
        let tall_lines = draw_frame(&app, 80, 26);
        let tall = tall_lines.join("\n");
        assert!(!tall.contains("resize taller"), "note shown with room:\n{tall}");
        assert!(detail_line(&tall_lines, "clients").is_some(), "last line missing:\n{tall}");
    }

    #[test]
    fn ages_read_as_ages() {
        assert_eq!(format_age(0), "0s");
        assert_eq!(format_age(45), "45s");
        assert_eq!(format_age(60), "1m");
        assert_eq!(format_age(252), "4m12s");
        assert_eq!(format_age(3600), "1h");
        assert_eq!(format_age(7500), "2h5m");
        assert_eq!(format_age(183_600), "2d3h");
        assert_eq!(format_age(172_800), "2d");
        // Unlike format_secs, an age is not rounded up: 4m12s is not "5m".
        assert_eq!(format_secs(252), "5m");
    }

    /// The bar that started this: three lines, the widest 126 columns, clipping
    /// mid-number on a 94-column terminal.
    #[test]
    fn the_top_bar_fits_eighty_columns() {
        let app = busy_server_app();
        let lines = draw_frame(&app, 80, 30);
        // Title, then the bar's own lines, then the chart's bottom border.
        for (i, line) in lines.iter().take(3).enumerate() {
            assert!(
                line.chars().count() <= 80,
                "top bar line {i} needs {} columns: {line:?}",
                line.chars().count()
            );
        }
        let bar = lines[1..3].join("\n");
        // What it keeps: identity, and the headroom you want to be interrupted by.
        for kept in ["tuber 0.13.0", "up 41m", "workers:35", "Mem:", "TOAST:", "Disk:"] {
            assert!(bar.contains(kept), "top bar lost {kept}:\n{bar}");
        }
        // What moved to the server's detail row, where there is room for it.
        for moved in ["CPU", "RSS", "ready,", "files"] {
            assert!(!bar.contains(moved), "top bar still carries {moved}:\n{bar}");
        }
    }

    /// The address we dialled titles the frame. It is the only identity of the
    /// server the client is certain of — the reported hostname is the server's
    /// own view, which in a container is the container ID.
    #[test]
    fn the_title_names_the_server_we_dialled() {
        let app = busy_server_app();
        assert!(
            draw_frame(&app, 80, 30)[0].contains("test:11300"),
            "no address in the title"
        );
    }

    /// A server with no storage limits to report — beanstalkd — has nothing for
    /// the second line, and gets a shorter bar rather than a row of blanks.
    #[test]
    fn the_bar_shrinks_when_there_is_no_headroom_to_report() {
        let mut app = App::new("test:11300".to_string());
        app.update(Snapshot {
            server: ServerStats::from_yaml("version: 1.13\ncurrent-connections: 1\n"),
            tubes: vec![TubeStats::from_yaml("name: q\ncurrent-jobs-ready: 2\n")],
            fetched_at: Instant::now(),
        });
        assert_eq!(top_bar_lines(&app).len(), 1, "no storage means one line");
        assert_eq!(top_bar_lines(&busy_server_app()).len(), 2);
    }

    /// The trailing duration is the tube's overall processing ewma, and it now
    /// says so. It was the one unlabelled number in the table — brackets were
    /// standing in for a column heading.
    #[test]
    fn the_ewma_column_is_labelled_and_aligned() {
        let mut tubes = sample_tubes();
        // A tube holding work but with no samples yet, so the column is empty.
        tubes.push(TubeStats::from_yaml("name: idle-tube\ncurrent-jobs-ready: 3\n"));
        let app = app_with_selection(tubes, "covers");
        let lines = draw_chart(&app, 100, 15);

        let header = &lines[HEADER_LINE];
        let end = header.find("ewma").expect("no ewma header") + "ewma".len();
        assert!(!header[..end].contains('('), "brackets left in the header");

        // Every tube row's value ends in the header's last column, whether it
        // has a figure or the placeholder.
        let rule = lines
            .iter()
            .position(|l| !l.trim().is_empty() && l.trim().chars().all(|c| c == '\u{2500}'))
            .expect("no totals rule");
        let mut seen_placeholder = false;
        for line in &lines[HEADER_LINE + 1..rule] {
            let row: Vec<char> = line.chars().collect();
            if row.is_empty() {
                continue;
            }
            assert_ne!(row[end - 1], ' ', "ewma not flush with its header: {line:?}");
            seen_placeholder |= row[end - 1] == '\u{00b7}';
            assert!(!line.contains('('), "brackets left in a row: {line:?}");
        }
        assert!(seen_placeholder, "a tube with no samples should show a placeholder");

        // The bar gives way before the ewma does, so at 70 the column is still
        // there with no bar beside it.
        let seventy = &draw_chart(&app, 70, 15)[HEADER_LINE];
        assert!(seventy.contains("ewma"), "ewma dropped before the bar: {seventy:?}");
        assert!(!seventy.contains("size"), "bar kept at 70 columns: {seventy:?}");
        // Narrower still and the column goes too, label and all.
        assert!(!draw_chart(&app, 65, 15)[HEADER_LINE].contains("ewma"));
    }

    /// Every state keeps its column whether anything is using it or not.
    ///
    /// The columns used to be chosen per frame from what had jobs in it, which
    /// held them still down the screen but moved them *between polls*: a tube's
    /// last delayed job finishing dropped the `delay` column and slid every
    /// number after it sideways, on a display that repaints every second and a
    /// half. Fixed columns that are sometimes empty beat columns that move.
    #[test]
    fn every_state_keeps_its_column() {
        let quiet: Vec<TubeStats> = sample_tubes()
            .into_iter()
            .filter(|t| t.name == "book" || t.name == "series")
            .collect();
        let mut app = App::new("test:11300".to_string());
        app.update(Snapshot {
            // Nothing anywhere — tube or server — is urgent or reserved.
            server: ServerStats::from_yaml(
                "version: tuber 0.13.0\ncurrent-jobs-ready: 10004\n\
                 current-jobs-delayed: 1\ncurrent-jobs-buried: 145\n",
            ),
            tubes: quiet,
            fetched_at: Instant::now(),
        });
        let header = &draw_chart(&app, 130, 13)[HEADER_LINE];
        for state in STATES.iter().map(|(header, _, _)| *header) {
            assert!(header.contains(state), "header dropped {state}: {header:?}");
        }
    }

    /// The column set does not depend on the data, so it cannot shift as the
    /// data changes. This is the property the previous behaviour lacked.
    #[test]
    fn the_columns_do_not_move_between_polls() {
        let busy = TubeStats::from_yaml(
            "name: q\ncurrent-jobs-ready: 5\ncurrent-jobs-urgent: 2\n\
             current-jobs-reserved: 1\ncurrent-jobs-delayed: 3\ncurrent-jobs-buried: 4\n",
        );
        // The same tube one poll later, with only ready jobs left.
        let drained = TubeStats::from_yaml("name: q\ncurrent-jobs-ready: 5\n");

        let header_for = |tube: TubeStats| {
            let mut app = App::new("test:11300".to_string());
            app.update(Snapshot {
                server: ServerStats::from_yaml("version: tuber 0.13.0\ncurrent-jobs-ready: 5\n"),
                tubes: vec![tube],
                fetched_at: Instant::now(),
            });
            draw_chart(&app, 130, 13)[HEADER_LINE].clone()
        };

        assert_eq!(
            header_for(busy),
            header_for(drained),
            "the header moved when the tube's states emptied"
        );
    }

    /// The totals row carries the server's own figures, which can exceed
    /// anything on screen — 2196 buried jobs matter whether or not the tube
    /// holding them is visible.
    #[test]
    fn the_totals_row_reports_the_server_not_the_screen() {
        let mut app = App::new("test:11300".to_string());
        app.update(Snapshot {
            server: ServerStats::from_yaml(
                "version: tuber 0.13.0\ncurrent-jobs-ready: 5\ncurrent-jobs-buried: 2196\n",
            ),
            // One tube, holding nothing buried at all.
            tubes: vec![TubeStats::from_yaml("name: q\ncurrent-jobs-ready: 5\n")],
            fetched_at: Instant::now(),
        });
        let lines = draw_chart(&app, 130, 13);
        let footer = lines.iter().find(|l| l.contains("server")).expect("no totals row");
        assert!(footer.contains("2.2K"), "totals row lacks the buried count: {footer:?}");
        // And the tube itself shows the placeholder, not the server's number.
        let row = lines.iter().find(|l| l.contains("q ") || l.trim_start().starts_with('q'));
        assert!(row.is_some(), "no tube row:\n{}", lines.join("\n"));
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
    fn no_selected_tube_means_no_pane() {
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
