mod cli;

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::io::Write;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use zarn_config::Config;
use zarn_core::agent::{Agent, AgentConfig, AgentEvent};
use zarn_core::provider::Event;
use zarn_core::tools::Ctx;
use zarn_core::types::Message;
use zarn_providers::{build_chain, ProviderSpec};
use zarn_tools::permissions::Permissions;
use cli::{Args, Command, ConfigAction, RunMode};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("ZARN_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
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

fn cmd_config_init() -> Result<()> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    let config_dir = home.join(".config/zarn");
    let config_path = config_dir.join("config.toml");

    if config_path.exists() {
        eprintln!("Config already exists at {}", config_path.display());
        return Ok(());
    }

    std::fs::create_dir_all(&config_dir)
        .with_context(|| format!("failed to create {}", config_dir.display()))?;

    let starter_config = r#"[providers.openai]
kind = "openai"

[providers.anthropic]
kind = "anthropic"

[profiles.default]
provider = "openai"
model = "gpt-4o-mini"
"#;

    std::fs::write(&config_path, starter_config)
        .with_context(|| format!("failed to write config to {}", config_path.display()))?;

    println!("Created starter config at {}", config_path.display());
    Ok(())
}

fn cmd_config_check() -> Result<()> {
    let cfg = Config::load().unwrap_or_default();

    println!("Providers:");
    for (name, spec) in &cfg.providers {
        println!("  {}: {}", name, spec.kind);
        if let Some(base_url) = &spec.base_url {
            println!("    base_url: {}", base_url);
        }
        if spec.api_key.is_some() {
            println!("    api_key: configured");
        }
        if let Some(context_window) = spec.context_window {
            println!("    context_window: {}", context_window);
        }
    }

    println!("\nProfiles:");
    for (name, profile) in &cfg.profiles {
        println!("  {}:", name);
        if let Some(provider) = &profile.provider {
            println!("    provider: {}", provider);
        }
        if let Some(model) = &profile.model {
            println!("    model: {}", model);
        }
        if let Some(fallback) = &profile.fallback {
            println!("    fallback: {}", fallback.join(", "));
        }
    }

    println!("\nPermissions: {} policies", cfg.permissions.len());

    Ok(())
}

async fn cmd_providers() -> Result<()> {
    let cfg = Config::load().unwrap_or_default();

    println!("Configured providers:");
    for (name, spec) in &cfg.providers {
        let status = if spec.api_key.is_some() {
            "configured"
        } else {
            "not configured"
        };

        println!("  {} [{}]: {}", name, status, spec.kind);
    }

    Ok(())
}

async fn run_agent(args: &Args) -> Result<()> {
    let mut cfg = Config::load().unwrap_or_default();

    let profile_name = args.profile
        .clone()
        .or_else(|| std::env::var("ZARN_PROFILE").ok())
        .unwrap_or_else(|| "default".to_string());

    let profile = cfg.profiles.entry(profile_name.clone())
        .or_default();

    let provider_name = profile.provider.as_deref()
        .unwrap_or("openai");

    let mut specs = vec![];
    if let Some(fallback) = &profile.fallback {
        for name in fallback {
            if let Some(spec) = cfg.providers.remove(name) {
                specs.push(spec);
            }
        }
    }
    if let Some(spec) = cfg.providers.remove(provider_name) {
        specs.insert(0, spec);
    } else {
        specs.insert(0, ProviderSpec {
            kind: provider_name.to_string(),
            ..ProviderSpec::default()
        });
    }

    let provider = build_chain(&specs)?;
    let model = args.model
        .clone()
        .or_else(|| profile.model.clone())
        .or_else(|| std::env::var("ZARN_MODEL").ok())
        .unwrap_or_else(|| "gpt-4o-mini".to_string());

    let tools = Arc::new(zarn_tools::builtins());
    let ctx = Arc::new(Ctx::new(std::env::current_dir()?));
    let config = AgentConfig {
        model,
        ..AgentConfig::default()
    };

    let run_mode = args.run_mode();

    match run_mode {
        RunMode::Interactive => {
            let (prompt_sink, permissions) = zarn_tui::permission_channel();
            let mut perms = Permissions::new(prompt_sink);
            perms = perms.with_tools(cfg.permissions);
            let gate = Arc::new(perms);
            let agent = Arc::new(
                Agent::new(provider, tools, ctx, config).with_gate(gate),
            );
            zarn_tui::run(agent, Vec::new(), permissions).await?;
        }
        RunMode::OneShot(prompt) => {
            let agent = Agent::new(provider, tools, ctx, config)
                .with_gate(Arc::new(Permissions::headless()));

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
            let mut messages = vec![Message::user(&prompt)];
            let result = agent.run(&mut messages, &tx, &cancel).await;
            drop(tx);
            let _ = printer.await;
            println!();

            match result {
                Ok(_) => {}
                Err(err) => bail!(err),
            }
        }
        RunMode::ContinueLast => {
            bail!("--continue not yet implemented");
        }
        RunMode::ResumeSession(_id) => {
            bail!("--resume not yet implemented");
        }
    }

    Ok(())
}
