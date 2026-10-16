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
use zarn_config::{Config, modes, runtime};
use zarn_core::agent::AgentEvent;
use zarn_core::cost::CostTracker;
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
        Command::Hooks { .. } => commands::hooks_list(),
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
    let (mode, overlay) = print_mode(&cfg, args)?;
    let profile_name = runtime::active_profile(
        &cfg,
        mode.profile.as_deref().or(args.profile.as_deref()),
    );
    let plan = runtime::plan(
        &cfg,
        &store,
        &profile_name,
        args.model.as_deref().or(mode.model.as_deref()),
    )?;
    if let Some(notice) = store.fallback_notice() {
        eprintln!("{notice}");
    }

    let cwd = std::env::current_dir()?;
    let ctx = Arc::new(Ctx::new(&cwd));
    let mut authed = std::collections::BTreeMap::new();
    for (name, config) in &mcp {
        authed.insert(name.clone(), zarn_config::mcp_auth::with_auth(&store, name, config).await);
    }
    let mcp_tools = zarn_tools::mcp::connect_all(&authed).await;
    let notify: runtime::Notify = Arc::new(|text: String| eprintln!("{}", redact(&text)));
    let shells = zarn_tools::shells::Shells::new();
    let gate = Arc::new(Permissions::headless().with_overlay(overlay));
    for err in cfg.apply_permissions(&gate, &cwd) {
        eprintln!("permission rule ignored: {err}");
    }
    let hooks = Arc::new(std::sync::RwLock::new(cfg.hooks.clone()));
    let hooked: Arc<dyn zarn_core::agent::ToolGate> =
        Arc::new(zarn_config::hooks::HookedGate::new(gate, hooks, cwd.clone()));
    let rt = runtime::assemble(
        &cfg,
        plan,
        &cwd,
        ctx.clone(),
        mcp_tools,
        runtime::Assembly {
            store: store.clone(),
            notify: Some(notify),
            mode: Some(mode),
            approver: Some(Arc::new(zarn_tools::exit_plan::Unattended)),
            todos: Some(Default::default()),
            shells: Some(shells.clone()),
            task_gate: Some(hooked.clone()),
            task_monitor: None,
            background: None,
            memory: None,
        },
    )?;
    for warning in &rt.warnings {
        eprintln!("{warning}");
    }
    let agent = rt.agent(ctx, hooked);
    let blocked = zarn_config::hooks::fire(
        &cfg.hooks,
        "prompt_submit",
        None,
        serde_json::json!({ "prompt": prompt }),
        &cwd,
    )
    .await;
    if let Some(outcome) = blocked.iter().find(|o| !o.ok()) {
        bail!("prompt blocked by a prompt_submit hook: {}", outcome.reason());
    }

    let limits = cfg.limits;
    let ledger = zarn_config::spend::Ledger::system();
    let mut tracker = CostTracker::new(zarn_config::pricing::table(&cfg), rt.model.clone());
    let priced = tracker.rates().is_some();
    let day_base = ledger.today();
    if let Some(note) = zarn_config::spend::unenforceable(&limits, &rt.model, priced) {
        eprintln!("{note}");
    } else if let zarn_config::spend::Check::Stop(text) =
        zarn_config::spend::check(&limits, 0.0, day_base)
    {
        bail!("stopped: {text}");
    }

    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let post_hooks = cfg.hooks.clone();
    let hook_cwd = cwd.clone();
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let printer = tokio::spawn(async move {
        let mut out = std::io::stdout();
        let mut stopped = false;
        let mut started: std::collections::HashMap<String, serde_json::Value> = Default::default();
        while let Some(event) = rx.recv().await {
            if let AgentEvent::ToolStarted { id, args, .. } = &event {
                started.insert(id.clone(), args.clone());
            }
            if let AgentEvent::ToolFinished { id, name, output } = &event
                && !post_hooks.post_tool.is_empty()
            {
                let payload = serde_json::json!({
                    "args": started.get(id).cloned().unwrap_or_default(),
                    "output": output.content,
                    "is_error": output.is_error
                });
                for outcome in zarn_config::hooks::fire(&post_hooks, "post_tool", Some(name), payload, &hook_cwd).await {
                    if !outcome.ok() {
                        eprintln!("post_tool hook: {}", outcome.reason());
                    }
                }
            }
            if let AgentEvent::Provider(event) = &event {
                tracker.observe(event);
                if priced && !stopped && matches!(event, Event::Usage { .. }) {
                    let session = tracker.total().usd.unwrap_or(0.0);
                    match zarn_config::spend::check(&limits, session, day_base + session) {
                        zarn_config::spend::Check::Stop(text) => {
                            stopped = true;
                            eprintln!("stopped: {text}");
                            stop.cancel();
                        }
                        zarn_config::spend::Check::Warn(warnings) => {
                            for warning in warnings {
                                eprintln!("{warning}");
                            }
                        }
                        zarn_config::spend::Check::Ok => {}
                    }
                }
            }
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
        tracker
    });

    let mut messages = vec![Message::System(rt.system.clone()), Message::user(&prompt)];
    let result = agent.run(&mut messages, &tx, &cancel).await;
    shells.kill_all();
    drop(tx);
    for outcome in zarn_config::hooks::fire(
        &cfg.hooks,
        "turn_end",
        None,
        serde_json::json!({ "messages": messages.len() }),
        &cwd,
    )
    .await
    {
        if !outcome.ok() {
            eprintln!("turn_end hook: {}", outcome.reason());
        }
    }
    if let Ok(tracker) = printer.await
        && let Some(usd) = tracker.total().usd
        && let Err(err) = ledger.add(usd)
    {
        eprintln!("daily spend not recorded: {err}");
    }
    println!();

    if let Err(err) = result {
        bail!(redact(&err.to_string()));
    }
    Ok(())
}

fn print_mode(
    cfg: &Config,
    args: &Args,
) -> Result<(modes::ModeDef, zarn_tools::permissions::Overlay)> {
    let loaded = modes::load();
    let find = |name: &str| {
        loaded.find(name).map(|m| m.def.clone()).with_context(|| {
            format!("no mode named '{name}'; modes: {}", loaded.names().join(", "))
        })
    };
    let name = args
        .mode
        .clone()
        .or_else(|| args.permission_mode.clone())
        .unwrap_or_else(|| cfg.general.mode());
    let mode = find(&name)?;
    let policy = match &args.permission_mode {
        Some(permission) => find(permission)?,
        None => mode.clone(),
    };
    if policy.allow_all && args.permission_mode.as_deref() != Some("bypass") {
        bail!(
            "--print will not run in bypass mode unless you pass --permission-mode bypass. {}",
            modes::BYPASS_RISK
        );
    }
    Ok((mode, modes::overlay(&policy)))
}
