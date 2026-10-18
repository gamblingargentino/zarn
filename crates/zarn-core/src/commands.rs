use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Clear,
    Compact,
    Cost,
    Model(Option<String>),
    Resume(Option<String>),
    Tools,
    Custom(Invocation),
    Unknown(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CustomCommand {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    pub allowed_tools: Vec<String>,
    pub model: Option<String>,
    pub body: String,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub command: CustomCommand,
    pub args: String,
}

impl Invocation {
    pub fn prompt(&self) -> String {
        substitute(&self.command.body, &self.args)
    }
}

pub const BUILTINS: &[(&str, &str)] = &[
    ("/help", "list the commands"),
    ("/clear", "drop the conversation and start over"),
    ("/compact", "summarize the conversation to free context"),
    ("/cost", "show token usage and cost for this session"),
    ("/model", "show the current model, or switch: /model <name>"),
    ("/resume", "resume a session: /resume [id]"),
    ("/tools", "list the registered tools"),
];

fn list(value: &str) -> Vec<String> {
    value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|t| t.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

pub fn parse_command(name: &str, text: &str, source: &str) -> CustomCommand {
    let mut command = CustomCommand {
        name: name.to_string(),
        source: source.to_string(),
        ..CustomCommand::default()
    };
    let normalized = text.replace("\r\n", "\n");
    let body = match normalized.strip_prefix("---\n").and_then(|rest| rest.split_once("\n---")) {
        Some((front, body)) => {
            for line in front.lines() {
                let Some((key, value)) = line.split_once(':') else { continue };
                let value = value.trim().trim_matches('"').trim_matches('\'');
                match key.trim() {
                    "description" => command.description = value.to_string(),
                    "argument-hint" | "argument_hint" => command.argument_hint = value.to_string(),
                    "allowed-tools" | "allowed_tools" => command.allowed_tools = list(value),
                    "model" => command.model = Some(value.to_string()).filter(|m| !m.is_empty()),
                    _ => {}
                }
            }
            body.trim_start_matches(|c| c != '\n').trim_start_matches('\n').to_string()
        }
        None => normalized,
    };
    command.body = body;
    command
}

pub fn split_args(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in args.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(current);
    }
    out
}

pub fn substitute(body: &str, args: &str) -> String {
    let words = split_args(args);
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some(d @ '1'..='9') => {
                chars.next();
                let i = d.to_digit(10).unwrap_or(1) as usize - 1;
                out.push_str(words.get(i).map(String::as_str).unwrap_or(""));
            }
            Some('A') => {
                let rest: String = chars.clone().take(9).collect();
                if rest == "ARGUMENTS" {
                    for _ in 0..9 {
                        chars.next();
                    }
                    out.push_str(args.trim());
                } else {
                    out.push('$');
                }
            }
            _ => out.push('$'),
        }
    }
    out
}

pub fn bang_commands(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find("!`") {
        let start = from + at;
        let inner = start + 2;
        let Some(len) = text[inner..].find('`') else { break };
        let command = text[inner..inner + len].trim().to_string();
        let end = inner + len + 1;
        if !command.is_empty() {
            found.push((start..end, command));
        }
        from = end;
    }
    found
}

#[derive(Clone, Debug, Default)]
pub struct Commands {
    custom: HashMap<String, CustomCommand>,
}

fn read_dir(dir: &Path, source: &str, prefix: &str) -> Vec<CustomCommand> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            if let Some(sub) = path.file_name().map(|s| s.to_string_lossy().to_string()) {
                out.extend(read_dir(&path, source, &format!("{prefix}{sub}:")));
            }
            continue;
        }
        if path.extension().is_none_or(|x| x != "md") {
            continue;
        }
        let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().to_string()) else {
            continue;
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => out.push(parse_command(&format!("{prefix}{stem}"), &text, source)),
            Err(err) => tracing::warn!(path = %path.display(), %err, "unreadable command"),
        }
    }
    out
}

impl Commands {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load(dir: &Path) -> Self {
        let mut commands = Commands::new();
        for command in read_dir(dir, "project", "") {
            commands.insert(command);
        }
        commands
    }

    pub fn load_all(user: Option<&Path>, project: &Path) -> Self {
        let mut commands = Commands::new();
        if let Some(user) = user {
            for command in read_dir(user, "user", "") {
                commands.insert(command);
            }
        }
        for command in read_dir(project, "project", "") {
            commands.insert(command);
        }
        commands
    }

    pub fn load_namespaced(&mut self, dir: &Path, namespace: &str) {
        for command in read_dir(dir, &format!("plugin {namespace}"), &format!("{namespace}:")) {
            self.insert(command);
        }
    }

    pub fn insert(&mut self, command: CustomCommand) {
        self.custom.insert(command.name.clone(), command);
    }

    pub fn custom(&self) -> Vec<&CustomCommand> {
        let mut all: Vec<&CustomCommand> = self.custom.values().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        all
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = BUILTINS.iter().map(|(n, _)| n.to_string()).collect();
        names.extend(self.custom.keys().map(|n| format!("/{n}")));
        names.sort();
        names
    }

    pub fn help(&self) -> String {
        let mut out: Vec<String> = BUILTINS
            .iter()
            .map(|(name, description)| format!("{name:<10} {description}"))
            .collect();
        for command in self.custom() {
            let description = if command.description.is_empty() { "custom command" } else { &command.description };
            out.push(format!("{:<10} {description}", format!("/{}", command.name)));
        }
        out.join("\n")
    }

    pub fn parse(&self, input: &str) -> Option<Command> {
        let trimmed = input.trim();
        let rest = trimmed.strip_prefix('/')?;
        if rest.is_empty() {
            return None;
        }

        let (name, args) = match rest.split_once(char::is_whitespace) {
            Some((name, args)) => (name, args.trim()),
            None => (rest, ""),
        };
        let optional = |args: &str| Some(args.to_string()).filter(|a| !a.is_empty());

        Some(match name {
            "help" => Command::Help,
            "clear" => Command::Clear,
            "compact" => Command::Compact,
            "cost" => Command::Cost,
            "tools" => Command::Tools,
            "model" => Command::Model(optional(args)),
            "resume" => Command::Resume(optional(args)),
            other => match self.custom.get(other) {
                Some(command) => Command::Custom(Invocation {
                    command: command.clone(),
                    args: args.to_string(),
                }),
                None => Command::Unknown(other.to_string()),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_custom(name: &str, body: &str) -> Commands {
        let mut commands = Commands::new();
        commands.insert(parse_command(name, body, "project"));
        commands
    }

    fn prompt(commands: &Commands, line: &str) -> String {
        match commands.parse(line) {
            Some(Command::Custom(invocation)) => invocation.prompt(),
            other => panic!("expected a custom command, got {other:?}"),
        }
    }

    #[test]
    fn plain_text_is_not_a_command() {
        assert_eq!(Commands::new().parse("hello there"), None);
        assert_eq!(Commands::new().parse("  "), None);
    }

    #[test]
    fn a_lone_slash_is_not_a_command() {
        assert_eq!(Commands::new().parse("/"), None);
    }

    #[test]
    fn builtins_parse_with_and_without_arguments() {
        let commands = Commands::new();
        assert_eq!(commands.parse("/help"), Some(Command::Help));
        assert_eq!(commands.parse("/model"), Some(Command::Model(None)));
        assert_eq!(
            commands.parse("/model gpt-4o"),
            Some(Command::Model(Some("gpt-4o".into())))
        );
        assert_eq!(
            commands.parse("/resume  abc123 "),
            Some(Command::Resume(Some("abc123".into())))
        );
    }

    #[test]
    fn a_custom_command_substitutes_its_arguments() {
        let commands = with_custom("review", "Review this diff: $ARGUMENTS\nBe brief.");
        assert_eq!(prompt(&commands, "/review HEAD~1"), "Review this diff: HEAD~1\nBe brief.");
    }

    #[test]
    fn a_custom_command_with_no_arguments_substitutes_an_empty_string() {
        let commands = with_custom("review", "Review: $ARGUMENTS.");
        assert_eq!(prompt(&commands, "/review"), "Review: .");
    }

    #[test]
    fn an_unknown_command_is_reported_rather_than_sent_to_the_model() {
        assert_eq!(
            Commands::new().parse("/nope"),
            Some(Command::Unknown("nope".into()))
        );
    }

    #[test]
    fn a_custom_command_cannot_shadow_a_builtin() {
        let commands = with_custom("help", "not help");
        assert_eq!(commands.parse("/help"), Some(Command::Help));
    }

    #[test]
    fn frontmatter_fields_are_read() {
        let c = parse_command(
            "fix",
            "---\ndescription: fix an issue\nargument-hint: <issue> [branch]\nallowed-tools: bash(git status:*), read\nmodel: small\n---\nFix $1 on $2.\n",
            "user",
        );
        assert_eq!(c.description, "fix an issue");
        assert_eq!(c.argument_hint, "<issue> [branch]");
        assert_eq!(c.allowed_tools, vec!["bash(git status:*)", "read"]);
        assert_eq!(c.model.as_deref(), Some("small"));
        assert_eq!(c.body, "Fix $1 on $2.\n");
        assert_eq!(c.source, "user");
    }

    #[test]
    fn positional_arguments_respect_quotes_and_missing_ones_are_empty() {
        assert_eq!(substitute("[$1] [$2] [$3] [$ARGUMENTS] $HOME $", "one \"two words\""), "[one] [two words] [] [one \"two words\"] $HOME $");
        assert_eq!(split_args("a 'b c' \"\" d"), vec!["a", "b c", "", "d"]);
    }

    #[test]
    fn bang_commands_are_found_with_their_ranges() {
        let text = "Status:\n!`git status --short`\nDiff: !`git diff`, and `plain code`.";
        let found = bang_commands(text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].1, "git status --short");
        assert_eq!(&text[found[1].0.clone()], "!`git diff`");
    }

    #[test]
    fn project_commands_override_user_ones_and_subdirectories_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user");
        let project = dir.path().join("project");
        std::fs::create_dir_all(user.join("git")).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(user.join("hello.md"), "user hello").unwrap();
        std::fs::write(user.join("git/log.md"), "show the log").unwrap();
        std::fs::write(project.join("hello.md"), "project hello").unwrap();
        let commands = Commands::load_all(Some(&user), &project);
        assert_eq!(prompt(&commands, "/hello"), "project hello");
        assert_eq!(prompt(&commands, "/git:log"), "show the log");
    }
}
