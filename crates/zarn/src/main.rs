mod cli;

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use zarn_config::{Config, runtime};
use zarn_core::agent::{Agent, AgentEvent};
use zarn_core::provider::Event;
use zarn_core::session;
use zarn_core::tools::Ctx;
use zarn_core::types::Message;
use zarn_tools::permissions::Permissions;
use cli::{Args, Command, ConfigAction, RunMode};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("ZARN_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(|| zarn_config::guard::RedactingWriter::new(std::io::stderr()))
        .init();

    let args = Args::parse();

    match &args.command {
        Some(Command::Config { action }) => {
            match action {
                ConfigAction::Init => cmd_config_init()?,
                ConfigAction::Check => cmd_config_check()?,
            }
            return Ok(());
        }
        Some(Command::Providers) => {
            cmd_providers().await?;
            return Ok(());
        }
        None => {}
    }

    run_agent(&args).await
}

fn resume_messages(id: Option<&str>) -> Result<Vec<Message>> {
    let root = session::data_root();
    let cwd = std::env::current_dir()?;

    let path = match id {
        None => session::newest(&root, &cwd)
            .context("no previous session for this directory")?,
        Some(id) => session::find(&root, &cwd, id)
            .with_context(|| format!("no session matching '{id}' in this directory"))?,
    };

    let (meta, messages) = session::read(&path)?;
    eprintln!(
        "resuming {} ({} messages, {} in / {} out)",
        meta.id, messages.len(), meta.input_tokens, meta.output_tokens
    );
    Ok(messages)
}

fn config_target() -> Result<PathBuf> {
    Config::home_path().context("cannot determine a home directory for the config")
}

fn cmd_config_init() -> Result<()> {
    let path = config_target()?;
    if path.exists() {
        println!("config already exists at {}", path.display());
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, Config::starter())
        .with_context(|| format!("writing {}", path.display()))?;

    println!("wrote a starter config to {}", path.display());
    println!("set the api_key_env variables it names, then run: zarn config check");
    Ok(())
}

fn cmd_config_check() -> Result<()> {
    let sources = Config::sources();
    println!("sources, highest precedence first:");
    for path in &sources {
        let mark = if path.exists() { "found" } else { "absent" };
        println!("  [{mark}] {}", path.display());
    }

    let cfg = Config::load()?;
    println!();

    let mut providers: Vec<_> = cfg.providers.iter().collect();
    providers.sort_by_key(|(name, _)| name.as_str());
    println!("providers:");
    for (name, spec) in providers {
        println!("  {name} ({})", spec.kind);
        if let Some(base_url) = &spec.base_url {
            println!("    base_url      {base_url}");
        }
        match &spec.api_key_env {
            Some(var) => {
                let state = if std::env::var(var).is_ok() { "set" } else { "NOT SET" };
                println!("    api_key_env   {var} ({state})");
            }
            None => println!("    api_key_env   none"),
        }
        if let Some(window) = spec.context_window {
            println!("    context       {window}");
        }
    }

    let mut profiles: Vec<_> = cfg.profiles.iter().collect();
    profiles.sort_by_key(|(name, _)| name.as_str());
    println!("\nprofiles:");
    for (name, profile) in profiles {
        println!("  {name}");
        if let Some(provider) = &profile.provider {
            println!("    provider      {provider}");
        }
        if let Some(model) = &profile.model {
            println!("    model         {model}");
        }
        if let Some(fallback) = &profile.fallback
            && !fallback.is_empty()
        {
            println!("    fallback      {}", fallback.join(" -> "));
        }
    }

    let mut permissions: Vec<_> = cfg.permissions.iter().collect();
    permissions.sort_by_key(|(name, _)| name.as_str());
    println!("\npermissions:");
    if permissions.is_empty() {
        println!("  none set; risk defaults apply");
    }
    for (tool, policy) in permissions {
        println!("  {tool:<8} {policy:?}");
    }

    let mut servers: Vec<_> = cfg.mcp.iter().collect();
    servers.sort_by_key(|(name, _)| name.as_str());
    if !servers.is_empty() {
        println!("\nmcp servers:");
        for (name, server) in servers {
            match (&server.command, &server.url) {
                (Some(command), _) => {
                    println!("  {name:<12} stdio  {command} {}", server.args.join(" "))
                }
                (None, Some(url)) => println!("  {name:<12} http   {url}"),
                (None, None) => println!("  {name:<12} INVALID: neither command nor url"),
            }
        }
    }

    println!("\nconfig is valid");
    Ok(())
}

async fn reachable(base_url: &str) -> String {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    {
        Ok(client) => client,
        Err(err) => return format!("unknown ({err})"),
    };

    match client.get(base_url).send().await {
        Ok(response) => format!("reachable (HTTP {})", response.status().as_u16()),
        Err(err) if err.is_timeout() => "unreachable (timed out)".to_string(),
        Err(err) if err.is_connect() => "unreachable (connection refused)".to_string(),
        Err(_) => "unreachable".to_string(),
    }
}

async fn cmd_providers() -> Result<()> {
    let cfg = Config::load()?;
    if cfg.providers.is_empty() {
        println!("no providers configured; run: zarn config init");
        return Ok(());
    }

    let mut providers: Vec<_> = cfg.providers.iter().collect();
    providers.sort_by_key(|(name, _)| name.as_str());

    for (name, spec) in providers {
        let key = match &spec.api_key_env {
            None => "no key needed".to_string(),
            Some(var) if std::env::var(var).is_ok() => format!("{var} set"),
            Some(var) => format!("{var} NOT SET"),
        };
        let health = match &spec.base_url {
            Some(base_url) => reachable(base_url).await,
            None => "no base_url to probe".to_string(),
        };
        println!("{name:<12} {:<16} {key:<28} {health}", spec.kind);
    }

    Ok(())
}

async fn run_agent(args: &Args) -> Result<()> {
    let mut cfg = Config::load()?;
    let mcp = std::mem::take(&mut cfg.mcp);
    let store = zarn_config::auth::KeyStore::shared();
    let profile_name = runtime::active_profile(&cfg, args.profile.as_deref());
    let plan = runtime::plan(&cfg, store, &profile_name, args.model.as_deref())?;
    if let Some(notice) = store.fallback_notice() {
        eprintln!("{notice}");
    }

    let cwd = std::env::current_dir()?;
    let ctx = Arc::new(Ctx::new(&cwd));
    let mcp_tools = zarn_tools::mcp::connect_all(&mcp).await;
    let rt = runtime::assemble(&cfg, plan, &cwd, ctx.clone(), mcp_tools)?;
    let config = rt.agent_config();
    let provider = rt.provider.clone();
    let tools = rt.tools.clone();
    let system = Message::System(rt.system.clone());

    let run_mode = args.run_mode();

    match run_mode {
        RunMode::Interactive | RunMode::ContinueLast | RunMode::ResumeSession(_) => {
            let history = match &run_mode {
                RunMode::ContinueLast => resume_messages(None)?,
                RunMode::ResumeSession(id) => resume_messages(Some(id))?,
                _ => vec![system],
            };
            let (prompt_sink, permissions) = zarn_tui::permission_channel();
            let mut perms = Permissions::new(prompt_sink);
            perms = perms.with_tools(cfg.permissions.clone());
            let gate = Arc::new(perms);
            let agent = Arc::new(
                Agent::new(provider, tools, ctx, config).with_gate(gate),
            );
            zarn_tui::run(agent, history, permissions).await?;
        }
        RunMode::OneShot(prompt) => {
            let agent = Agent::new(provider, tools, ctx, config)
                .with_gate(Arc::new(Permissions::headless().with_tools(cfg.permissions.clone())));

            let (tx, mut rx) = tokio::sync::mpsc::channel(256);
            let printer = tokio::spawn(async move {
                let mut out = std::io::stdout();
                while let Some(event) = rx.recv().await {
                    match event {
                        AgentEvent::Provider(Event::TextDelta(text)) => {
                            let _ = write!(out, "{text}");
                            let _ = out.flush();
                        }
                        AgentEvent::ToolStarted { name, args, .. } => {
                            let _ = writeln!(out, "\n[tool] {name} {args}");
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
            let mut messages = vec![system, Message::user(&prompt)];
            let result = agent.run(&mut messages, &tx, &cancel).await;
            drop(tx);
            let _ = printer.await;
            println!();

            match result {
                Ok(_) => {}
                Err(err) => bail!(err),
            }
        }
    }

    Ok(())
}
