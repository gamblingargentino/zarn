use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

pub const MAX_ROWS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Builtin,
    Custom,
    Skill,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub usage: String,
    pub description: String,
    pub origin: Origin,
}

pub const BUILTINS: &[(&str, &str, &str)] = &[
    ("help", "", "list the commands"),
    ("clear", "", "drop the conversation and start over"),
    ("compact", "", "summarize the conversation to free context"),
    ("cost", "", "show token usage and cost for this session"),
    ("resume", "[id]", "resume an earlier session in this directory"),
    ("tools", "", "list the registered tools"),
    ("login", "[provider]", "add or replace a provider's API key"),
    ("logout", "[provider]", "remove a provider's API key"),
    ("provider", "[list|add|switch [name]]", "list, add or switch providers"),
    ("model", "[name]", "pick a model, or set one directly"),
    ("config", "", "open the settings panel"),
    ("skills", "", "list skills and turn them on or off"),
    ("mcp", "[list|add|remove|restart]", "manage MCP servers"),
    ("agents", "[list|create|edit name]", "list, create and edit subagents"),
    ("permissions", "", "view and edit tool permissions"),
    ("init", "", "have the model inspect the repo and write ZARN.md"),
    ("doctor", "", "check config, keys, providers, MCP and the terminal"),
    ("status", "", "show profile, model, provider, cwd and session"),
    ("theme", "[color]", "pick the accent color"),
    ("quit", "", "exit zarn"),
];

pub fn builtin_entries() -> Vec<Entry> {
    BUILTINS
        .iter()
        .map(|(name, usage, description)| Entry {
            name: (*name).to_string(),
            usage: (*usage).to_string(),
            description: (*description).to_string(),
            origin: Origin::Builtin,
        })
        .collect()
}

pub fn is_builtin(name: &str) -> bool {
    BUILTINS.iter().any(|(n, _, _)| *n == name)
}

pub fn entries(custom: &[String], skills: &[(String, String)]) -> Vec<Entry> {
    let mut all = builtin_entries();
    for name in custom {
        if all.iter().any(|e| &e.name == name) {
            continue;
        }
        all.push(Entry {
            name: name.clone(),
            usage: "[args]".into(),
            description: "custom command".into(),
            origin: Origin::Custom,
        });
    }
    for (name, description) in skills {
        if all.iter().any(|e| &e.name == name) {
            continue;
        }
        all.push(Entry {
            name: name.clone(),
            usage: "[args]".into(),
            description: format!("skill: {description}"),
            origin: Origin::Skill,
        });
    }
    all
}

pub fn query(input: &str) -> Option<&str> {
    let rest = input.strip_prefix('/')?;
    if rest.contains(char::is_whitespace) {
        return None;
    }
    Some(rest)
}

pub fn filter<'a>(entries: &'a [Entry], query: &str) -> Vec<&'a Entry> {
    let needle = query.to_lowercase();
    let mut starts: Vec<&Entry> = Vec::new();
    let mut contains: Vec<&Entry> = Vec::new();
    for entry in entries {
        let name = entry.name.to_lowercase();
        if name.starts_with(&needle) {
            starts.push(entry);
        } else if !needle.is_empty()
            && (name.contains(&needle) || entry.description.to_lowercase().contains(&needle))
        {
            contains.push(entry);
        }
    }
    starts.extend(contains);
    starts
}

pub fn split(line: &str) -> Option<(String, String)> {
    let rest = line.trim().strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }
    Some(match rest.split_once(char::is_whitespace) {
        Some((name, args)) => (name.to_string(), args.trim().to_string()),
        None => (rest.to_string(), String::new()),
    })
}

pub fn draw(frame: &mut Frame, area: Rect, shown: &[&Entry], selected: usize, accent: Color) {
    if shown.is_empty() || area.height < 3 {
        return;
    }
    let rows = shown.len().min(MAX_ROWS);
    let start = selected.saturating_sub(rows - 1);
    let height = (rows as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x,
        y: area.y + area.height - height,
        width: area.width.min(72),
        height,
    };
    let name_width = shown.iter().map(|e| e.name.len()).max().unwrap_or(0) + 1;
    let lines: Vec<Line> = shown
        .iter()
        .enumerate()
        .skip(start)
        .take(rows)
        .map(|(i, entry)| {
            let chosen = i == selected;
            let marker = if chosen { "› " } else { "  " };
            let name_style = if chosen {
                Style::default().fg(accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(marker, Style::default().fg(accent)),
                Span::styled(format!("/{:<name_width$}", entry.name), name_style),
                Span::styled(entry.description.clone(), Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::DarkGray)),
        ),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_narrows_the_list_with_prefix_matches_first() {
        let all = entries(&[], &[]);
        let names: Vec<&str> = filter(&all, "lo").iter().map(|e| e.name.as_str()).collect();
        assert_eq!(&names[..2], &["login", "logout"]);
        assert!(filter(&all, "").len() == all.len());
    }

    #[test]
    fn custom_commands_and_skills_join_the_popup_without_shadowing_builtins() {
        let all = entries(
            &["review".into(), "help".into()],
            &[("pdf".into(), "fill PDFs".into()), ("review".into(), "x".into())],
        );
        assert_eq!(all.iter().filter(|e| e.name == "help").count(), 1);
        assert_eq!(
            all.iter().find(|e| e.name == "review").unwrap().origin,
            Origin::Custom
        );
        assert_eq!(
            all.iter().find(|e| e.name == "pdf").unwrap().origin,
            Origin::Skill
        );
    }

    #[test]
    fn the_popup_only_applies_to_the_command_word() {
        assert_eq!(query("/mo"), Some("mo"));
        assert_eq!(query("/model gpt"), None);
        assert_eq!(query("hello"), None);
    }

    #[test]
    fn a_line_splits_into_name_and_arguments() {
        assert_eq!(
            split("/mcp add fs npx srv"),
            Some(("mcp".into(), "add fs npx srv".into()))
        );
        assert_eq!(split("/"), None);
    }
}
