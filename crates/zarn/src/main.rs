mod cli;
mod commands;
mod print;

use anyhow::{Context, Result};
use clap::Parser;
use cli::{Args, Command, RunMode};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use zarn_config::auth::redact;
use zarn_config::guard::RedactingWriter;
use zarn_core::session;

struct SharedFile(Arc<std::fs::File>);

impl Write for SharedFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        (&*self.0).write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        (&*self.0).flush()
    }
}

fn log_file() -> Option<std::fs::File> {
    let dir = session::data_root().parent()?.to_path_buf();
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("zarn.log"))
        .ok()
}

fn init_logging(to_file: bool) {
    let filter = tracing_subscriber::EnvFilter::try_from_env("ZARN_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let file = if to_file { log_file() } else { None };
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match file {
        Some(file) => {
            let file = Arc::new(file);
            builder
                .with_ansi(false)
                .with_writer(move || RedactingWriter::new(SharedFile(file.clone())))
                .init()
        }
        None => builder
            .with_writer(|| RedactingWriter::new(std::io::stderr()))
            .init(),
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match start().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {}", redact(&format!("{err:#}")));
            std::process::ExitCode::FAILURE
        }
    }
}

async fn start() -> Result<()> {
    let args = Args::parse();
    let interactive = args.command.is_none() && !matches!(args.run_mode(), RunMode::OneShot(_));
    init_logging(interactive);

    if let Some(command) = &args.command {
        return run_command(command).await;
    }

    if let Some(name) = &args.worktree {
        let root = zarn_core::worktree::repo_root(&std::env::current_dir()?)?;
        let path = zarn_core::worktree::create(&root, name)?;
        std::env::set_current_dir(&path).with_context(|| format!("entering {}", path.display()))?;
        eprintln!("working in {} on branch {name}", path.display());
    }

    match args.run_mode() {
        RunMode::OneShot(prompt) => print::run(&args, prompt).await,
        RunMode::Interactive => tui(&args, None).await,
        RunMode::ContinueLast => tui(&args, Some(session_path(None)?)).await,
        RunMode::ResumeSession(id) => tui(&args, Some(session_path(Some(&id))?)).await,
    }
}

async fn run_command(command: &Command) -> Result<()> {
    use cli::{ConfigAction, McpAction, ProvidersAction, SkillsAction};
    match command {
        Command::Config { action } => match action {
            ConfigAction::Init => commands::config_init(),
            ConfigAction::Check => commands::config_check(),
        },
        Command::Login { provider } => commands::login(provider).await,
        Command::Setup { port, no_browser } => commands::setup(*port, *no_browser).await,
        Command::Logout { provider } => commands::logout(provider),
        Command::Providers { action } => match action {
            None | Some(ProvidersAction::List) => commands::providers_list().await,
            Some(ProvidersAction::Add(add)) => commands::providers_add(add).await,
            Some(ProvidersAction::Test { name }) => commands::providers_test(name).await,
            Some(ProvidersAction::Remove { name }) => commands::providers_remove(name),
        },
        Command::Skills { action } => match action {
            None | Some(SkillsAction::List) => commands::skills_list(),
            Some(SkillsAction::Enable { name }) => commands::skills_toggle(name, true),
            Some(SkillsAction::Disable { name }) => commands::skills_toggle(name, false),
        },
        Command::Mcp { action } => match action {
            None => commands::mcp_list(false).await,
            Some(McpAction::List { offline }) => commands::mcp_list(*offline).await,
            Some(McpAction::Add {
                name,
                project,
                target,
            }) => commands::mcp_add(name, *project, target),
            Some(McpAction::Remove { name }) => commands::mcp_remove(name),
        },
        Command::Hooks { .. } => commands::hooks_list(),
        Command::Schedule { action } => match action {
            None | Some(cli::ScheduleAction::List) => commands::schedule_list(),
            Some(cli::ScheduleAction::Add { prompt, when, mode }) => commands::schedule_add(prompt, when.clone(), mode.clone()),
            Some(cli::ScheduleAction::Remove { id }) => commands::schedule_remove(id),
        },
        Command::RunScheduled { id } => {
            let entry = zarn_config::schedule::Schedules::system().get(id)?;
            std::env::set_current_dir(&entry.cwd).with_context(|| format!("entering {}", entry.cwd.display()))?;
            let mut argv = vec!["zarn".to_string()];
            if let Some(mode) = &entry.mode {
                argv.push("--mode".into());
                argv.push(mode.clone());
            }
            argv.push("--print".into());
            argv.push(entry.prompt.clone());
            let args = Args::parse_from(argv);
            print::run(&args, entry.prompt.clone()).await
        }
        Command::Plugin { action } => match action {
            None | Some(cli::PluginAction::List) => commands::plugin_list(),
            Some(cli::PluginAction::Add { source }) => commands::plugin_add(source),
            Some(cli::PluginAction::Approve { name, yes }) => commands::plugin_approve(name, *yes),
            Some(cli::PluginAction::Revoke { name }) => commands::plugin_revoke(name),
            Some(cli::PluginAction::Remove { name }) => commands::plugin_remove(name),
        },
        Command::Models { provider, refresh } => commands::models(provider.as_deref(), *refresh).await,
        Command::Limits { session, day } => commands::limits(session.as_deref(), day.as_deref()),
        Command::Modes { action } => match action {
            None | Some(cli::ModesAction::List) => commands::modes_list(),
            Some(cli::ModesAction::Show { name }) => commands::modes_show(name),
        },
        Command::Doctor { offline } => commands::doctor(*offline).await,
    }
}

fn session_path(id: Option<&str>) -> Result<PathBuf> {
    let root = session::data_root();
    let cwd = std::env::current_dir()?;
    match id {
        None => session::newest(&root, &cwd).context("no previous session for this directory"),
        Some(id) => session::find(&root, &cwd, id)
            .with_context(|| format!("no session matching '{id}' in this directory")),
    }
}

async fn tui(args: &Args, resume: Option<PathBuf>) -> Result<()> {
    zarn_tui::run(zarn_tui::Options {
        profile: args.profile.clone(),
        model: args.model.clone(),
        resume,
        mode: args.mode.clone().or_else(|| args.permission_mode.clone()),
        fork: args.fork_session,
        add_dirs: args.add_dir.clone(),
        ..zarn_tui::Options::default()
    })
    .await
}
