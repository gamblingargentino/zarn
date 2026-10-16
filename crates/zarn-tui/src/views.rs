use crate::brand;
use crate::onboarding::{Onboarding, Step};
use crate::settings::{Editor, Form, Panel, TABS};
use crate::widgets::{MaskedInput, Picker, TextField};
use crate::theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

const PICKER_ROWS: usize = 10;

fn dim() -> Style {
    Style::default().fg(theme::dim())
}

fn frame_block(title: &str, accent: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ))
}

pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(10.min(area.width));
    let height = height.min(area.height.saturating_sub(2)).max(3.min(area.height));
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let keep: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{keep}…")
}

pub fn draw_panel(frame: &mut Frame, panel: &Panel, accent: Color) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    let block = frame_block("zarn settings · changes save as you go", accent);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let parts = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);

    let mut tabs = Vec::new();
    for (i, tab) in TABS.iter().enumerate() {
        if i > 0 {
            tabs.push(Span::styled("  ", dim()));
        }
        let style = if *tab == panel.tab {
            Style::default()
                .fg(accent)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default()
        };
        tabs.push(Span::styled(tab.label(), style));
    }
    frame.render_widget(Paragraph::new(Line::from(tabs)), parts[0]);

    let rows = panel.rows();
    let selected = panel.selected();
    let body = parts[2];
    let height = body.height as usize;
    let offset = selected.saturating_sub(height.saturating_sub(1));
    let label_width = rows
        .iter()
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(0)
        .min(32)
        + 2;
    let width = body.width as usize;
    let lines: Vec<Line> = if rows.is_empty() {
        vec![Line::styled(
            match panel.tab {
                crate::settings::Tab::Skills => {
                    "no skills found in ~/.config/zarn/skills or ./.zarn/skills"
                }
                crate::settings::Tab::Hooks => "no hooks; add them under [hooks] in config.toml",
                crate::settings::Tab::Quota => "no provider with a key to query",
                _ => "nothing here yet",
            },
            dim(),
        )]
    } else {
        rows.iter()
            .enumerate()
            .skip(offset)
            .take(height)
            .map(|(i, row)| {
                let chosen = i == selected;
                let marker = if chosen { "› " } else { "  " };
                let label_style = if chosen {
                    Style::default().fg(accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let label = format!("{:<label_width$}", clip(&row.label, label_width - 2));
                let used = 2 + label_width;
                let value = clip(&row.value, width.saturating_sub(used).max(1));
                let rest = width.saturating_sub(used + value.chars().count() + 2);
                let mut spans = vec![
                    Span::styled(marker, Style::default().fg(accent)),
                    Span::styled(label, label_style),
                    Span::raw(value),
                ];
                if !row.note.is_empty() && rest > 3 {
                    let style = if row.note.starts_with("FAILED") {
                        Style::default().fg(theme::error())
                    } else if row.note.starts_with("OK") {
                        Style::default().fg(theme::ok())
                    } else {
                        dim()
                    };
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(clip(&row.note, rest), style));
                }
                Line::from(spans)
            })
            .collect()
    };
    frame.render_widget(Paragraph::new(lines), body);

    if let Some((text, error)) = &panel.message {
        let style = if *error {
            Style::default().fg(theme::error())
        } else {
            Style::default().fg(theme::ok())
        };
        frame.render_widget(Paragraph::new(Line::styled(clip(text, width), style)), parts[3]);
    }
    frame.render_widget(
        Paragraph::new(Line::styled(
            clip(
                &format!(
                    "↑↓ move · ←→ tab · {} · esc close",
                    panel.tab.hints()
                ),
                width,
            ),
            dim(),
        )),
        parts[4],
    );

    if let Some(editor) = &panel.editor {
        draw_editor(frame, editor, accent, panel.store().backend_label());
    }
}

fn draw_editor(frame: &mut Frame, editor: &Editor, accent: Color, store_label: String) {
    let area = frame.area();
    match editor {
        Editor::Pick(picker, _) => draw_picker(frame, area, picker, accent),
        Editor::Form(form, _) => draw_form(frame, area, form, accent),
        Editor::Secret {
            input,
            provider,
            optional,
        } => {
            let mut hint = format!(
                "Paste the key and press enter. It goes to the {store_label}, never to a config file."
            );
            if *optional {
                hint.push_str(" Leave it empty if this endpoint needs no key.");
            }
            draw_secret(frame, area, &format!("API key for {provider}"), input, &hint, accent);
        }
        Editor::Confirm(text, _) => {
            let rect = centered(area, (text.chars().count() as u16 + 6).max(30), 3);
            frame.render_widget(Clear, rect);
            frame.render_widget(
                Paragraph::new(Line::raw(text.clone())).block(frame_block("confirm", accent)),
                rect,
            );
        }
    }
}

pub fn draw_secret(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    input: &MaskedInput,
    hint: &str,
    accent: Color,
) {
    let rect = centered(area, 72, 7);
    frame.render_widget(Clear, rect);
    let block = frame_block(title, accent);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let masked = input.masked();
    let shown = clip(&masked, inner.width.saturating_sub(6) as usize);
    let lines = vec![
        Line::from(vec![
            Span::styled("key ", dim()),
            Span::styled(shown.clone(), Style::default().fg(accent)),
        ]),
        Line::raw(""),
        Line::styled(hint.to_string(), dim()),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    frame.set_cursor_position(Position::new(
        inner.x + 4 + (shown.chars().count() as u16).min(inner.width.saturating_sub(5)),
        inner.y,
    ));
}

pub fn draw_picker(frame: &mut Frame, area: Rect, picker: &Picker, accent: Color) {
    let visible = picker.visible();
    let rows = visible.len().clamp(1, PICKER_ROWS);
    let hint_rows = u16::from(!picker.hint.is_empty());
    let rect = centered(area, 64, rows as u16 + 4 + hint_rows);
    frame.render_widget(Clear, rect);
    let block = frame_block(&picker.title, accent);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let mut lines = vec![Line::from(vec![
        Span::styled("filter ", dim()),
        Span::raw(picker.filter.value.clone()),
    ])];
    if !picker.hint.is_empty() {
        lines.push(Line::styled(picker.hint.clone(), dim()));
    }
    let start = picker.selected.saturating_sub(rows - 1);
    if visible.is_empty() {
        lines.push(Line::styled(
            if picker.allow_custom {
                "no match; enter uses what you typed"
            } else {
                "no match"
            },
            dim(),
        ));
    }
    let width = inner.width as usize;
    for (i, choice) in visible.iter().enumerate().skip(start).take(rows) {
        let chosen = i == picker.selected;
        let style = if chosen {
            Style::default().fg(accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let mut spans = vec![
            Span::styled(if chosen { "› " } else { "  " }, Style::default().fg(accent)),
            Span::styled(clip(&choice.label, width.saturating_sub(2)), style),
        ];
        if !choice.detail.is_empty() {
            spans.push(Span::styled(format!("  {}", choice.detail), dim()));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), inner);
    frame.set_cursor_position(Position::new(
        inner.x + 7 + field_col(&picker.filter).min(inner.width.saturating_sub(8)),
        inner.y,
    ));
}

fn field_col(field: &TextField) -> u16 {
    let before: String = field.value.chars().take(field.cursor).collect();
    before.split('\n').next_back().unwrap_or("").chars().count() as u16
}

pub fn draw_form(frame: &mut Frame, area: Rect, form: &Form, accent: Color) {
    let label_width = form
        .fields
        .iter()
        .map(|f| f.label.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let field_rows: u16 = form
        .fields
        .iter()
        .map(|f| f.input.value.split('\n').count().max(1) as u16)
        .sum();
    let extra = if form.preview.is_some() { 2 } else { 0 };
    let rect = centered(area, 76, field_rows + 5 + extra);
    frame.render_widget(Clear, rect);
    let block = frame_block(&form.title, accent);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let mut lines = Vec::new();
    let mut cursor = None;
    for (i, field) in form.fields.iter().enumerate() {
        let focused = i == form.focus;
        let style = if focused {
            Style::default().fg(accent).add_modifier(Modifier::BOLD)
        } else {
            dim()
        };
        for (j, part) in field.input.value.split('\n').enumerate() {
            let label = if j == 0 { field.label.as_str() } else { "" };
            lines.push(Line::from(vec![
                Span::styled(format!("{label:<label_width$}"), style),
                Span::raw(part.to_string()),
            ]));
        }
        if focused {
            let before: String = field.input.value.chars().take(field.input.cursor).collect();
            let row = before.matches('\n').count();
            cursor = Some((lines.len() - field.input.value.split('\n').count() + row, field_col(&field.input)));
        }
    }
    if let Some(values) = &form.preview {
        let source = form.fields.first().map(|f| f.input.value.as_str()).unwrap_or("");
        lines.push(Line::raw(""));
        lines.push(match crate::status_line::preview(source, values) {
            Ok(text) => Line::from(vec![Span::styled("preview  ", dim()), Span::raw(text)]),
            Err(err) => Line::from(vec![
                Span::styled("preview  ", dim()),
                Span::styled(err, Style::default().fg(theme::error())),
            ]),
        });
    }
    lines.push(Line::raw(""));
    let hint = form
        .fields
        .get(form.focus)
        .map(|f| f.hint.clone())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "↑↓ move between fields · enter next / save · esc cancel".to_string());
    lines.push(Line::styled(hint, dim()));
    frame.render_widget(Paragraph::new(lines), inner);
    if let Some((row, col)) = cursor {
        frame.set_cursor_position(Position::new(
            inner.x + label_width as u16 + col.min(inner.width.saturating_sub(label_width as u16 + 1)),
            inner.y + (row as u16).min(inner.height.saturating_sub(1)),
        ));
    }
}

pub fn draw_onboarding(frame: &mut Frame, onboarding: &Onboarding, accent: Color) {
    let area = frame.area();
    frame.render_widget(Clear, area);
    let block = frame_block("welcome to zarn", accent);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let parts = Layout::vertical([
        Constraint::Length(brand::MARK.len() as u16 + 1),
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(inner);

    let mark: Vec<Line> = brand::MARK
        .iter()
        .map(|row| {
            Line::styled(
                (*row).to_string(),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(mark), parts[0]);

    let mut intro = vec![Line::raw(
        "No model provider is configured yet. Connect one to start; your API key stays on this machine.",
    )];
    let progress = match onboarding.kind {
        Some(kind) => format!("{} · {}", kind.label, onboarding.step.title()),
        None => onboarding.step.title().to_string(),
    };
    intro.push(Line::styled(
        progress,
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(Paragraph::new(intro).wrap(Wrap { trim: false }), parts[1]);

    let body = parts[2];
    let hint = match &onboarding.step {
        Step::Kind(picker) => {
            draw_picker(frame, body, picker, accent);
            "↑↓ choose · enter select".to_string()
        }
        Step::Model(picker) => {
            draw_picker(frame, body, picker, accent);
            "↑↓ choose · type to filter or name any model · enter test · esc back".to_string()
        }
        Step::BaseUrl(field) => {
            let rect = centered(body, 72, 5);
            frame.render_widget(Clear, rect);
            let b = frame_block("endpoint URL", accent);
            let i = b.inner(rect);
            frame.render_widget(b, rect);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::raw(field.value.clone()),
                    Line::styled("e.g. https://api.groq.com/openai/v1 or http://localhost:11434", dim()),
                ]),
                i,
            );
            frame.set_cursor_position(Position::new(
                i.x + field_col(field).min(i.width.saturating_sub(1)),
                i.y,
            ));
            "enter continue · esc back".to_string()
        }
        Step::Key(input) => {
            let optional = onboarding.kind.is_some_and(|k| !k.needs_key);
            let mut hint = format!(
                "Characters are not shown. The key is saved to the {} only after a test request succeeds.",
                onboarding.store_label()
            );
            if optional {
                hint.push_str(" Leave it empty if the endpoint needs no key.");
            }
            draw_secret(frame, body, "paste your API key", input, &hint, accent);
            "enter continue · esc back".to_string()
        }
        Step::Testing => {
            frame.render_widget(
                Paragraph::new(Line::styled(
                    format!("sending a test request to {}…", onboarding.model),
                    Style::default().fg(theme::warn()),
                )),
                body,
            );
            "waiting for the provider".to_string()
        }
        Step::Failed(err) => {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled("The test request failed:", Style::default().fg(theme::error())),
                    Line::raw(err.clone()),
                ])
                .wrap(Wrap { trim: false }),
                body,
            );
            onboarding.failure_hints()
        }
        Step::Saved(text) => {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled("It works.", Style::default().fg(theme::ok()).add_modifier(Modifier::BOLD)),
                    Line::raw(text.clone()),
                    Line::raw(""),
                    Line::raw("Change anything later with /config, /login, /model or /provider."),
                ])
                .wrap(Wrap { trim: false }),
                body,
            );
            "enter start".to_string()
        }
    };
    let line = match &onboarding.message {
        Some(message) => Line::styled(message.clone(), Style::default().fg(theme::warn())),
        None => Line::styled(hint, dim()),
    };
    frame.render_widget(Paragraph::new(line), parts[3]);
}

pub fn draw_confirm(frame: &mut Frame, area: Rect, text: &str, accent: Color) {
    let wrapped: usize = text
        .lines()
        .map(|l| l.chars().count().div_ceil(68).max(1))
        .sum();
    let rect = centered(area, 72, wrapped as u16 + 5);
    frame.render_widget(Clear, rect);
    let block = frame_block("confirm", theme::warn());
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let mut lines: Vec<Line> = text
        .lines()
        .map(|l| Line::styled(l.to_string(), Style::default().fg(theme::warn())))
        .collect();
    lines.extend([
        Line::raw(""),
        Line::from(vec![
            Span::styled("[y] ", Style::default().fg(accent).add_modifier(Modifier::BOLD)),
            Span::raw("yes   "),
            Span::styled("[n] ", Style::default().fg(accent).add_modifier(Modifier::BOLD)),
            Span::raw("no"),
        ]),
    ]);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

pub fn draw_plan(frame: &mut Frame, area: Rect, review: &crate::state::PlanReview, accent: Color) {
    let body = crate::markdown::render(&review.plan);
    let height = (body.len() as u16 + 6).min(area.height.saturating_sub(1)).max(8);
    let rect = centered(area, 90, height);
    frame.render_widget(Clear, rect);
    let block = frame_block("plan for approval", accent);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(inner);
    frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), rows[0]);
    let footer = match &review.feedback {
        Some(field) => vec![
            Line::from(vec![
                Span::styled("feedback ", dim()),
                Span::raw(field.value.clone()),
            ]),
            Line::styled("enter sends it and keeps plan mode · esc goes back", dim()),
        ],
        None => vec![
            Line::from(vec![
                Span::styled("[a] ", Style::default().fg(accent).add_modifier(Modifier::BOLD)),
                Span::raw("approve, accept edits   "),
                Span::styled("[d] ", Style::default().fg(accent).add_modifier(Modifier::BOLD)),
                Span::raw("approve, ask as usual   "),
                Span::styled("[r] ", Style::default().fg(accent).add_modifier(Modifier::BOLD)),
                Span::raw("reject with feedback"),
            ]),
            Line::styled("esc rejects without feedback", dim()),
        ],
    };
    frame.render_widget(Paragraph::new(footer), rows[1]);
}
