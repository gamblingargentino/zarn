use clap::{Args as ClapArgs, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "zarn", version)]
#[command(about = "Interactive AI agent CLI", long_about = None)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[arg(long, value_name = "PROMPT", help = "Run one turn and print to stdout")]
    pub print: Option<String>,

    #[arg(long = "continue", help = "Resume the most recent session for this directory")]
    pub continue_session: bool,

    #[arg(long, value_name = "ID", help = "Resume a specific session by id")]
    pub resume: Option<String>,

    #[arg(long, value_name = "MODEL", help = "Override the profile's model")]
    pub model: Option<String>,

    #[arg(long, value_name = "PROFILE", help = "Use this profile instead of the default")]
    pub profile: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    #[command(about = "Create or inspect the configuration")]
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    #[command(about = "Store an API key for a provider in the OS credential store")]
    Login {
        #[arg(help = "Provider name, e.g. anthropic, openai, google, or one you added")]
        provider: String,
    },
    #[command(about = "Remove a provider's stored API key")]
    Logout {
        #[arg(help = "Provider name")]
        provider: String,
    },
    #[command(about = "List, add, test and remove providers")]
    Providers {
        #[command(subcommand)]
        action: Option<ProvidersAction>,
    },
    #[command(about = "List skills and turn them on or off")]
    Skills {
        #[command(subcommand)]
        action: Option<SkillsAction>,
    },
    #[command(about = "Manage MCP servers")]
    Mcp {
        #[command(subcommand)]
        action: Option<McpAction>,
    },
    #[command(about = "Check config, keys, provider reachability, MCP servers and the terminal")]
    Doctor {
        #[arg(long, help = "Skip network probes and MCP server starts")]
        offline: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    #[command(about = "Write a starter config")]
    Init,
    #[command(about = "Validate the config and show the resolved values")]
    Check,
}

#[derive(ClapArgs, Debug, Default)]
pub struct AddProvider {
    #[arg(long, help = "anthropic, openai, google, openai-compat or ollama")]
    pub kind: Option<String>,
    #[arg(long, help = "Name for this provider; defaults to the kind")]
    pub name: Option<String>,
    #[arg(long = "base-url", help = "API endpoint, for openai-compat and ollama")]
    pub base_url: Option<String>,
    #[arg(long, help = "Model to use if this becomes the default provider")]
    pub model: Option<String>,
    #[arg(long = "api-key-env", help = "Environment variable that overrides the stored key")]
    pub api_key_env: Option<String>,
    #[arg(long = "no-key", help = "Do not prompt for an API key")]
    pub no_key: bool,
    #[arg(long = "no-test", help = "Skip the test request after adding")]
    pub no_test: bool,
}

#[derive(Subcommand, Debug)]
pub enum ProvidersAction {
    #[command(about = "List providers, their keys and whether they are reachable")]
    List,
    #[command(about = "Add a provider; prompts for anything not given as a flag")]
    Add(AddProvider),
    #[command(about = "Send a minimal real request and report OK or the exact failure")]
    Test {
        #[arg(help = "Provider name")]
        name: String,
    },
    #[command(about = "Remove a provider and its stored key")]
    Remove {
        #[arg(help = "Provider name")]
        name: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum SkillsAction {
    #[command(about = "List skills with their description, source and state")]
    List,
    #[command(about = "Turn a skill on")]
    Enable {
        #[arg(help = "Skill name")]
        name: String,
    },
    #[command(about = "Turn a skill off")]
    Disable {
        #[arg(help = "Skill name")]
        name: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum McpAction {
    #[command(about = "List MCP servers and whether they start")]
    List {
        #[arg(long, help = "Do not start the servers to check them")]
        offline: bool,
    },
    #[command(about = "Add a server: zarn mcp add <name> -- <command> [args...], or a URL")]
    Add {
        #[arg(help = "Server name")]
        name: String,
        #[arg(long, help = "Save to ./.zarn/config.toml instead of the user config")]
        project: bool,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true, help = "Command and arguments, or an http(s) URL")]
        target: Vec<String>,
    },
    #[command(about = "Remove a server")]
    Remove {
        #[arg(help = "Server name")]
        name: String,
    },
}

impl Args {
    pub fn run_mode(&self) -> RunMode {
        if let Some(prompt) = &self.print {
            RunMode::OneShot(prompt.clone())
        } else if self.continue_session {
            RunMode::ContinueLast
        } else if let Some(id) = &self.resume {
            RunMode::ResumeSession(id.clone())
        } else {
            RunMode::Interactive
        }
    }
}

#[derive(Debug)]
pub enum RunMode {
    Interactive,
    OneShot(String),
    ContinueLast,
    ResumeSession(String),
}
