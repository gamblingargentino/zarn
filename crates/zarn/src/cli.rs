use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    StreamJson,
}

#[derive(Parser, Debug)]
#[command(name = "zarn", version)]
#[command(about = "Interactive AI agent CLI", long_about = None)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[arg(long, value_name = "PROMPT", num_args = 0..=1, default_missing_value = "", help = "Run one turn and print to stdout; without PROMPT the prompt is read from stdin")]
    pub print: Option<String>,

    #[arg(long = "output-format", value_enum, default_value_t = OutputFormat::Text, help = "Output of --print: text, json (one object at the end) or stream-json (one object per event per line)")]
    pub output_format: OutputFormat,

    #[arg(long = "allowed-tools", value_name = "RULES", value_delimiter = ',', help = "Allow these tools or rules without asking, e.g. read,bash(git diff:*)")]
    pub allowed_tools: Vec<String>,

    #[arg(long = "disallowed-tools", value_name = "RULES", value_delimiter = ',', help = "Deny these tools or rules; a bare tool name also hides the tool")]
    pub disallowed_tools: Vec<String>,

    #[arg(long = "max-turns", value_name = "N", help = "Stop after N model calls")]
    pub max_turns: Option<usize>,

    #[arg(long = "add-dir", value_name = "DIR", help = "Another directory the file tools may touch (repeatable)")]
    pub add_dir: Vec<PathBuf>,

    #[arg(long = "system-prompt-file", value_name = "FILE", help = "Replace zarn's base instructions with this file; ZARN.md, skills and the mode still follow")]
    pub system_prompt_file: Option<PathBuf>,

    #[arg(long = "append-system-prompt", value_name = "TEXT", help = "Add this text to the end of the system prompt")]
    pub append_system_prompt: Option<String>,

    #[arg(long = "no-session-persistence", help = "Do not save this --print run as a session")]
    pub no_session_persistence: bool,

    #[arg(long = "fork-session", help = "With --resume or --continue, copy the history into a new session and leave the original unchanged")]
    pub fork_session: bool,

    #[arg(long, value_name = "NAME", help = "Create or reuse .zarn/worktrees/NAME on a new branch and run there")]
    pub worktree: Option<String>,

    #[arg(long = "continue", help = "Resume the most recent session for this directory")]
    pub continue_session: bool,

    #[arg(long, value_name = "ID", help = "Resume a specific session by id")]
    pub resume: Option<String>,

    #[arg(long, value_name = "MODEL", help = "Override the profile's model")]
    pub model: Option<String>,

    #[arg(long, value_name = "PROFILE", help = "Use this profile instead of the default")]
    pub profile: Option<String>,

    #[arg(long, value_name = "NAME", help = "Start in this mode: default, plan, accept-edits, bypass, full or a custom mode")]
    pub mode: Option<String>,

    #[arg(long = "permission-mode", value_name = "NAME", help = "Permission policy to apply; --print needs --permission-mode bypass (or full) before it will run in bypass or full mode")]
    pub permission_mode: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    #[command(about = "Create or inspect the configuration")]
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    #[command(about = "Store an API key in the OS credential store and load the models it can use")]
    Login {
        #[arg(help = "Provider name, e.g. anthropic, openai, google, groq, openrouter, or one you added; leave it out and zarn works it out from the key")]
        provider: Option<String>,
    },
    #[command(about = "Update zarn to the latest release from GitHub")]
    Update {
        #[arg(long, help = "Only say whether a newer version exists")]
        check: bool,
    },
    #[command(about = "Open a page on localhost to add, test and remove API keys in your browser")]
    Setup {
        #[arg(long, value_name = "PORT", help = "Port to listen on; the default is 9276, or the next free one up to 9299")]
        port: Option<u16>,
        #[arg(long = "no-browser", help = "Print the link without opening a browser")]
        no_browser: bool,
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
    #[command(about = "Store prompts for an external scheduler such as cron to run with zarn run-scheduled")]
    Schedule {
        #[command(subcommand)]
        action: Option<ScheduleAction>,
    },
    #[command(name = "run-scheduled", about = "Run a stored prompt headlessly; meant to be called by cron or Task Scheduler")]
    RunScheduled { id: String },
    #[command(about = "Install, list, approve and remove plugins")]
    Plugin {
        #[command(subcommand)]
        action: Option<PluginAction>,
    },
    #[command(about = "List the hooks that run before and after tools, prompts and turns")]
    Hooks {
        #[command(subcommand)]
        action: Option<HooksAction>,
    },
    #[command(about = "List each provider's models, rate limits and credit balance")]
    Models {
        #[arg(long, value_name = "NAME", help = "Only this provider")]
        provider: Option<String>,
        #[arg(long, help = "Ignore the cache and ask the providers again")]
        refresh: bool,
    },
    #[command(about = "Show or set the session and daily spend caps")]
    Limits {
        #[arg(long, value_name = "USD", help = "Session cap in dollars, or off")]
        session: Option<String>,
        #[arg(long, value_name = "USD", help = "Daily cap in dollars, or off")]
        day: Option<String>,
    },
    #[command(about = "List and inspect modes")]
    Modes {
        #[command(subcommand)]
        action: Option<ModesAction>,
    },
    #[command(about = "Check config, keys, provider reachability, MCP servers and the terminal")]
    Doctor {
        #[arg(long, help = "Skip network probes and MCP server starts")]
        offline: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ScheduleAction {
    #[command(about = "Store a prompt to run in this directory")]
    Add {
        prompt: String,
        #[arg(long, value_name = "CRON", help = "The cron expression you intend to use; only shown in the suggested crontab line")]
        when: Option<String>,
        #[arg(long, value_name = "MODE", help = "Mode for the run (never bypass)")]
        mode: Option<String>,
    },
    #[command(about = "List stored prompts with a crontab line for each")]
    List,
    #[command(about = "Remove a stored prompt")]
    Remove { id: String },
}

#[derive(Subcommand, Debug)]
pub enum PluginAction {
    #[command(about = "Install a plugin from a directory or a git URL; nothing in it runs until you approve it")]
    Add { source: String },
    #[command(about = "List installed plugins, what they contribute and whether their code is approved")]
    List,
    #[command(about = "Show a plugin's hooks and MCP servers and allow them to run")]
    Approve {
        name: String,
        #[arg(long, help = "Approve without asking")]
        yes: bool,
    },
    #[command(about = "Stop a plugin's hooks and MCP servers from running")]
    Revoke { name: String },
    #[command(about = "Remove an installed plugin")]
    Remove { name: String },
}

#[derive(Subcommand, Debug)]
pub enum HooksAction {
    #[command(about = "List hooks by event, with their match pattern and command")]
    List,
}

#[derive(Subcommand, Debug)]
pub enum ModesAction {
    #[command(about = "List built-in and custom modes with their source")]
    List,
    #[command(about = "Show what a mode allows, its tools, prompt and profile")]
    Show {
        #[arg(help = "Mode name")]
        name: String,
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
    #[command(about = "Install skills from a git URL, a GitHub folder link, a local folder or a link to a SKILL.md")]
    Install {
        #[arg(help = "Where the skills are, e.g. https://github.com/owner/repo/tree/main/skills/pdf")]
        source: String,
        #[arg(long, help = "Install into this project's .zarn/skills instead of your user folder")]
        project: bool,
    },
    #[command(about = "Delete an installed skill")]
    Remove {
        #[arg(help = "Skill name")]
        name: String,
    },
    #[command(about = "Print a skill's SKILL.md and list its files")]
    Show {
        #[arg(help = "Skill name")]
        name: String,
    },
    #[command(about = "Make an empty skill from a template; /skills create in zarn has the model write one with you")]
    New {
        #[arg(help = "Skill name: lowercase letters, digits, '-' and '_'")]
        name: String,
        #[arg(help = "What the skill does and when to use it")]
        description: Vec<String>,
        #[arg(long, help = "Create it in this project's .zarn/skills instead of your user folder")]
        project: bool,
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
