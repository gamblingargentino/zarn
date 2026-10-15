use anyhow::{Context, Result, bail};
use std::io::Write;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use zarn_core::agent::{Agent, AgentConfig, AgentEvent};
use zarn_core::provider::Event;
use zarn_core::tools::{Ctx, ToolRegistry};
use zarn_core::types::Message;
use zarn_providers::OpenAiCompatProvider;
use zarn_tools::permissions::Permissions;

fn usage() -> ! {
    eprintln!("usage: zarn [--print \"prompt\"]");
    std::process::exit(2);
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("ZARN_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let prompt = match args.as_slice() {
        [] => None,
        [flag, prompt] if flag == "--print" => Some(prompt.clone()),
        _ => usage(),
    };

    let base_url =
        std::env::var("ZARN_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
    let api_key = std::env::var("ZARN_API_KEY")
        .context("ZARN_API_KEY is not set")
        .map_err(|e| {
            eprintln!("{e:#}");
            e
        })?;
    let model = std::env::var("ZARN_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());

    let provider = Arc::new(OpenAiCompatProvider::new(base_url, api_key));
    let tools = Arc::new(ToolRegistry::new());
    let ctx = Arc::new(Ctx::new(std::env::current_dir()?));
    let config = AgentConfig {
        model,
        ..AgentConfig::default()
    };

    let Some(prompt) = prompt else {
        let (prompt_sink, permissions) = zarn_tui::permission_channel();
        let gate = Arc::new(Permissions::new(prompt_sink));
        let agent = Arc::new(
            Agent::new(provider, tools, ctx, config).with_gate(gate),
        );
        zarn_tui::run(agent, Vec::new(), permissions).await?;
        return Ok(());
    };

    let agent = Agent::new(provider, tools, ctx, config).with_gate(Arc::new(Permissions::headless()));

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
    let mut messages = vec![Message::user(prompt)];
    let result = agent.run(&mut messages, &tx, &cancel).await;
    drop(tx);
    let _ = printer.await;
    println!();

    match result {
        Ok(_) => Ok(()),
        Err(err) => bail!(err),
    }
}
