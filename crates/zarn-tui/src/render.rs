use crate::brand;
use crate::markdown;
use crate::popup;
use crate::state::{App, Block, CardStatus, COLLAPSED_LINES, CompareSide, Mode, Overlay, ToolCard};
use crate::views;
use crate::theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as Border, BorderType, Borders, Paragraph, Wrap};
use serde_json::Value;

const MAX_INPUT_ROWS: u16 = 8;
pub const MAX_TODO_ROWS: usize = 8;

fn todo_list(frame: &mut Frame, app: &App, area: Rect) {
    let done = app.todos.iter().filter(|t| t.status == "done").count();
    let mut lines = vec![Line::from(vec![
        Span::styled("todo ", Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{done}/{} done", app.todos.len()), Style::default().fg(theme::dim())),
    ])];
    let current = app.todos.iter().position(|t| t.status == "in_progress").unwrap_or(0);
    let start = current.saturating_sub(MAX_TODO_ROWS.saturating_sub(1)).min(app.todos.len().saturating_sub(MAX_TODO_ROWS));
    for todo in app.todos.iter().skip(start).take(MAX_TODO_ROWS) {
        let (mark, style) = match todo.status.as_str() {
            "done" => ("☑", Style::default().fg(theme::dim()).add_modifier(Modifier::CROSSED_OUT)),
            "in_progress" => ("◐", Style::default().fg(theme::warn()).add_modifier(Modifier::BOLD)),
            _ => ("☐", Style::default()),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {mark} "), style),
            Span::styled(todo.content.clone(), style),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

pub fn draw(frame: &mut Frame, app: &App) {
    theme::set(app.theme.palette);
    match &app.mode {
        Mode::Panel(panel) => return views::draw_panel(frame, panel, app.theme.accent),
        Mode::Onboarding(flow) => return views::draw_onboarding(frame, flow, app.theme.accent),
        Mode::Chat => {}
    }
    let input_rows = (app.input.split('\n').count() as u16 + 2).clamp(3, MAX_INPUT_ROWS);
    let todo_rows = if app.todos.is_empty() {
        0
    } else {
        app.todos.len().min(MAX_TODO_ROWS) as u16 + 1
    };
    let agent_rows = app.agents.len().min(MAX_AGENT_ROWS) as u16;
    let layout = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(agent_rows),
        Constraint::Length(todo_rows),
        Constraint::Length(1),
        Constraint::Length(input_rows),
    ])
    .split(frame.area());
    let areas = [layout[0], layout[3], layout[4]];

    scrollback(frame, app, areas[0]);
    if agent_rows > 0 {
        agent_list(frame, app, layout[1]);
    }
    if todo_rows > 0 {
        todo_list(frame, app, layout[2]);
    }
    status(frame, app, areas[1]);
    input(frame, app, areas[2]);

    if app.popup_visible() {
        let shown = app.popup_matches();
        let selected = app.popup_selected.min(shown.len().saturating_sub(1));
        popup::draw(frame, areas[0], &shown, selected, app.theme.accent);
    } else if app.mention_visible() {
        let entries = app.mention_entries();
        let shown: Vec<&popup::Entry> = entries.iter().collect();
        let selected = app.popup_selected.min(shown.len().saturating_sub(1));
        popup::draw_with(frame, areas[0], &shown, selected, app.theme.accent, '@');
    }

    match &app.overlay {
        Some(Overlay::Pick(picker, _)) => {
            views::draw_picker(frame, areas[0], picker, app.theme.accent)
        }
        Some(Overlay::Secret { input, provider }) => views::draw_secret(
            frame,
            areas[0],
            &format!("API key for {provider}"),
            input,
            "Paste the key and press enter; characters are not shown. It goes to your OS credential store (or an owner-only fallback file), never to a config file. esc cancels.",
            app.theme.accent,
        ),
        Some(Overlay::Confirm(text, _)) => views::draw_confirm(frame, areas[0], text, app.theme.accent),
        None => {}
    }

    if let Some(review) = &app.plan_review
        && review.decided.is_none()
    {
        views::draw_plan(frame, areas[0], review, app.theme.accent);
    }
}

pub const MAX_AGENT_ROWS: usize = 6;

pub fn agent_line(row: &crate::state::AgentRow, tick: usize, accent: Color) -> Line<'static> {
    let (mark, style) = match row.done {
        None => (crate::settings::SPINNER[tick % crate::settings::SPINNER.len()].to_string(), Style::default().fg(accent)),
        Some(true) => ("✓".to_string(), Style::default().fg(theme::ok())),
        Some(false) => ("✗".to_string(), Style::default().fg(theme::error())),
    };
    let doing = match (&row.tool, row.done) {
        (_, Some(true)) => "done".to_string(),
        (_, Some(false)) => "failed".to_string(),
        (Some(tool), None) => tool.clone(),
        (None, None) => "thinking".to_string(),
    };
    let mut spans = vec![
        Span::styled(format!("{mark} "), style),
        Span::styled(row.agent.clone(), Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {doing}"), Style::default()),
        Span::styled(
            format!(
                "  ·  {}  ·  ~{} tok",
                elapsed_label(row.started.elapsed().as_secs()),
                compact(row.tokens)
            ),
            Style::default().fg(theme::dim()),
        ),
    ];
    if row.background {
        spans.push(Span::styled(format!("  ·  background {}", row.id), Style::default().fg(theme::dim())));
    }
    Line::from(spans)
}

fn agent_list(frame: &mut Frame, app: &App, area: Rect) {
    let lines: Vec<Line> = app
        .agents
        .iter()
        .take(MAX_AGENT_ROWS)
        .map(|row| agent_line(row, app.tick, app.theme.accent))
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn line_text(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

pub fn search_hits(lines: &[Line], query: &str) -> Vec<usize> {
    let needle = query.to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| line_text(l).to_lowercase().contains(&needle))
        .map(|(i, _)| i)
        .collect()
}

fn transcript_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::styled(
            "transcript · every tool output in full · ↑↓ pgup pgdn scroll · esc or ctrl+o returns",
            Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
    ];
    for block in &app.blocks {
        match block {
            Block::Tool(card) => lines.extend(card_lines_full(card, true)),
            other => lines.extend(block_lines(other, app.theme.accent, width)),
        }
        lines.push(Line::raw(""));
    }
    lines
}

fn scroll_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    if app.transcript {
        return transcript_lines(app, width);
    }
    let mut lines = Vec::new();
    if app.theme.banner {
        lines = brand::header_in(&app.status.model, app.theme.accent);
        lines.push(Line::raw(""));
    }
    for block in &app.blocks {
        lines.extend(block_lines(block, app.theme.accent, width));
        lines.push(Line::raw(""));
    }
    if let Some(request) = &app.pending {
        lines.extend(permission_lines(request, app.pending_expanded));
    }
    lines
}

fn scrollback(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines = scroll_lines(app, area.width as usize);

    let width = area.width.max(1) as usize;
    let rows: Vec<usize> = lines.iter().map(|line| line.width().max(1).div_ceil(width)).collect();
    let total = rows.iter().sum::<usize>().min(u16::MAX as usize) as u16;
    let bottom = total.saturating_sub(area.height);
    let scroll = if app.transcript { app.transcript_scroll } else { app.scroll_up };
    let mut offset = bottom.saturating_sub(scroll);

    if let Some(search) = &app.search {
        let hits = search_hits(&lines, &search.query);
        if !hits.is_empty() {
            let current = hits[hits.len() - 1 - search.hit % hits.len()];
            for &i in &hits {
                let style = if i == current {
                    Style::default().fg(Color::Black).bg(theme::warn())
                } else {
                    Style::default().add_modifier(Modifier::UNDERLINED)
                };
                lines[i] = lines[i].clone().patch_style(style);
            }
            if !search.typing {
                let row = rows[..current].iter().sum::<usize>().min(u16::MAX as usize) as u16;
                if row < offset || row >= offset.saturating_add(area.height) {
                    offset = row.saturating_sub(area.height / 2).min(bottom);
                }
            }
        }
    }

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((offset, 0)),
        area,
    );
}

pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(4);
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let mut line = String::new();
        for word in raw.split(' ') {
            let needed = line.chars().count() + usize::from(!line.is_empty()) + word.chars().count();
            if needed > width && !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            let mut rest: String = word.to_string();
            while rest.chars().count() > width {
                let head: String = rest.chars().take(width).collect();
                out.push(head);
                rest = rest.chars().skip(width).collect();
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&rest);
        }
        out.push(line);
    }
    out
}

fn compare_lines(sides: &[CompareSide], accent: Color, width: usize) -> Vec<Line<'static>> {
    let col = width.saturating_sub(3).max(8) / 2;
    let edge = Style::default().fg(theme::dim());
    let columns: Vec<Vec<(String, Style)>> = sides
        .iter()
        .map(|side| {
            let mut rows = vec![(
                truncate(&side.label, col),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            )];
            rows.push((truncate(&side.meta, col), edge));
            let body = if side.failed {
                Style::default().fg(theme::error())
            } else {
                Style::default()
            };
            rows.extend(wrap(&side.text, col).into_iter().map(|l| (l, body)));
            rows
        })
        .collect();
    let height = columns.iter().map(Vec::len).max().unwrap_or(0);
    (0..height)
        .map(|i| {
            let cell = |c: usize| {
                columns
                    .get(c)
                    .and_then(|rows| rows.get(i))
                    .cloned()
                    .unwrap_or_default()
            };
            let (left, left_style) = cell(0);
            let (right, right_style) = cell(1);
            let pad = col.saturating_sub(left.chars().count());
            Line::from(vec![
                Span::styled(left, left_style),
                Span::raw(" ".repeat(pad)),
                Span::styled(" │ ", edge),
                Span::styled(right, right_style),
            ])
        })
        .collect()
}

fn block_lines(block: &Block, accent: Color, width: usize) -> Vec<Line<'static>> {
    match block {
        Block::Compare(sides) => compare_lines(sides, accent, width),
        Block::User(text) => text
            .split('\n')
            .map(|l| {
                Line::from(vec![
                    Span::styled("› ", Style::default().fg(accent)),
                    Span::styled(
                        l.to_string(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ])
            })
            .collect(),
        Block::Assistant(text) => markdown::render(text),
        Block::Notice(text) => text
            .split('\n')
            .map(|l| Line::styled(l.to_string(), Style::default().fg(theme::dim())))
            .collect(),
        Block::Tool(card) => card_lines(card),
        Block::Queued(text) => text
            .split('\n')
            .enumerate()
            .map(|(i, l)| {
                let mut spans = vec![
                    Span::styled("› ", Style::default().fg(theme::dim())),
                    Span::styled(l.to_string(), Style::default().fg(theme::dim())),
                ];
                if i == 0 {
                    spans.push(Span::styled("  (queued)", Style::default().fg(theme::warn())));
                }
                Line::from(spans)
            })
            .collect(),
    }
}

fn status_style(status: CardStatus) -> Style {
    let color = match status {
        CardStatus::Pending => theme::dim(),
        CardStatus::Running => theme::warn(),
        CardStatus::Done => theme::ok(),
        CardStatus::Failed => theme::error(),
    };
    Style::default().fg(color)
}

fn arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

fn result_lines(output: &str) -> usize {
    output
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("[truncated") && !l.starts_with("[stopped"))
        .count()
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn untab(line: &str) -> String {
    line.replace('\t', "  ")
}

pub fn card_summary(card: &ToolCard) -> Option<String> {
    let done = card.output.as_deref().filter(|_| card.status == CardStatus::Done);
    match card.name.as_str() {
        "read" => {
            let path = arg(&card.args, "path");
            let from = card.args.get("offset").and_then(Value::as_u64).unwrap_or(1);
            Some(match done {
                Some(out) => {
                    let n = out
                        .lines()
                        .filter(|l| l.split('\t').next().is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit())))
                        .count() as u64;
                    if n == 0 {
                        path.to_string()
                    } else {
                        format!("{path} · lines {from}–{}", from + n - 1)
                    }
                }
                None => path.to_string(),
            })
        }
        "grep" => {
            let pattern = arg(&card.args, "pattern");
            Some(match done {
                Some(out) if out.starts_with("no matches") => format!("'{pattern}' · 0 matches"),
                Some(out) => format!("'{pattern}' · {}", plural(result_lines(out), "match", "matches")),
                None => format!("'{pattern}'"),
            })
        }
        "glob" => {
            let pattern = arg(&card.args, "pattern");
            Some(match done {
                Some(out) if out.starts_with("no files") => format!("{pattern} · 0 files"),
                Some(out) => format!("{pattern} · {}", plural(result_lines(out), "file", "files")),
                None => pattern.to_string(),
            })
        }
        "edit" | "write" | "multiedit" => Some(
            card.detail
                .clone()
                .unwrap_or_else(|| arg(&card.args, "path").to_string()),
        ),
        "task" => {
            let agent = arg(&card.args, "agent");
            let prompt = arg(&card.args, "prompt");
            Some(if prompt.is_empty() {
                format!("agent {agent}")
            } else {
                format!("agent {agent} · {}", truncate(prompt, 60))
            })
        }
        "bash" => Some(format!(
            "$ {}{}",
            truncate(arg(&card.args, "command"), 100),
            if card.args.get("background").and_then(Value::as_bool) == Some(true) { "  (background)" } else { "" }
        )),
        "webfetch" => Some(truncate(arg(&card.args, "url"), 100)),
        "websearch" => Some(format!("'{}'", truncate(arg(&card.args, "query"), 80))),
        "bash_output" | "bash_kill" => Some(arg(&card.args, "id").to_string()),
        "notebook_edit" => Some(format!(
            "{} · {}",
            arg(&card.args, "path"),
            card.args.get("edit_mode").and_then(Value::as_str).unwrap_or("replace")
        )),
        _ => None,
    }
}

fn card_lines(card: &ToolCard) -> Vec<Line<'static>> {
    card_lines_full(card, card.expanded)
}

fn card_lines_full(card: &ToolCard, full: bool) -> Vec<Line<'static>> {
    let edge = Style::default().fg(theme::dim());
    let summary = card_summary(card);
    let mut header = vec![
        Span::styled("┌ ", edge),
        Span::styled(card.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("  ", edge),
        Span::styled(card.status.label().to_string(), status_style(card.status)),
    ];
    if let Some(summary) = &summary
        && card.name != "bash"
    {
        header.push(Span::styled("  ", edge));
        header.push(Span::raw(summary.clone()));
    }
    let mut lines = vec![Line::from(header)];

    match &summary {
        Some(text) if card.name == "bash" => lines.push(Line::from(vec![
            Span::styled("│ ", edge),
            Span::styled(text.clone(), Style::default().add_modifier(Modifier::BOLD)),
        ])),
        Some(_) => {}
        None => {
            for (key, value) in key_args(&card.args) {
                lines.push(Line::from(vec![
                    Span::styled("│ ", edge),
                    Span::styled(format!("{key}: "), Style::default().fg(theme::dim())),
                    Span::raw(value),
                ]));
            }
        }
    }

    if card.output.is_none() && !card.live.is_empty() {
        let skip = card.live.len().saturating_sub(LIVE_SHOWN);
        if skip > 0 {
            lines.push(Line::from(vec![
                Span::styled("│ ", edge),
                Span::styled(format!("… {skip} earlier lines"), Style::default().fg(theme::dim())),
            ]));
        }
        for line in card.live.iter().skip(skip) {
            lines.push(Line::from(vec![Span::styled("│ ", edge), Span::raw(untab(line))]));
        }
    }

    if let Some(output) = &card.output {
        let all: Vec<&str> = output.lines().collect();
        let shown = if full {
            all.len()
        } else {
            all.len().min(COLLAPSED_LINES)
        };
        for line in &all[..shown] {
            lines.push(Line::from(vec![
                Span::styled("│ ", edge),
                Span::raw(untab(line)),
            ]));
        }
        if all.len() > shown {
            lines.push(Line::from(vec![
                Span::styled("│ ", edge),
                Span::styled(
                    format!("… {} more lines (ctrl+r)", all.len() - shown),
                    Style::default().fg(theme::dim()),
                ),
            ]));
        }
    }

    lines.push(Line::styled("└", edge));
    lines
}

pub const LIVE_SHOWN: usize = 6;

fn key_args(args: &Value) -> Vec<(String, String)> {
    let Value::Object(map) = args else {
        return Vec::new();
    };
    map.iter()
        .take(3)
        .map(|(k, v)| {
            let rendered = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (k.clone(), truncate(&rendered, 120))
        })
        .collect()
}

fn truncate(text: &str, max: usize) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() <= max {
        return flat;
    }
    let kept: String = flat.chars().take(max).collect();
    format!("{kept}…")
}

pub const PREVIEW_LINES: usize = 30;

fn diff_lines(preview: &zarn_tools::diff::Preview, expanded: bool) -> Vec<Line<'static>> {
    use zarn_tools::diff::DiffLine;
    let width = preview
        .lines
        .iter()
        .map(|l| match l {
            DiffLine::Context { old, new, .. } => (*old).max(*new),
            DiffLine::Added { new, .. } => *new,
            DiffLine::Removed { old, .. } => *old,
            DiffLine::Hunk(_) => 0,
        })
        .max()
        .unwrap_or(0)
        .to_string()
        .len();
    let total = preview.lines.len();
    let shown = if expanded { total } else { total.min(PREVIEW_LINES) };
    let mut lines: Vec<Line<'static>> = preview.lines[..shown]
        .iter()
        .map(|line| match line {
            DiffLine::Hunk(text) => Line::styled(format!("  {text}"), Style::default().fg(theme::info())),
            DiffLine::Context { new, text, .. } => Line::styled(
                format!("  {new:>width$}   {text}"),
                Style::default().fg(theme::dim()),
            ),
            DiffLine::Added { new, text } => Line::styled(
                format!("  {new:>width$} + {text}"),
                Style::default().fg(theme::added()),
            ),
            DiffLine::Removed { old, text } => Line::styled(
                format!("  {old:>width$} - {text}"),
                Style::default().fg(theme::removed()),
            ),
        })
        .collect();
    if shown < total {
        lines.push(Line::styled(
            format!("  … {} more diff lines ([e] expands)", total - shown),
            Style::default().fg(theme::dim()),
        ));
    }
    lines
}

fn permission_lines(request: &crate::state::PermissionRequest, expanded: bool) -> Vec<Line<'static>> {
    let warn = Style::default().fg(theme::warn());
    let title = match &request.preview {
        Some(p) if p.new_file => format!("new file {} ({})", p.path, p.summary()),
        Some(p) => format!("{} ({})", p.path, p.summary()),
        None => "wants to run".to_string(),
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!("{} ", request.tool),
            warn.add_modifier(Modifier::BOLD),
        ),
        Span::styled(title, warn),
    ])];
    match &request.preview {
        Some(preview) => lines.extend(diff_lines(preview, expanded)),
        None => {
            for (key, value) in key_args(&request.args) {
                lines.push(Line::raw(format!("  {key}: {value}")));
            }
        }
    }
    lines.push(Line::styled(
        format!("  [y] once   [a] allow {} for this session", request.pattern),
        warn,
    ));
    let expand = if request.preview.is_some() { "   [e] expand" } else { "" };
    lines.push(Line::styled(
        format!("  [p] save that rule to the project   [u] save it to the user config{expand}   [n] no"),
        warn,
    ));
    lines
}

fn status(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    let mode = if app.status.mode.is_empty() { "default" } else { app.status.mode.as_str() };
    let mut parts = Vec::new();
    if let Some(vim) = app.vim {
        parts.push(if vim.normal { "NORMAL".to_string() } else { "INSERT".to_string() });
    }
    parts.extend([
        format!("{mode} mode"),
        app.status.model.clone(),
        format!(
            "{} in / {} out",
            compact(app.status.input_tokens),
            compact(app.status.output_tokens)
        ),
    ]);
    if app.status.cached_tokens > 0 {
        parts.push(format!("{} cached", compact(app.status.cached_tokens)));
    }
    if let Some((used, window)) = app.status.context
        && window > 0
    {
        parts.push(format!("ctx {}%", (u64::from(used) * 100 / u64::from(window)).min(999)));
    }
    if app.theme.show_cost {
        parts.push(match app.status.cost {
            Some(usd) => format!("${usd:.4}"),
            None => "no rate configured".to_string(),
        });
    }
    if !app.queue.is_empty() {
        parts.push(format!("{} queued", app.queue.len()));
    }
    if let Some(reason) = &app.not_ready {
        parts.push(format!("not ready: {reason}"));
    }
    if app.streaming {
        parts.push("esc to cancel".to_string());
    }
    if app.scroll_up > 0 {
        parts.push(format!("scrolled {}", app.scroll_up));
    }

    if let Some(template) = &app.theme.status_line {
        let mut extras: Vec<String> = parts
            .into_iter()
            .filter(|p| {
                p.ends_with(" queued") || p.starts_with("not ready") || p == "esc to cancel" || p.starts_with("scrolled ")
            })
            .collect();
        extras.insert(0, template.render(&status_values(app)));
        parts = extras;
    }
    if let Some(search) = &app.search {
        parts.insert(0, search_label(app, search, width));
    }
    if app.transcript {
        parts.insert(0, "transcript".to_string());
    }

    let mut spans = Vec::new();
    if app.streaming {
        spans.push(brand::frame_in(app.tick, app.theme.accent));
        spans.push(Span::raw(" "));
        if let Some(started) = app.status.turn_started {
            spans.push(Span::styled(
                format!(
                    "{} · ~{} tok  ",
                    elapsed_label(started.elapsed().as_secs()),
                    compact(app.status.turn_tokens())
                ),
                Style::default().fg(app.theme.accent),
            ));
        }
    }
    spans.push(Span::styled(
        parts.join("  ·  "),
        Style::default().fg(theme::dim()),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub fn elapsed_label(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

fn search_label(app: &App, search: &crate::state::Search, width: usize) -> String {
    if search.typing {
        return format!("search: {}▏ (enter to find, esc to stop)", search.query);
    }
    let hits = search_hits(&scroll_lines(app, width), &search.query).len();
    if hits == 0 {
        format!("search '{}': no matches", search.query)
    } else {
        format!("search '{}' {}/{hits} · n older · N newer", search.query, search.hit % hits + 1)
    }
}

pub fn status_values(app: &App) -> crate::status_line::Values {
    let status = &app.status;
    crate::status_line::Values {
        model: status.model.clone(),
        mode: if status.mode.is_empty() { "default".into() } else { status.mode.clone() },
        profile: status.profile.clone(),
        context: match status.context {
            Some((used, window)) if window > 0 => {
                format!("{}%", (u64::from(used) * 100 / u64::from(window)).min(999))
            }
            _ => "-".into(),
        },
        cost: match status.cost {
            Some(usd) => format!("${usd:.4}"),
            None => "no rate".into(),
        },
        cwd: status.cwd.clone(),
        branch: status.branch.clone(),
    }
}

fn compact(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    format!("{:.1}k", n as f64 / 1000.0)
}

fn input(frame: &mut Frame, app: &App, area: Rect) {
    let border = Border::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if app.pending.is_some() {
            theme::warn()
        } else {
            theme::dim()
        }));
    let inner = border.inner(area);

    frame.render_widget(border, area);
    frame.render_widget(Paragraph::new(app.input.clone()), inner);

    if app.pending.is_none() && app.overlay.is_none() {
        let before: String = app.input.chars().take(app.cursor).collect();
        let row = before.matches('\n').count() as u16;
        let col = before.split('\n').next_back().unwrap_or("").chars().count() as u16;
        frame.set_cursor_position(Position::new(
            inner.x + col.min(inner.width.saturating_sub(1)),
            inner.y + row.min(inner.height.saturating_sub(1)),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn card(output: &str, expanded: bool) -> ToolCard {
        ToolCard {
            id: "t".into(),
            name: "bash".into(),
            args: json!({ "command": "ls" }),
            status: CardStatus::Done,
            output: Some(output.into()),
            expanded,
            ..ToolCard::default()
        }
    }

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn long_output_collapses_to_three_lines_plus_a_count() {
        let lines = card_lines(&card("a\nb\nc\nd\ne", false));
        let rendered = text(&lines);
        assert!(rendered.iter().any(|l| l.contains("… 2 more lines")));
        assert!(!rendered.iter().any(|l| l.ends_with("d")));
        assert!(rendered.iter().any(|l| l.ends_with("c")));
    }

    #[test]
    fn expanding_shows_everything_and_drops_the_count() {
        let lines = card_lines(&card("a\nb\nc\nd\ne", true));
        let rendered = text(&lines);
        assert!(rendered.iter().any(|l| l.ends_with('e')));
        assert!(!rendered.iter().any(|l| l.contains("more lines")));
    }

    #[test]
    fn short_output_gets_no_count_line() {
        let lines = card_lines(&card("only", false));
        assert!(!text(&lines).iter().any(|l| l.contains("more lines")));
    }

    #[test]
    fn wrapping_respects_the_width_and_keeps_every_word() {
        let lines = wrap("alpha beta gamma delta", 11);
        assert_eq!(lines, vec!["alpha beta", "gamma delta"]);
        assert!(wrap(&"x".repeat(25), 10).iter().all(|l| l.chars().count() <= 10));
    }

    #[test]
    fn a_long_argument_is_truncated_onto_one_line() {
        let args = json!({ "command": "x".repeat(400) });
        let rendered = key_args(&args);
        assert!(rendered[0].1.chars().count() <= 121);
        assert!(rendered[0].1.ends_with('…'));
    }
}
