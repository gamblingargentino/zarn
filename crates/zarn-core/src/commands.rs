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
    Prompt(String),
    Unknown(String),
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

#[derive(Clone, Debug, Default)]
pub struct Commands {
    custom: HashMap<String, String>,
}

impl Commands {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load(dir: &Path) -> Self {
        let mut custom = HashMap::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Commands { custom };
        };

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().is_none_or(|x| x != "md") {
                continue;
            }
            let Some(name) = path.file_stem().map(|s| s.to_string_lossy().to_string()) else {
                continue;
            };
            match std::fs::read_to_string(&path) {
                Ok(body) => {
                    custom.insert(name, body);
                }
                Err(err) => tracing::warn!(path = %path.display(), %err, "unreadable command"),
            }
        }

        Commands { custom }
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
        let mut names: Vec<&String> = self.custom.keys().collect();
        names.sort();
        for name in names {
            out.push(format!("{:<10} custom command", format!("/{name}")));
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
                Some(body) => Command::Prompt(body.replace("$ARGUMENTS", args)),
                None => Command::Unknown(other.to_string()),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_custom(name: &str, body: &str) -> Commands {
        Commands {
            custom: HashMap::from([(name.to_string(), body.to_string())]),
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
        assert_eq!(
            commands.parse("/review HEAD~1"),
            Some(Command::Prompt(
                "Review this diff: HEAD~1\nBe brief.".into()
            ))
        );
    }

    #[test]
    fn a_custom_command_with_no_arguments_substitutes_an_empty_string() {
        let commands = with_custom("review", "Review: $ARGUMENTS.");
        assert_eq!(
            commands.parse("/review"),
            Some(Command::Prompt("Review: .".into()))
        );
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
}
