use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "zarn")]
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

    #[arg(long, value_name = "PROFILE", help = "Use this profile instead of 'default'")]
    pub profile: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create or inspect the configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// List configured providers and whether they are reachable
    Providers,
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Write a starter config
    Init,
    /// Validate the config and show the resolved values
    Check,
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
