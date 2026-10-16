use crate::theme;
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
    ("attach-output", "", "add the last !command output to your next message"),
    ("rewind", "", "undo the file changes and messages of recent turns (esc esc)"),
    ("cost", "", "show token usage and cost for this session"),
    ("context", "", "show what fills the context window"),
    ("limits", "[session|day <usd|off>]", "show or set the spend caps"),
    ("resume", "[id]", "resume an earlier session in this directory"),
    ("rename", "<title>", "rename this session"),
    ("review", "[focus]", "review the uncommitted changes"),
    ("commit", "", "draft a commit message from the staged diff and commit after you confirm"),
    ("pr", "", "draft a pull request title and body from this branch; nothing is sent anywhere"),
    ("loop", "<interval> <prompt> | list | stop <id>", "re-run a prompt on an interval in this session"),
    ("fork", "", "continue in a new session with this history; the original stays as it is"),
    ("tasks", "[cancel <id>]", "list background agents and their results"),
    ("plugins", "", "list, approve and remove plugins"),
    ("export", "[path]", "write the conversation to Markdown with keys redacted"),
    ("copy", "", "copy the last reply to the clipboard"),
    ("tools", "", "list the registered tools"),
    ("login", "[provider]", "add or replace a provider's API key"),
    ("logout", "[provider]", "remove a provider's API key"),
    ("provider", "[list|add|switch [name]]", "list, add or switch providers"),
    ("model", "[name]", "pick a model, or set one directly"),
    ("profile", "[name]", "use another profile for this session (ctrl+p)"),
    ("compare", "<a> <b> <prompt>", "send one prompt to two providers, answers side by side"),
    ("models", "[refresh]", "list each provider's models, rate limits and credit"),
    ("mode", "[name]", "pick a mode, or switch: default, plan, accept-edits, bypass, custom"),
    ("config", "", "open the settings panel"),
    ("skills", "", "list skills and turn them on or off"),
    ("mcp", "[list|add|remove|restart|login|logout]", "manage MCP servers"),
    ("agents", "[list|create|edit name]", "list, create and edit subagents"),
    ("permissions", "[edit]", "show rules, the mode overlay and what each tool resolves to"),
    ("init", "", "have the model inspect the repo and write ZARN.md"),
    ("hooks", "", "list the configured hooks"),
    ("doctor", "", "check config, keys, providers, MCP and the terminal"),
    ("status", "", "show profile, model, provider, cwd and session"),
    ("theme", "[dark|light|colorblind|auto|color]", "pick the theme or the accent color"),
    ("quit", "", "exit zarn"),
];

pub const SUBCOMMANDS: &[(&str, &str, &str, &str)] = &[
    ("provider", "list", "", "list providers and their keys"),
    ("provider", "add", "", "add a provider step by step"),
    ("provider", "switch", "[name]", "make another provider the default"),
    ("mcp", "list", "", "list MCP servers and their state"),
    ("mcp", "add", "<name> <command> [args]", "add an MCP server"),
    ("mcp", "remove", "<name>", "remove an MCP server"),
    ("mcp", "restart", "<name>", "restart an MCP server"),
    ("mcp", "login", "<name>", "sign in to an MCP server"),
    ("mcp", "logout", "<name>", "forget an MCP server sign-in"),
    ("agents", "list", "", "list subagents"),
    ("agents", "create", "", "create a subagent"),
    ("agents", "edit", "<name>", "edit a subagent"),
    ("limits", "session", "<usd|off>", "cap the spend of this session"),
    ("limits", "day", "<usd|off>", "cap the spend per day"),
    ("tasks", "cancel", "<id>", "cancel a background agent"),
    ("loop", "list", "", "list the running loops"),
    ("loop", "stop", "<id>", "stop a loop"),
    ("loop", "5m", "<prompt>", "every 5 minutes"),
    ("loop", "1h", "<prompt>", "every hour"),
    ("permissions", "edit", "", "edit the rules in the settings panel"),
    ("models", "refresh", "", "query every provider again"),
    ("login", "web", "", "set up API keys in your browser"),
    ("login", "auto", "", "paste any key; zarn works out the provider"),
    ("theme", "dark", "", "dark background"),
    ("theme", "light", "", "light background"),
    ("theme", "colorblind", "", "colorblind-safe colors"),
    ("theme", "auto", "", "follow the terminal"),
];

pub fn requires_arguments(usage: &str) -> bool {
    usage.starts_with('<') && !usage.contains(" | ")
}

pub fn argument_query(input: &str) -> Option<(String, &str)> {
    let rest = input.strip_prefix('/')?;
    let (command, args) = rest.split_once(' ')?;
    if command.is_empty() || args.contains('\n') {
        return None;
    }
    let mut words: Vec<&str> = args.split(' ').collect();
    let partial = words.pop().unwrap_or("");
    if words.iter().any(|w| w.is_empty()) || words.len() > 1 {
        return None;
    }
    let mut key = command.to_string();
    for word in words {
        key.push(' ');
        key.push_str(word);
    }
    Some((key, partial))
}

pub fn arguments(key: &str, dynamic: &std::collections::BTreeMap<String, Vec<(String, String)>>) -> Vec<Entry> {
    let mut out: Vec<Entry> = SUBCOMMANDS
        .iter()
        .filter(|(command, _, _, _)| *command == key)
        .map(|(_, name, usage, description)| Entry {
            name: (*name).to_string(),
            usage: (*usage).to_string(),
            description: (*description).to_string(),
            origin: Origin::Builtin,
        })
        .collect();
    for (name, description) in dynamic.get(key).into_iter().flatten() {
        if out.iter().any(|e| &e.name == name) {
            continue;
        }
        out.push(Entry {
            name: name.clone(),
            usage: String::new(),
            description: description.clone(),
            origin: Origin::Custom,
        });
    }
    out
}

pub fn usage_hint(
    input: &str,
    entries: &[Entry],
) -> Option<String> {
    let rest = input.strip_prefix('/')?;
    let (command, args) = rest.split_once(' ')?;
    if args.trim().is_empty() {
        let entry = entries.iter().find(|e| e.name == command)?;
        return (!entry.usage.is_empty()).then(|| entry.usage.clone());
    }
    let (sub, tail) = args.split_once(' ')?;
    if !tail.trim().is_empty() {
        return None;
    }
    SUBCOMMANDS
        .iter()
        .find(|(c, name, usage, _)| *c == command && *name == sub && !usage.is_empty())
        .map(|(_, _, usage, _)| (*usage).to_string())
}

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

pub fn entries(custom: &[(String, String, String)], skills: &[(String, String)]) -> Vec<Entry> {
    let mut all = builtin_entries();
    for (name, usage, description) in custom {
        if all.iter().any(|e| &e.name == name) {
            continue;
        }
        all.push(Entry {
            name: name.clone(),
            usage: if usage.is_empty() { "[args]".into() } else { usage.clone() },
            description: if description.is_empty() { "custom command".into() } else { description.clone() },
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
    let mut exact: Vec<&Entry> = Vec::new();
    let mut starts: Vec<&Entry> = Vec::new();
    let mut contains: Vec<&Entry> = Vec::new();
    for entry in entries {
        let name = entry.name.to_lowercase();
        if !needle.is_empty() && name == needle {
            exact.push(entry);
        } else if name.starts_with(&needle) {
            starts.push(entry);
        } else if !needle.is_empty()
            && (name.contains(&needle) || entry.description.to_lowercase().contains(&needle))
        {
            contains.push(entry);
        }
    }
    exact.extend(starts);
    exact.extend(contains);
    exact
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
    draw_with(frame, area, shown, selected, accent, "/");
}

pub fn draw_with(
    frame: &mut Frame,
    area: Rect,
    shown: &[&Entry],
    selected: usize,
    accent: Color,
    prefix: &str,
) {
    if shown.is_empty() || area.height < 3 {
        return;
    }
    let rows = shown.len().min(MAX_ROWS);
    let start = selected.saturating_sub(rows - 1);
    let height = (rows as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x,
        y: area.y + area.height - height,
        width: area.width.min(if prefix == "@" { 90 } else { 72 }),
        height,
    };
    let name_width = shown
        .iter()
        .map(|e| e.name.chars().count() + if e.usage.is_empty() || !prefix.is_empty() { 0 } else { e.usage.chars().count() + 1 })
        .max()
        .unwrap_or(0)
        + 1;
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
                Span::styled(
                    if prefix.is_empty() && !entry.usage.is_empty() {
                        format!("{:<name_width$}", format!("{} {}", entry.name, entry.usage))
                    } else {
                        format!("{prefix}{:<name_width$}", entry.name)
                    },
                    name_style,
                ),
                Span::styled(entry.description.clone(), Style::default().fg(theme::dim())),
            ])
        })
        .collect();
    for (k, i) in (start..shown.len()).take(rows).enumerate() {
        crate::hits::row(
            Rect {
                x: rect.x + 1,
                y: rect.y + 1,
                width: rect.width.saturating_sub(2),
                height: rect.height.saturating_sub(2),
            },
            rect.y + 1 + k as u16,
            crate::hits::Hit::Popup(i),
        );
    }
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::dim())),
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
        let names: Vec<&str> = filter(&all, "log").iter().map(|e| e.name.as_str()).collect();
        assert_eq!(&names[..2], &["login", "logout"]);
        let lo: Vec<&str> = filter(&all, "lo").iter().map(|e| e.name.as_str()).collect();
        assert!(lo.iter().take(3).all(|n| n.starts_with("lo")), "{lo:?}");
        assert!(filter(&all, "").len() == all.len());
    }

    #[test]
    fn custom_commands_and_skills_join_the_popup_without_shadowing_builtins() {
        let all = entries(
            &[
                ("deploy".into(), "<env>".into(), "deploy a build".into()),
                ("help".into(), String::new(), String::new()),
            ],
            &[("pdf".into(), "fill PDFs".into()), ("deploy".into(), "x".into())],
        );
        assert_eq!(all.iter().filter(|e| e.name == "help").count(), 1);
        let deploy = all.iter().find(|e| e.name == "deploy").unwrap();
        assert_eq!(deploy.origin, Origin::Custom);
        assert_eq!((deploy.usage.as_str(), deploy.description.as_str()), ("<env>", "deploy a build"));
        assert_eq!(
            all.iter().find(|e| e.name == "pdf").unwrap().origin,
            Origin::Skill
        );
    }

    #[test]
    fn an_exact_name_ranks_above_longer_names_that_share_its_prefix() {
        let all = entries(&[], &[]);
        let names: Vec<&str> = filter(&all, "mode").iter().map(|e| e.name.as_str()).collect();
        assert_eq!(&names[..2], &["mode", "model"]);
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
