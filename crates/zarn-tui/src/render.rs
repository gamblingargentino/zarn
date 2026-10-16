use crate::brand;
use crate::markdown;
use crate::state::{App, Block, CardStatus, COLLAPSED_LINES, ToolCard};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as Border, BorderType, Borders, Paragraph, Wrap};
use serde_json::Value;

const MAX_INPUT_ROWS: u16 = 8;

pub fn draw(frame: &mut Frame, app: &App) {
    let input_rows = (app.input.split('\n').count() as u16 + 2).clamp(3, MAX_INPUT_ROWS);
    let areas = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(input_rows),
    ])
    .split(frame.area());

    scrollback(frame, app, areas[0]);
    status(frame, app, areas[1]);
    input(frame, app, areas[2]);
}

fn scrollback(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines = brand::header(&app.status.model);
    lines.push(Line::raw(""));
    for block in &app.blocks {
        lines.extend(block_lines(block));
        lines.push(Line::raw(""));
    }
    if let Some(request) = &app.pending {
        lines.extend(permission_lines(&request.tool, &request.args));
    }

    let total = lines.len() as u16;
    let bottom = total.saturating_sub(area.height);
    let offset = bottom.saturating_sub(app.scroll_up);

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((offset, 0)),
        area,
    );
}

fn block_lines(block: &Block) -> Vec<Line<'static>> {
    match block {
        Block::User(text) => text
            .split('\n')
            .map(|l| {
                Line::from(vec![
                    Span::styled("› ", Style::default().fg(Color::Blue)),
                    Span::styled(
                        l.to_string(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ])
            })
            .collect(),
        Block::Assistant(text) => markdown::render(text),
        Block::Notice(text) => vec![Line::styled(
            text.clone(),
            Style::default().fg(Color::DarkGray),
        )],
        Block::Tool(card) => card_lines(card),
    }
}

fn status_style(status: CardStatus) -> Style {
    let color = match status {
        CardStatus::Pending => Color::DarkGray,
        CardStatus::Running => Color::Yellow,
        CardStatus::Done => Color::Green,
        CardStatus::Failed => Color::Red,
    };
    Style::default().fg(color)
}

fn card_lines(card: &ToolCard) -> Vec<Line<'static>> {
    let edge = Style::default().fg(Color::DarkGray);
    let mut lines = vec![Line::from(vec![
        Span::styled("┌ ", edge),
        Span::styled(
            card.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", edge),
        Span::styled(card.status.label().to_string(), status_style(card.status)),
    ])];

    for (key, value) in key_args(&card.args) {
        lines.push(Line::from(vec![
            Span::styled("│ ", edge),
            Span::styled(format!("{key}: "), Style::default().fg(Color::DarkGray)),
            Span::raw(value),
        ]));
    }

    if let Some(output) = &card.output {
        let all: Vec<&str> = output.lines().collect();
        let shown = if card.expanded {
            all.len()
        } else {
            all.len().min(COLLAPSED_LINES)
        };
        for line in &all[..shown] {
            lines.push(Line::from(vec![
                Span::styled("│ ", edge),
                Span::raw(line.to_string()),
            ]));
        }
        if all.len() > shown {
            lines.push(Line::from(vec![
                Span::styled("│ ", edge),
                Span::styled(
                    format!("… {} more lines (ctrl+r)", all.len() - shown),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
    }

    lines.push(Line::styled("└", edge));
    lines
}

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

fn permission_lines(tool: &str, args: &Value) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!("{tool} "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("wants to run", Style::default().fg(Color::Yellow)),
    ])];
    for (key, value) in key_args(args) {
        lines.push(Line::raw(format!("  {key}: {value}")));
    }
    lines.push(Line::styled(
        "  [y] once   [a] always   [n] no",
        Style::default().fg(Color::Yellow),
    ));
    lines
}

fn status(frame: &mut Frame, app: &App, area: Rect) {
    let mut parts = vec![
        app.status.model.clone(),
        format!(
            "{} in / {} out",
            compact(app.status.input_tokens),
            compact(app.status.output_tokens)
        ),
        format!("${:.4}", app.status.cost),
    ];
    if app.status.cached_tokens > 0 {
        parts.insert(2, format!("{} cached", compact(app.status.cached_tokens)));
    }
    if app.streaming {
        parts.push("esc to cancel".to_string());
    }
    if app.scroll_up > 0 {
        parts.push(format!("scrolled {}", app.scroll_up));
    }

    let mut spans = Vec::new();
    if app.streaming {
        spans.push(brand::frame(app.tick));
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled(
        parts.join("  ·  "),
        Style::default().fg(Color::DarkGray),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
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
            Color::Yellow
        } else {
            Color::DarkGray
        }));
    let inner = border.inner(area);

    frame.render_widget(border, area);
    frame.render_widget(Paragraph::new(app.input.clone()), inner);

    if app.pending.is_none() {
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
    fn a_long_argument_is_truncated_onto_one_line() {
        let args = json!({ "command": "x".repeat(400) });
        let rendered = key_args(&args);
        assert!(rendered[0].1.chars().count() <= 121);
        assert!(rendered[0].1.ends_with('…'));
    }
}
