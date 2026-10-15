use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "zarn")]
#[command(about = "Interactive AI agent CLI", long_about = None)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[arg(long, value_name = "PROMPT")]
    pub print: Option<String>,

    #[arg(long)]
    pub continue_session: bool,

    #[arg(long, value_name = "ID")]
    pub resume: Option<String>,

    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,

    #[arg(long, value_name = "PROFILE")]
    pub profile: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    Providers,
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    Init,
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
