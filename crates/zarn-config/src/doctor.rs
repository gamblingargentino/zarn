use crate::auth::KeyStore;
use crate::layers::Layers;
use crate::{Config, catalog, context, paths, runtime};
use std::path::{Path, PathBuf};
use std::time::Duration;
use zarn_tools::mcp::McpServer;

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const MCP_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    pub fn mark(&self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub area: String,
    pub status: Status,
    pub detail: String,
}

impl Check {
    pub fn new(area: impl Into<String>, status: Status, detail: impl Into<String>) -> Self {
        Check {
            area: area.into(),
            status,
            detail: crate::auth::redact(&detail.into()),
        }
    }
}

pub fn render(checks: &[Check]) -> String {
    let width = checks.iter().map(|c| c.area.len()).max().unwrap_or(0);
    let mut out: Vec<String> = checks
        .iter()
        .map(|c| format!("[{:<4}] {:<width$}  {}", c.status.mark(), c.area, c.detail))
        .collect();
    let fails = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warns = checks.iter().filter(|c| c.status == Status::Warn).count();
    out.push(format!("{fails} failed, {warns} warnings"));
    out.join("\n")
}

fn file_check(label: &str, path: Option<&Path>) -> Check {
    let area = format!("config ({label})");
    let Some(path) = path else {
        return Check::new(area, Status::Warn, "no home directory");
    };
    if !path.exists() {
        return Check::new(area, Status::Ok, format!("{} not present", path.display()));
    }
    match Config::load_file(path) {
        Ok(_) => Check::new(area, Status::Ok, format!("{} parses", path.display())),
        Err(err) => Check::new(area, Status::Fail, format!("{err:#}")),
    }
}

pub async fn reachable(url: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    match client.get(url).send().await {
        Ok(response) => Ok(format!("reachable (HTTP {})", response.status().as_u16())),
        Err(err) if err.is_timeout() => Err("unreachable (timed out)".into()),
        Err(err) if err.is_connect() => Err("unreachable (connection failed)".into()),
        Err(err) => Err(format!("unreachable ({err})")),
    }
}

pub async fn run(store: &KeyStore, project: PathBuf, user: Option<PathBuf>, probe: bool) -> Vec<Check> {
    let mut checks = vec![
        file_check("project", Some(&project)),
        file_check("user", user.as_deref()),
    ];

    let layers = match Layers::load_from(project, user) {
        Ok(layers) => {
            checks.push(Check::new("config", Status::Ok, "merged config is valid"));
            layers
        }
        Err(err) => {
            checks.push(Check::new("config", Status::Fail, format!("{err:#}")));
            return checks;
        }
    };
    let cfg = layers.merged();

    checks.push(if store.store_available() {
        Check::new("credential store", Status::Ok, store.backend_label())
    } else {
        Check::new(
            "credential store",
            Status::Warn,
            format!(
                "{} unavailable; keys fall back to {}",
                store.backend_label(),
                store.file_path().display()
            ),
        )
    });

    if cfg.providers.is_empty() {
        checks.push(Check::new(
            "providers",
            Status::Fail,
            "none configured; run zarn to set one up, or zarn providers add",
        ));
    }

    let profile = runtime::active_profile(&cfg, None);
    if cfg.has_provider() {
        checks.push(match runtime::plan(&cfg, store, &profile, None) {
            Ok(plan) => {
                let mut detail = format!(
                    "profile {} uses {} with model {}",
                    plan.profile, plan.provider_name, plan.model
                );
                if !plan.skipped.is_empty() {
                    detail.push_str(&format!(
                        "; fallbacks without keys skipped: {}",
                        plan.skipped.join(", ")
                    ));
                }
                Check::new("profile", Status::Ok, detail)
            }
            Err(err) => Check::new("profile", Status::Fail, format!("{err:#}")),
        });
    }

    for (name, spec) in &cfg.providers {
        let area = format!("provider {name}");
        let key = store.has_with(name, spec.api_key_env.as_deref(), spec.api_key_helper.as_deref());
        let key_text = match (&key, catalog::needs_key(spec)) {
            (Some(source), _) => format!("key from {}", source.describe(store)),
            (None, true) => "NO KEY: run zarn login ".to_string() + name,
            (None, false) => "no key needed".into(),
        };
        let mut status = if key.is_none() && catalog::needs_key(spec) {
            Status::Fail
        } else {
            Status::Ok
        };
        let mut detail = format!("{} · {key_text}", spec.kind);
        if probe {
            match catalog::probe_url(spec) {
                Some(url) => match reachable(&url).await {
                    Ok(text) => detail.push_str(&format!(" · {text}")),
                    Err(text) => {
                        status = Status::Fail;
                        detail.push_str(&format!(" · {text}"));
                    }
                },
                None => detail.push_str(" · no endpoint to probe"),
            }
        }
        checks.push(Check::new(area, status, detail));
    }

    for (name, server) in &cfg.mcp {
        let area = format!("mcp {name}");
        if !probe {
            checks.push(Check::new(area, Status::Ok, "configured"));
            continue;
        }
        let ready = match crate::mcp_auth::prepare(store, name, server).await {
            Ok(ready) => ready,
            Err(err) => {
                checks.push(Check::new(area, Status::Fail, format!("{err}; fix the reference in [mcp.{name}] or run zarn login")));
                continue;
            }
        };
        let started = tokio::time::timeout(MCP_TIMEOUT, McpServer::start(name, &ready)).await;
        checks.push(match started {
            Err(_) => Check::new(area, Status::Fail, "did not start in time"),
            Ok(Err(err)) => Check::new(area, Status::Fail, format!("{err:#}")),
            Ok(Ok(running)) => {
                let count = running.tool_names().len();
                running.shutdown().await;
                Check::new(area, Status::Ok, format!("running, {count} tools"))
            }
        });
    }

    let skills = context::discover_skills();
    let enabled = skills.iter().filter(|s| cfg.skill_enabled(&s.name)).count();
    checks.push(Check::new(
        "skills",
        Status::Ok,
        format!("{} found, {enabled} enabled", skills.len()),
    ));

    let memory = context::memory_files(Path::new("."), paths::user_dir().as_deref());
    checks.push(Check::new(
        "ZARN.md",
        Status::Ok,
        if memory.is_empty() {
            "none; /init writes one for this project".to_string()
        } else {
            memory
                .iter()
                .map(|m| format!("{} ({} tokens)", m.label, context::estimate_tokens(&m.text)))
                .collect::<Vec<_>>()
                .join(", ")
        },
    ));

    checks.push(sandbox_check(&cfg));
    checks.push(hooks_check(&cfg));
    checks.push(modes_check(&cfg, &crate::modes::load()));
    checks.push(routing_check(&cfg, store));
    checks.push(discovery_check(&crate::quota::cache_dir()));
    checks.push(plugins_check(&crate::plugins::installed()));
    checks.push(spend_check(&cfg, store));
    if let Some(search) = &cfg.websearch {
        checks.push(match search.backend(store) {
            Ok(_) => Check::new("websearch", Status::Ok, "backend configured"),
            Err(err) => Check::new("websearch", Status::Fail, format!("{err}; fix [websearch] in config.toml")),
        });
    }

    checks
}

pub fn modes_check(cfg: &Config, loaded: &crate::modes::Loaded) -> Check {
    let active = cfg.general.mode();
    if loaded.find(&active).is_none() {
        return Check::new(
            "modes",
            Status::Fail,
            format!("general.mode is '{active}', which is not a mode; run /mode or zarn modes list and pick one of {}", loaded.names().join(", ")),
        );
    }
    if let Some(warning) = loaded.warnings.first() {
        return Check::new(
            "modes",
            Status::Warn,
            format!("{warning}; fix or delete that file (zarn modes list shows where each mode comes from)"),
        );
    }
    Check::new("modes", Status::Ok, format!("{} modes, active {active}", loaded.modes.len()))
}

pub fn routing_check(cfg: &Config, store: &KeyStore) -> Check {
    if !cfg.routing.enabled.unwrap_or(false) || cfg.routing.roles.is_empty() {
        return Check::new("routing", Status::Ok, "off; every role uses the main model");
    }
    for (role, spec) in &cfg.routing.roles {
        if !crate::ROLES.contains(&role.as_str()) {
            return Check::new(
                "routing",
                Status::Fail,
                format!("[routing.roles.{role}] is not a role; use one of {} (edit it under /config → Models)", crate::ROLES.join(", ")),
            );
        }
        if let Some(provider) = &spec.provider {
            let Some(provider_spec) = cfg.providers.get(provider) else {
                return Check::new(
                    "routing",
                    Status::Fail,
                    format!("role {role} names provider {provider}, which is not configured; run zarn providers add or change it under /config → Models"),
                );
            };
            if catalog::needs_key(provider_spec)
                && store.has_with(provider, provider_spec.api_key_env.as_deref(), provider_spec.api_key_helper.as_deref()).is_none()
            {
                return Check::new(
                    "routing",
                    Status::Fail,
                    format!("role {role} uses {provider}, which has no key; run zarn login {provider}"),
                );
            }
        }
    }
    Check::new("routing", Status::Ok, format!("{} roles routed", cfg.routing.roles.len()))
}

pub fn discovery_check(dir: &Path) -> Check {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Check::new("discovery cache", Status::Ok, "empty; zarn models fills it");
    };
    let mut good = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let ok = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<crate::quota::Report>(&t).ok())
            .is_some();
        if !ok {
            return Check::new(
                "discovery cache",
                Status::Warn,
                format!("{} is unreadable; run zarn models --refresh to rebuild it", path.display()),
            );
        }
        good += 1;
    }
    Check::new("discovery cache", Status::Ok, format!("{good} providers cached; zarn models --refresh updates them"))
}

pub fn plugins_check(plugins: &[crate::plugins::Plugin]) -> Check {
    if plugins.is_empty() {
        return Check::new("plugins", Status::Ok, "none installed");
    }
    let waiting: Vec<&str> = plugins.iter().filter(|p| p.needs_approval()).map(|p| p.name()).collect();
    if let Some(first) = waiting.first() {
        return Check::new(
            "plugins",
            Status::Warn,
            format!(
                "{} installed; hooks and MCP servers of {} are off until approved: zarn plugin approve {first}",
                plugins.len(),
                waiting.join(", ")
            ),
        );
    }
    Check::new("plugins", Status::Ok, format!("{} installed, code approved where any", plugins.len()))
}

pub fn spend_check(cfg: &Config, store: &KeyStore) -> Check {
    if cfg.limits.is_empty() {
        return Check::new("spend caps", Status::Ok, "none set; /limits or zarn limits sets them");
    }
    let profile = runtime::active_profile(cfg, None);
    let model = runtime::plan(cfg, store, &profile, None).map(|p| p.model).unwrap_or_default();
    let priced = crate::pricing::table(cfg).lookup(&model).is_some();
    match crate::spend::unenforceable(&cfg.limits, &model, priced) {
        Some(note) => Check::new("spend caps", Status::Warn, format!("{note} (zarn limits shows the caps)")),
        None => {
            let session = cfg.limits.session_usd.map(|v| format!("${v:.2} a session")).unwrap_or_default();
            let day = cfg.limits.day_usd.map(|v| format!("${v:.2} a day")).unwrap_or_default();
            let caps: Vec<String> = [session, day].into_iter().filter(|c| !c.is_empty()).collect();
            Check::new("spend caps", Status::Ok, format!("{} for {model}", caps.join(" and ")))
        }
    }
}

pub fn on_path(program: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(program);
        candidate.is_file() || (cfg!(windows) && dir.join(format!("{program}.exe")).is_file())
    })
}

pub fn sandbox_check(cfg: &Config) -> Check {
    let os_tool = if cfg!(target_os = "linux") {
        Some("bwrap")
    } else if cfg!(target_os = "macos") {
        Some("sandbox-exec")
    } else {
        None
    };
    let os = match os_tool {
        Some(tool) if on_path(tool) => format!("{tool} is installed, but zarn does not use it, so no OS sandbox is applied"),
        Some(tool) => format!("no OS sandbox is applied ({tool} is not installed and zarn would not use it yet)"),
        None => "no OS sandbox is available on this platform".to_string(),
    };
    if cfg.general.sandbox() {
        Check::new(
            "sandbox",
            Status::Warn,
            format!(
                "on: bash runs in the project directory with keys and tokens removed from its environment. This is not isolation; commands can still read and write anything your user can. {os}."
            ),
        )
    } else {
        Check::new(
            "sandbox",
            Status::Ok,
            format!(
                "off: bash runs with your full environment. Turn on with /config → General → bash sandbox, or general.sandbox = true. {os}."
            ),
        )
    }
}

pub fn hooks_check(cfg: &Config) -> Check {
    let all = cfg.hooks.all();
    if all.is_empty() {
        return Check::new("hooks", Status::Ok, "none configured");
    }
    let missing: Vec<String> = all
        .iter()
        .filter_map(|(event, hook)| {
            let program = crate::auth::split_command(&hook.command).into_iter().next()?;
            let path = Path::new(&program);
            let found = if path.components().count() > 1 {
                path.exists()
            } else {
                on_path(&program) || matches!(program.as_str(), "sh" | "cd" | "echo" | "exit" | "true" | "false")
            };
            (!found).then(|| format!("{event} hook `{}` runs {program}, which was not found", hook.command))
        })
        .collect();
    if missing.is_empty() {
        Check::new(
            "hooks",
            Status::Ok,
            format!("{} configured, timeout {} s; zarn hooks list shows them", all.len(), cfg.hooks.timeout().as_secs()),
        )
    } else {
        Check::new(
            "hooks",
            Status::Warn,
            format!("{}; fix the command under [hooks] in config.toml", missing.join("; ")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::MemoryBackend;
    use std::sync::Arc;

    #[tokio::test]
    async fn a_fresh_install_fails_the_providers_check_and_says_how_to_fix_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::new(Arc::new(MemoryBackend::new()), dir.path().join("auth.json"));
        let checks = run(&store, dir.path().join("p.toml"), Some(dir.path().join("u.toml")), false).await;
        let providers = checks.iter().find(|c| c.area == "providers").unwrap();
        assert_eq!(providers.status, Status::Fail);
        assert!(providers.detail.contains("providers add"));
    }

    #[tokio::test]
    async fn a_provider_without_a_key_is_reported_with_the_login_command() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("u.toml");
        std::fs::write(&user, "[providers.a]\nkind = \"anthropic\"\n[profiles.default]\nprovider = \"a\"\nmodel = \"m\"\n").unwrap();
        let store = KeyStore::new(Arc::new(MemoryBackend::new()), dir.path().join("auth.json"));
        let checks = run(&store, dir.path().join("p.toml"), Some(user), false).await;
        let provider = checks.iter().find(|c| c.area == "provider a").unwrap();
        assert_eq!(provider.status, Status::Fail);
        assert!(provider.detail.contains("zarn login a"));
        assert!(render(&checks).contains("failed"));
    }

    #[test]
    fn the_sandbox_check_never_claims_isolation() {
        let mut cfg = Config::default();
        let off = sandbox_check(&cfg);
        assert!(off.detail.starts_with("off"));
        cfg.general.sandbox = Some(true);
        let on = sandbox_check(&cfg);
        assert_eq!(on.status, Status::Warn);
        assert!(on.detail.contains("This is not isolation"));
        assert!(on.detail.contains("no OS sandbox"));
    }

    #[tokio::test]
    async fn an_unparseable_file_is_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p.toml");
        std::fs::write(&project, "this is not toml = = =").unwrap();
        let store = KeyStore::new(Arc::new(MemoryBackend::new()), dir.path().join("auth.json"));
        let checks = run(&store, project, None, false).await;
        assert!(checks.iter().any(|c| c.area == "config (project)" && c.status == Status::Fail));
    }
}
