use crate::cli::AddProvider;
use anyhow::{Context, Result, bail};
use zarn_config::auth::KeyStore;
use zarn_config::layers::{Layers, Scope};
use zarn_config::ops::{self, ProviderDraft};
use zarn_config::{Config, catalog, context, doctor, paths};
use zarn_tui::prompt;

fn layers() -> Result<Layers> {
    Layers::load()
}

pub fn config_init() -> Result<()> {
    let path = Config::home_path().context("cannot determine a home directory for the config")?;
    if path.exists() {
        println!("config already exists at {}", path.display());
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, Config::starter()).with_context(|| format!("writing {}", path.display()))?;
    println!("wrote a starter config to {}", path.display());
    println!("it has no provider yet: run `zarn` for guided setup, or `zarn providers add`");
    Ok(())
}

pub fn config_check() -> Result<()> {
    let store = KeyStore::shared();
    println!("sources, highest precedence first:");
    for path in Config::sources() {
        let mark = if path.exists() { "found" } else { "absent" };
        println!("  [{mark}] {}", path.display());
    }

    let cfg = Config::load()?;
    println!();
    println!("providers:");
    if cfg.providers.is_empty() {
        println!("  none; run `zarn` or `zarn providers add`");
    }
    for (name, spec) in &cfg.providers {
        println!("  {name} ({})", spec.kind);
        if let Some(base_url) = &spec.base_url {
            println!("    base_url      {base_url}");
        }
        if let Some(var) = &spec.api_key_env {
            let state = if std::env::var(var).is_ok() { "set" } else { "not set" };
            println!("    api_key_env   {var} ({state})");
        }
        let key = match store.has_with(name, spec.api_key_env.as_deref(), spec.api_key_helper.as_deref()) {
            Some(source) => source.describe(&store),
            None if catalog::needs_key(spec) => format!("MISSING: run zarn login {name}"),
            None => "not needed".into(),
        };
        println!("    key           {key}");
        if let Some(window) = spec.context_window {
            println!("    context       {window}");
        }
    }

    println!("\nprofiles (default: {}):", cfg.default_profile());
    for (name, profile) in &cfg.profiles {
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

    println!("\npermissions:");
    if cfg.permissions.is_empty() {
        println!("  none set; risk defaults apply");
    }
    for (tool, policy) in &cfg.permissions {
        println!("  {tool:<8} {policy:?}");
    }

    if !cfg.mcp.is_empty() {
        println!("\nmcp servers:");
        for (name, server) in &cfg.mcp {
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

fn ask_key(provider: &str) -> Result<Option<String>> {
    let label = format!("API key for {provider}");
    Ok(prompt::read_secret(&label)?.map(|s| s.expose().to_string()))
}

fn print_saved(store: &KeyStore, provider: &str, source: &zarn_config::auth::KeySource) {
    println!("key for {provider} saved to {}", source.describe(store));
    if let Some(notice) = store.fallback_notice() {
        eprintln!("{notice}");
    }
}

pub async fn login(provider: &str) -> Result<()> {
    let store = KeyStore::shared();
    let mut layers = layers()?;
    let Some(key) = ask_key(provider)? else {
        bail!("no key given; nothing changed");
    };
    let source = ops::login(&mut layers, &store, provider, &key)?;
    drop(key);
    print_saved(&store, provider, &source);
    match ops::test_named(&layers, &store, provider).await {
        Ok(ok) => println!("test: {ok}"),
        Err(err) => println!("test FAILED: {err}"),
    }
    Ok(())
}

pub fn logout(provider: &str) -> Result<()> {
    let store = KeyStore::shared();
    if ops::logout(&store, provider)? {
        println!("removed the stored key for {provider}");
    } else {
        println!("no stored key for {provider}");
    }
    if let Ok(layers) = layers()
        && let Some(spec) = layers.merged().providers.get(provider)
        && let Some(var) = &spec.api_key_env
        && std::env::var(var).is_ok()
    {
        println!("note: {provider} still reads a key from ${var}; unset it in your shell");
    }
    Ok(())
}

pub async fn providers_list() -> Result<()> {
    let store = KeyStore::shared();
    let layers = layers()?;
    let rows = ops::providers(&layers, &store);
    if rows.is_empty() {
        println!("no providers configured; run `zarn` for guided setup, or `zarn providers add`");
        return Ok(());
    }
    let cfg = layers.merged();
    for row in rows {
        let health = match cfg.providers.get(&row.name).and_then(catalog::probe_url) {
            Some(url) => match doctor::reachable(&url).await {
                Ok(text) | Err(text) => text,
            },
            None => "no endpoint to probe".to_string(),
        };
        let mark = if row.is_default { "*" } else { " " };
        println!(
            "{mark} {:<14} {:<14} {:<26} {:<8} {health}",
            row.name,
            row.kind,
            row.key_label(),
            row.scope.label()
        );
    }
    println!("* default provider");
    Ok(())
}

pub async fn providers_add(add: &AddProvider) -> Result<()> {
    let store = KeyStore::shared();
    let mut layers = layers()?;
    let interactive = prompt::interactive();

    let kind_id = match &add.kind {
        Some(kind) => kind.clone(),
        None if interactive => {
            let options: Vec<(&str, &str)> = catalog::KINDS.iter().map(|k| (k.id, k.label)).collect();
            prompt::choose("Provider kind:", &options)?.context("no such kind")?
        }
        None => bail!("--kind is required when stdin is not a terminal"),
    };
    let kind = catalog::kind(&kind_id).with_context(|| {
        format!(
            "unknown kind '{kind_id}'; use one of {}",
            catalog::KINDS.iter().map(|k| k.id).collect::<Vec<_>>().join(", ")
        )
    })?;

    let cfg = layers.merged();
    let mut default_name = kind.id.to_string();
    let mut n = 2;
    while cfg.providers.contains_key(&default_name) {
        default_name = format!("{}-{n}", kind.id);
        n += 1;
    }
    let name = match &add.name {
        Some(name) => name.clone(),
        None if interactive => prompt::read_line("Name", Some(&default_name))?,
        None => default_name,
    };
    if cfg.providers.contains_key(&name) {
        bail!("a provider named {name} already exists; remove it first or pick another --name");
    }

    let base_url = match &add.base_url {
        Some(url) => Some(url.clone()),
        None if kind.asks_base_url && interactive => {
            Some(prompt::read_line("Endpoint URL", kind.default_base_url)?)
        }
        None if kind.asks_base_url && kind.default_base_url.is_none() => {
            bail!("--base-url is required for {}", kind.id)
        }
        None => None,
    };

    let model = match &add.model {
        Some(model) => Some(model.clone()),
        None if interactive => {
            let suggestion = kind.models.first().copied();
            Some(prompt::read_line("Model", suggestion)?).filter(|m| !m.is_empty())
        }
        None => kind.models.first().map(|m| m.to_string()),
    };

    let draft = ProviderDraft {
        name: name.clone(),
        kind: kind.id.to_string(),
        base_url,
        api_key_env: add.api_key_env.clone(),
        model,
    };
    let wants_key = !add.no_key && (catalog::needs_key(&draft.spec()) || kind.id == "openai-compat");
    let key = if wants_key { ask_key(&name)? } else { None };
    if key.is_none() && catalog::needs_key(&draft.spec()) && !add.no_key {
        bail!("{name} needs an API key; nothing was saved");
    }

    let scope = ops::add_provider(&mut layers, &draft)?;
    println!("added {name} to the {} config", scope.label());
    if let Some(key) = key {
        let source = store.save(&name, &key)?;
        print_saved(&store, &name, &source);
    }
    if !add.no_test {
        match ops::test_named(&layers, &store, &name).await {
            Ok(ok) => println!("test: {ok}"),
            Err(err) => println!("test FAILED: {err}"),
        }
    }
    Ok(())
}

pub async fn providers_test(name: &str) -> Result<()> {
    let store = KeyStore::shared();
    let layers = layers()?;
    match ops::test_named(&layers, &store, name).await {
        Ok(ok) => {
            println!("{name}: {ok}");
            Ok(())
        }
        Err(err) => bail!("{name}: FAILED: {err}"),
    }
}

pub fn providers_remove(name: &str) -> Result<()> {
    let store = KeyStore::shared();
    let mut layers = layers()?;
    let had_key = ops::remove_provider(&mut layers, &store, name)?;
    println!(
        "removed {name}{}",
        if had_key { " and its stored key" } else { "" }
    );
    Ok(())
}

pub fn skills_list() -> Result<()> {
    let cfg = layers()?.merged();
    let skills = context::discover_skills();
    if skills.is_empty() {
        let (user, project) = context::skills_roots();
        println!(
            "no skills found in {} or {}",
            user.map(|p| p.display().to_string()).unwrap_or_else(|| "~/.config/zarn/skills".into()),
            project.display()
        );
        return Ok(());
    }
    for skill in skills {
        let state = if cfg.skill_enabled(&skill.name) { "on " } else { "off" };
        println!(
            "[{state}] {:<20} {:<8} {}",
            skill.name,
            skill.source.label(),
            skill.description
        );
    }
    Ok(())
}

pub fn skills_toggle(name: &str, enabled: bool) -> Result<()> {
    if !context::discover_skills().iter().any(|s| s.name == name) {
        bail!("no skill named {name}");
    }
    let mut layers = layers()?;
    let scope = ops::set_skill_enabled(&mut layers, name, enabled)?;
    println!(
        "{name} turned {} in the {} config",
        if enabled { "on" } else { "off" },
        scope.label()
    );
    Ok(())
}

pub async fn mcp_list(offline: bool) -> Result<()> {
    let cfg = layers()?.merged();
    if cfg.mcp.is_empty() {
        println!("no MCP servers; add one with: zarn mcp add <name> -- <command> [args...]");
        return Ok(());
    }
    for (name, server) in &cfg.mcp {
        let target = match (&server.command, &server.url) {
            (Some(command), _) => format!("stdio  {command} {}", server.args.join(" ")),
            (None, Some(url)) => format!("http   {url}"),
            (None, None) => "INVALID".to_string(),
        };
        let status = if offline {
            String::new()
        } else {
            match tokio::time::timeout(
                std::time::Duration::from_secs(20),
                zarn_tools::mcp::McpServer::start(name, server),
            )
            .await
            {
                Err(_) => "did not start in time".to_string(),
                Ok(Err(err)) => format!("failed: {}", zarn_config::auth::redact(&format!("{err:#}"))),
                Ok(Ok(running)) => {
                    let count = running.tool_names().len();
                    running.shutdown().await;
                    format!("ok, {count} tools")
                }
            }
        };
        println!("{name:<16} {target:<48} {status}");
    }
    Ok(())
}

pub fn mcp_add(name: &str, project: bool, target: &[String]) -> Result<()> {
    let mut layers = layers()?;
    let server = ops::parse_server(target)?;
    let scope = ops::mcp_add(&mut layers, name, server, project.then_some(Scope::Project))?;
    println!("added MCP server {name} to the {} config", scope.label());
    Ok(())
}

pub fn mcp_remove(name: &str) -> Result<()> {
    let mut layers = layers()?;
    ops::mcp_remove(&mut layers, name)?;
    println!("removed MCP server {name}");
    Ok(())
}

pub async fn doctor(offline: bool) -> Result<()> {
    let store = KeyStore::shared();
    let mut checks = doctor::run(&store, paths::project_config(), paths::user_config(), !offline).await;
    checks.extend(zarn_tui::terminal::checks(None));
    println!("{}", doctor::render(&checks));
    if checks.iter().any(|c| c.status == doctor::Status::Fail) {
        std::process::exit(1);
    }
    Ok(())
}

pub fn modes_list() -> Result<()> {
    let loaded = zarn_config::modes::load();
    let current = Config::load().map(|c| c.general.mode()).unwrap_or_else(|_| "default".into());
    for file in &loaded.modes {
        let mark = if file.def.name == current { "*" } else { " " };
        println!(
            "{mark} {:<16} {:<9} {}",
            file.def.name,
            file.source.label(),
            file.def.description
        );
    }
    for warning in &loaded.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

pub fn modes_show(name: &str) -> Result<()> {
    let loaded = zarn_config::modes::load();
    let Some(file) = loaded.find(name) else {
        bail!(
            "no mode named '{name}'; modes: {}",
            loaded.names().join(", ")
        );
    };
    println!("{}", zarn_config::modes::describe(file));
    Ok(())
}

pub async fn models(provider: Option<&str>, refresh: bool) -> Result<()> {
    let cfg = Config::load()?;
    if let Some(name) = provider
        && !cfg.providers.contains_key(name)
    {
        bail!("no provider named '{name}'");
    }
    let store = KeyStore::shared();
    let dir = zarn_config::quota::cache_dir();
    let reports = zarn_config::quota::reports(&cfg, &store, provider, refresh, &dir).await;
    println!("{}", zarn_config::auth::redact(&zarn_config::quota::render(&reports)));
    Ok(())
}

pub fn limits(session: Option<&str>, day: Option<&str>) -> Result<()> {
    let mut layers = layers()?;
    for (is_day, value) in [(false, session), (true, day)] {
        if let Some(value) = value {
            let usd = ops::parse_cap(value)?;
            let scope = ops::set_spend_cap(&mut layers, is_day, usd)?;
            println!(
                "{} spend cap {} in the {} config",
                if is_day { "daily" } else { "session" },
                usd.map(|v| format!("set to ${v:.2}")).unwrap_or_else(|| "removed".into()),
                scope.label()
            );
        }
    }
    let limits = layers.merged().limits;
    let show = |v: Option<f64>| v.map(|v| format!("${v:.2}")).unwrap_or_else(|| "off".into());
    println!("session cap  {}", show(limits.session_usd));
    println!(
        "daily cap    {} (spent today ${:.4})",
        show(limits.day_usd),
        zarn_config::spend::Ledger::system().today()
    );
    Ok(())
}
