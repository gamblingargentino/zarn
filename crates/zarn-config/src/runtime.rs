use crate::agents::{self, AgentFile};
use crate::auth::KeyStore;
use crate::context::{self, DEFAULT_BUDGET};
use crate::guard::{guarded_chain, redacting};
use crate::{Config, catalog};
use anyhow::{Result, bail};
use std::path::Path;
use std::sync::Arc;
use zarn_core::agent::AgentConfig;
use zarn_core::provider::Provider;
use zarn_core::subagent::{ProviderFactory, TaskTool, ToolFactory};
use zarn_core::tools::{Ctx, Tool, ToolRegistry};
use zarn_providers::ProviderSpec;
use zarn_tools::skill::{Skill, SkillTool};

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub profile: String,
    pub provider_name: String,
    pub model: String,
    pub specs: Vec<ProviderSpec>,
    pub skipped: Vec<String>,
}

pub fn active_profile(cfg: &Config, requested: Option<&str>) -> String {
    requested
        .map(str::to_string)
        .or_else(|| std::env::var("ZARN_PROFILE").ok().filter(|p| !p.is_empty()))
        .unwrap_or_else(|| cfg.default_profile())
}

pub fn plan(
    cfg: &Config,
    store: &KeyStore,
    profile: &str,
    model_override: Option<&str>,
) -> Result<Plan> {
    let Some(spec) = cfg.profiles.get(profile) else {
        bail!("no provider is configured for profile '{profile}'. Run /login or `zarn providers add`.");
    };
    let Some(primary) = spec.provider.clone() else {
        bail!("profile '{profile}' names no provider. Pick one in /config or with /provider.");
    };
    let model = model_override
        .map(str::to_string)
        .or_else(|| spec.model.clone())
        .or_else(|| std::env::var("ZARN_MODEL").ok().filter(|m| !m.is_empty()));
    let Some(model) = model else {
        bail!("profile '{profile}' has no model. Pick one with /model.");
    };

    let chain = cfg.chain_for(store, profile)?;
    let mut names = vec![primary.clone()];
    names.extend(spec.fallback.iter().flatten().cloned());

    let mut specs = Vec::new();
    let mut skipped = Vec::new();
    for (name, resolved) in names.into_iter().zip(chain) {
        let missing = catalog::needs_key(&resolved) && resolved.api_key.is_none();
        if missing && name == primary {
            bail!(
                "provider '{name}' has no API key. Run /login {name} or `zarn login {name}`."
            );
        }
        if missing {
            skipped.push(name);
            continue;
        }
        specs.push(resolved);
    }

    Ok(Plan {
        profile: profile.to_string(),
        provider_name: primary,
        model,
        specs,
        skipped,
    })
}

pub struct Runtime {
    pub plan: Plan,
    pub provider: Arc<dyn Provider>,
    pub tools: Arc<ToolRegistry>,
    pub system: String,
    pub skills: Vec<Skill>,
    pub agents: Vec<AgentFile>,
}

impl Runtime {
    pub fn agent_config(&self) -> AgentConfig {
        AgentConfig {
            model: self.plan.model.clone(),
            ..AgentConfig::default()
        }
    }
}

fn subagent_tools() -> ToolFactory {
    Arc::new(|names: &[String]| {
        let wanted: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
        let mut registry = ToolRegistry::new();
        for tool in zarn_tools::builtin_list() {
            let name = tool.spec().name;
            if wanted.is_empty() || wanted.contains(&name) {
                registry.register(redacting(tool));
            }
        }
        registry
    })
}

pub fn enabled_skills(cfg: &Config, all: Vec<Skill>) -> Vec<Skill> {
    all.into_iter()
        .filter(|s| cfg.skill_enabled(&s.name))
        .collect()
}

pub fn assemble(
    cfg: &Config,
    plan: Plan,
    cwd: &Path,
    ctx: Arc<Ctx>,
    extra_tools: Vec<Box<dyn Tool>>,
) -> Result<Runtime> {
    let provider = guarded_chain(&plan.specs)?;
    let skills = enabled_skills(cfg, context::discover_skills());
    let agents = agents::load();
    let budget = cfg.general.context_budget.unwrap_or(DEFAULT_BUDGET);
    let system = context::system_prompt(cwd, &skills, budget);

    let mut registry = ToolRegistry::new();
    for tool in zarn_tools::builtin_list() {
        registry.register(redacting(tool));
    }
    if !skills.is_empty() {
        registry.register(redacting(Box::new(SkillTool::new(skills.clone()))));
    }
    if !agents.is_empty() {
        let shared = provider.clone();
        let providers: ProviderFactory = Arc::new(move |_model: Option<&str>| shared.clone());
        let task = TaskTool::new(
            agents.iter().map(|a| a.def.clone()).collect(),
            providers,
            subagent_tools(),
            ctx,
            AgentConfig {
                model: plan.model.clone(),
                ..AgentConfig::default()
            },
        );
        registry.register(redacting(Box::new(task)));
    }
    for tool in extra_tools {
        registry.register(redacting(tool));
    }

    Ok(Runtime {
        plan,
        provider,
        tools: Arc::new(registry),
        system,
        skills,
        agents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::MemoryBackend;

    fn cfg(body: &str) -> Config {
        toml::from_str(body).unwrap()
    }

    fn store() -> (tempfile::TempDir, KeyStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::new(Arc::new(MemoryBackend::new()), dir.path().join("auth.json"));
        (dir, store)
    }

    #[test]
    fn a_fresh_install_has_nothing_to_run() {
        let (_d, store) = store();
        assert!(plan(&Config::default(), &store, "default", None).is_err());
    }

    #[test]
    fn a_primary_without_a_key_is_an_error_naming_login() {
        let (_d, store) = store();
        let c = cfg("[providers.a]\nkind = \"anthropic\"\n[profiles.default]\nprovider = \"a\"\nmodel = \"m\"\n");
        let err = plan(&c, &store, "default", None).unwrap_err().to_string();
        assert!(err.contains("login a"));
    }

    #[test]
    fn a_fallback_without_a_key_is_skipped_not_fatal() {
        let (_d, store) = store();
        store.save("a", "test-key-not-real").unwrap();
        let c = cfg("[providers.a]\nkind = \"anthropic\"\n[providers.b]\nkind = \"google\"\n[profiles.default]\nprovider = \"a\"\nmodel = \"m\"\nfallback = [\"b\"]\n");
        let p = plan(&c, &store, "default", None).unwrap();
        assert_eq!(p.specs.len(), 1);
        assert_eq!(p.skipped, vec!["b".to_string()]);
        assert_eq!(p.model, "m");
    }

    #[test]
    fn a_model_override_beats_the_profile() {
        let (_d, store) = store();
        let c = cfg("[providers.local]\nkind = \"ollama\"\n[profiles.default]\nprovider = \"local\"\nmodel = \"m\"\n");
        assert_eq!(plan(&c, &store, "default", Some("other")).unwrap().model, "other");
    }
}
