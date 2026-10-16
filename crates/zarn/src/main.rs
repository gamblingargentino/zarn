mod cli;
mod commands;

use anyhow::{Context, Result, bail};
use clap::Parser;
use cli::{Args, Command, RunMode};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use zarn_config::auth::{KeyStore, redact};
use zarn_config::guard::RedactingWriter;
use zarn_config::{Config, runtime};
use zarn_core::agent::{Agent, AgentEvent};
use zarn_core::provider::Event;
use zarn_core::session;
use zarn_core::tools::Ctx;
use zarn_core::types::Message;
use zarn_tools::permissions::Permissions;

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

    match args.run_mode() {
        RunMode::OneShot(prompt) => run_print(&args, prompt).await,
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
        ..zarn_tui::Options::default()
    })
    .await
}

async fn run_print(args: &Args, prompt: String) -> Result<()> {
    let mut cfg = Config::load()?;
    if !cfg.has_provider() {
        bail!(
            "no provider is configured. Run `zarn` for guided setup, or `zarn providers add` / `zarn login <provider>`."
        );
    }
    let mcp = std::mem::take(&mut cfg.mcp);
    let store = KeyStore::shared();
    let profile_name = runtime::active_profile(&cfg, args.profile.as_deref());
    let plan = runtime::plan(&cfg, &store, &profile_name, args.model.as_deref())?;
    if let Some(notice) = store.fallback_notice() {
        eprintln!("{notice}");
    }

    let cwd = std::env::current_dir()?;
    let ctx = Arc::new(Ctx::new(&cwd));
    let mcp_tools = zarn_tools::mcp::connect_all(&mcp).await;
    let notify: runtime::Notify = Arc::new(|text: String| eprintln!("{}", redact(&text)));
    let rt = runtime::assemble(
        &cfg,
        plan,
        &cwd,
        ctx.clone(),
        mcp_tools,
        runtime::Assembly {
            store: store.clone(),
            notify: Some(notify),
        },
    )?;
    let agent = Agent::new(rt.provider.clone(), rt.tools.clone(), ctx, rt.agent_config())
        .with_gate(Arc::new(Permissions::headless().with_tools(cfg.permissions.clone())));

    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let printer = tokio::spawn(async move {
        let mut out = std::io::stdout();
        while let Some(event) = rx.recv().await {
            match event {
                AgentEvent::Provider(Event::TextDelta(text)) => {
                    let _ = write!(out, "{}", redact(&text));
                    let _ = out.flush();
                }
                AgentEvent::ToolStarted { name, args, .. } => {
                    let _ = writeln!(out, "\n[tool] {name} {}", redact(&args.to_string()));
                    let _ = out.flush();
                }
                AgentEvent::ToolFinished { name, output, .. } => {
                    let status = if output.is_error { "failed" } else { "ok" };
                    let _ = writeln!(out, "[tool] {name} {status}");
                    let _ = out.flush();
                }
                _ => {}
            }
        }
    });

    let cancel = CancellationToken::new();
    let mut messages = vec![Message::System(rt.system.clone()), Message::user(&prompt)];
    let result = agent.run(&mut messages, &tx, &cancel).await;
    drop(tx);
    let _ = printer.await;
    println!();

    if let Err(err) = result {
        bail!(redact(&err.to_string()));
    }
    Ok(())
}
