use crate::cli::{Args, OutputFormat};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use zarn_config::auth::{KeyStore, redact};
use zarn_config::{Config, modes, runtime};
use zarn_core::agent::{Agent, AgentEvent};
use zarn_core::cost::CostTracker;
use zarn_core::provider::Event;
use zarn_core::session::{self, Session};
use zarn_core::tools::Ctx;
use zarn_core::types::{Message, StopReason};
use zarn_tools::permissions::{Permissions, Rules};

pub fn read_prompt(given: String) -> Result<String> {
    if !given.trim().is_empty() {
        return Ok(given);
    }
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        bail!("--print needs a prompt, either as its argument or on stdin");
    }
    let mut text = String::new();
    stdin.read_to_string(&mut text).context("reading the prompt from stdin")?;
    if text.trim().is_empty() {
        bail!("--print got an empty prompt on stdin");
    }
    Ok(text.trim_end().to_string())
}

fn emit(out: &mut impl Write, value: &Value) {
    let _ = writeln!(out, "{}", redact(&value.to_string()));
    let _ = out.flush();
}

fn hidden_tools(disallowed: &[String]) -> Vec<String> {
    disallowed
        .iter()
        .map(|r| r.trim())
        .filter(|r| !r.is_empty() && !r.contains('('))
        .map(|r| r.to_lowercase())
        .collect()
}

fn history(args: &Args) -> Result<Option<PathBuf>> {
    let root = session::data_root();
    let cwd = std::env::current_dir()?;
    if let Some(id) = &args.resume {
        return session::find(&root, &cwd, id)
            .map(Some)
            .with_context(|| format!("no session matching '{id}' in this directory"));
    }
    if args.continue_session {
        return session::newest(&root, &cwd).map(Some).context("no previous session for this directory");
    }
    Ok(None)
}

pub fn mode(cfg: &Config, args: &Args) -> Result<(modes::ModeDef, zarn_tools::permissions::Overlay)> {
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

pub async fn run(args: &Args, prompt: String) -> Result<()> {
    let prompt = read_prompt(prompt)?;
    zarn_providers::patient::on_wait(Some(Arc::new(|text: String| eprintln!("{text}"))));
    let mut cfg = Config::load()?;
    if !cfg.has_provider() {
        bail!(
            "no provider is configured. Run `zarn` for guided setup, or `zarn providers add` / `zarn login <provider>`."
        );
    }
    cfg.permissions.rules.extend(&Rules {
        allow: args.allowed_tools.iter().map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).collect(),
        ask: Vec::new(),
        deny: args.disallowed_tools.iter().map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).collect(),
    });
    let mcp = std::mem::take(&mut cfg.mcp);
    let store = KeyStore::shared();
    let (mode, overlay) = mode(&cfg, args)?;
    let profile_name = runtime::active_profile(&cfg, mode.profile.as_deref().or(args.profile.as_deref()));
    let plan = runtime::plan(&cfg, &store, &profile_name, args.model.as_deref().or(mode.model.as_deref()))?;
    if let Some(notice) = store.fallback_notice() {
        eprintln!("{notice}");
    }

    let cwd = std::env::current_dir()?;
    let ctx = Arc::new(Ctx::new(&cwd));
    let mut authed = std::collections::BTreeMap::new();
    for (name, config) in &mcp {
        match zarn_config::mcp_auth::prepare(&store, name, config).await {
            Ok(ready) => {
                authed.insert(name.clone(), ready);
            }
            Err(err) => eprintln!("mcp {name} skipped: {err}"),
        }
    }
    let mcp_tools = zarn_tools::mcp::connect_all(&authed).await;
    let notify: runtime::Notify = Arc::new(|text: String| eprintln!("{}", redact(&text)));
    let shells = zarn_tools::shells::Shells::new();
    let gate = Arc::new(Permissions::headless().with_overlay(overlay));
    for err in cfg.apply_permissions(&gate, &cwd) {
        eprintln!("permission rule ignored: {err}");
    }
    gate.set_boundary(Some(args.add_dir.clone()));
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

    let mut config = rt.agent_config();
    if let Some(turns) = args.max_turns {
        config.max_iterations = turns.max(1);
    }
    let hidden = hidden_tools(&args.disallowed_tools);
    let lazy = rt.filter();
    let mut agent = Agent::new(rt.provider.clone(), rt.tools.clone(), ctx, config).with_gate(hooked);
    if !hidden.is_empty() || lazy.is_some() {
        agent = agent.with_filter(Arc::new(move |spec: &zarn_core::types::ToolSpec| {
            !hidden.contains(&spec.name.to_lowercase()) && lazy.as_ref().is_none_or(|keep| keep(spec))
        }));
    }

    let mut system = rt.system.clone();
    if let Some(path) = &args.system_prompt_file {
        let base = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut parts = rt.parts.clone();
        parts.base = base.trim().to_string();
        system = parts.join();
    }
    if let Some(extra) = &args.append_system_prompt {
        system = format!("{system}\n\n{}", extra.trim());
    }

    let blocked = zarn_config::hooks::fire(&cfg.hooks, "prompt_submit", None, json!({ "prompt": prompt }), &cwd).await;
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
    } else if let zarn_config::spend::Check::Stop(text) = zarn_config::spend::check(&limits, 0.0, day_base) {
        bail!("stopped: {text}");
    }

    let source = history(args)?;
    let mut messages = vec![Message::System(system)];
    if let Some(path) = &source {
        let (_, loaded) = session::read(path)?;
        messages.extend(loaded.into_iter().skip_while(|m| matches!(m, Message::System(_))));
    }
    let start = messages.len();
    messages.push(Message::user(&prompt));

    let mut saved = if args.no_session_persistence {
        None
    } else {
        match (&source, args.fork_session) {
            (Some(path), false) => Some((Session::open(path)?, start)),
            _ => Some((Session::create(&session::data_root(), &cwd, &rt.model, Some(profile_name.clone()))?, 1)),
        }
    };

    let format = args.output_format;
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let post_hooks = cfg.hooks.clone();
    let hook_cwd = cwd.clone();
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let printer = tokio::spawn(async move {
        let mut out = std::io::stdout();
        let mut stopped = false;
        let mut calls: Vec<Value> = Vec::new();
        let mut started: std::collections::HashMap<String, Value> = Default::default();
        while let Some(event) = rx.recv().await {
            if let AgentEvent::ToolStarted { id, args, .. } = &event {
                started.insert(id.clone(), args.clone());
            }
            if let AgentEvent::ToolFinished { id, name, output } = &event {
                calls.push(json!({
                    "id": id,
                    "name": name,
                    "input": started.get(id).cloned().unwrap_or_default(),
                    "is_error": output.is_error,
                    "output": output.content,
                }));
                if !post_hooks.post_tool.is_empty() {
                    let payload = json!({
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
            }
            if let AgentEvent::Provider(event) = &event {
                tracker.observe(event);
                if priced && !stopped && matches!(event, Event::Usage { .. }) {
                    let spent = tracker.total().usd.unwrap_or(0.0);
                    match zarn_config::spend::check(&limits, spent, day_base + spent) {
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
            match format {
                OutputFormat::Text => match event {
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
                },
                OutputFormat::StreamJson => {
                    let line = match &event {
                        AgentEvent::Provider(Event::TextDelta(text)) => Some(json!({ "type": "text", "text": text })),
                        AgentEvent::Provider(Event::Usage { input, output, cached }) => Some(json!({
                            "type": "usage", "input_tokens": input, "output_tokens": output, "cached_tokens": cached
                        })),
                        AgentEvent::Provider(Event::Error(err)) => Some(json!({ "type": "error", "message": err.to_string() })),
                        AgentEvent::ToolStarted { id, name, args } => Some(json!({ "type": "tool_use", "id": id, "name": name, "input": args })),
                        AgentEvent::ToolFinished { id, name, output } => Some(json!({
                            "type": "tool_result", "id": id, "name": name, "is_error": output.is_error, "content": output.content
                        })),
                        _ => None,
                    };
                    if let Some(line) = line {
                        emit(&mut out, &line);
                    }
                }
                OutputFormat::Json => {}
            }
        }
        (tracker, calls)
    });

    let result = agent.run(&mut messages, &tx, &cancel).await;
    shells.kill_all();
    drop(tx);
    for outcome in zarn_config::hooks::fire(&cfg.hooks, "turn_end", None, json!({ "messages": messages.len() }), &cwd).await {
        if !outcome.ok() {
            eprintln!("turn_end hook: {}", outcome.reason());
        }
    }
    let (tracker, calls) = printer.await.unwrap_or_else(|_| (CostTracker::new(Default::default(), String::new()), Vec::new()));
    let total = tracker.total();
    if let Some(usd) = total.usd
        && let Err(err) = ledger.add(usd)
    {
        eprintln!("daily spend not recorded: {err}");
    }

    let mut session_id = None;
    if let Some((session, from)) = saved.as_mut() {
        let from = (*from).clamp(1, messages.len());
        match zarn_config::guard::append_redacted(session, &messages[from..]) {
            Ok(()) => {
                let _ = session.record_usage(total.usage.input, total.usage.output, total.usage.cached);
                session_id = Some(session.meta().id.clone());
            }
            Err(err) => eprintln!("session not saved: {err:#}"),
        }
    }

    let reached_limit = matches!(result, Ok(StopReason::MaxTokens)) && args.max_turns.is_some();
    if reached_limit {
        eprintln!("stopped after --max-turns {}", args.max_turns.unwrap_or_default());
    }
    let text = messages[start..]
        .iter()
        .rev()
        .find_map(|m| match m {
            Message::Assistant { .. } if !m.text().trim().is_empty() => Some(m.text()),
            _ => None,
        })
        .unwrap_or_default();
    let stop_reason = match &result {
        Ok(StopReason::EndTurn) => "end_turn",
        Ok(StopReason::ToolUse) => "tool_use",
        Ok(StopReason::MaxTokens) if reached_limit => "max_turns",
        Ok(StopReason::MaxTokens) => "max_tokens",
        Ok(StopReason::Cancelled) => "cancelled",
        Ok(_) => "other",
        Err(_) => "error",
    };
    let summary = json!({
        "text": text,
        "tool_calls": calls,
        "usage": {
            "input_tokens": total.usage.input,
            "output_tokens": total.usage.output,
            "cached_tokens": total.usage.cached
        },
        "cost_usd": total.usd,
        "model": rt.model,
        "stop_reason": stop_reason,
        "session_id": session_id,
        "error": result.as_ref().err().map(|e| e.to_string()),
    });
    let mut out = std::io::stdout();
    match format {
        OutputFormat::Text => println!(),
        OutputFormat::Json => emit(&mut out, &summary),
        OutputFormat::StreamJson => {
            let mut last = summary.clone();
            last["type"] = json!("result");
            emit(&mut out, &last);
        }
    }

    if let Err(err) = result {
        bail!(redact(&err.to_string()));
    }
    Ok(())
}
