use crate::agents::{self, AgentFile};
use crate::auth::KeyStore;
use crate::context::{self, DEFAULT_BUDGET};
use crate::guard::{guarded_named, redacting};
use crate::{Config, catalog};
use anyhow::{Result, bail};
use std::path::Path;
use std::sync::Arc;
use zarn_core::agent::AgentConfig;
use zarn_core::provider::Provider;
use zarn_core::subagent::{ProviderFactory, TaskTool, ToolFactory};
use zarn_core::tools::{Ctx, Tool, ToolRegistry};
use zarn_providers::{PinnedModel, ProviderSpec, Switch, SwitchSink};
use zarn_tools::skill::{Skill, SkillTool};

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub profile: String,
    pub provider_name: String,
    pub model: String,
    pub names: Vec<String>,
    pub specs: Vec<ProviderSpec>,
    pub skipped: Vec<String>,
}

impl Plan {
    pub fn entries(&self) -> Vec<(String, ProviderSpec)> {
        self.names.iter().cloned().zip(self.specs.iter().cloned()).collect()
    }
}

pub type Notify = Arc<dyn Fn(String) + Send + Sync>;

pub fn switch_sink(notify: &Option<Notify>) -> Option<SwitchSink> {
    let notify = notify.clone()?;
    Some(Arc::new(move |switch: &Switch| notify(switch.notice())))
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
    let mut kept = Vec::new();
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
        kept.push(name);
        specs.push(resolved);
    }

    Ok(Plan {
        profile: profile.to_string(),
        provider_name: primary,
        model,
        names: kept,
        specs,
        skipped,
    })
}

pub fn resolve_model(cfg: &Config, requested: &str) -> Option<(Vec<String>, String)> {
    if let Some((provider, model)) = requested.split_once(':')
        && cfg.providers.contains_key(provider)
        && !model.is_empty()
    {
        return Some((vec![provider.to_string()], model.to_string()));
    }
    let chain = |p: &crate::ProfileSpec| {
        let mut names: Vec<String> = p.provider.iter().cloned().collect();
        names.extend(p.fallback.iter().flatten().cloned());
        names
    };
    if let Some(profile) = cfg.profiles.get(requested)
        && profile.provider.is_some()
        && let Some(model) = &profile.model
    {
        return Some((chain(profile), model.clone()));
    }
    cfg.profiles
        .values()
        .find(|p| p.provider.is_some() && p.model.as_deref() == Some(requested))
        .map(|p| (chain(p), requested.to_string()))
}

#[derive(Clone)]
pub struct Providers {
    pub cfg: Config,
    pub store: Arc<KeyStore>,
    pub main: Arc<dyn Provider>,
    pub main_model: String,
    pub notify: Option<Notify>,
}

impl Providers {
    fn say(&self, text: String) {
        if let Some(notify) = &self.notify {
            notify(text);
        }
    }

    pub fn chain(&self, names: &[String]) -> Result<Arc<dyn Provider>, String> {
        let mut entries = Vec::new();
        let mut missing = Vec::new();
        for name in names {
            let Ok(spec) = crate::ops::spec_with_key(&self.cfg, &self.store, name) else {
                missing.push(format!("{name} is not configured"));
                continue;
            };
            if catalog::needs_key(&spec) && spec.api_key.is_none() {
                missing.push(format!("{name} has no API key"));
                continue;
            }
            entries.push((name.clone(), spec));
        }
        if entries.is_empty() {
            return Err(missing.join(", "));
        }
        guarded_named(&entries, switch_sink(&self.notify)).map_err(|e| e.to_string())
    }

    pub fn for_model(&self, requested: Option<&str>) -> (Arc<dyn Provider>, String) {
        let Some(requested) = requested.filter(|m| !m.trim().is_empty()) else {
            return (self.main.clone(), self.main_model.clone());
        };
        match resolve_model(&self.cfg, requested) {
            None => (self.main.clone(), requested.to_string()),
            Some((names, model)) => match self.chain(&names) {
                Ok(provider) => (provider, model),
                Err(why) => {
                    self.say(format!(
                        "{requested}: {why}, answered by the main model {}",
                        self.main_model
                    ));
                    (self.main.clone(), self.main_model.clone())
                }
            },
        }
    }

    pub fn factory(&self) -> ProviderFactory {
        let me = self.clone();
        Arc::new(move |model: Option<&str>| {
            let (provider, model) = me.for_model(model);
            PinnedModel::wrap(provider, model)
        })
    }
}

pub struct Runtime {
    pub plan: Plan,
    pub provider: Arc<dyn Provider>,
    pub tools: Arc<ToolRegistry>,
    pub system: String,
    pub parts: context::SystemParts,
    pub skills: Vec<Skill>,
    pub agents: Vec<AgentFile>,
    pub providers: Providers,
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

pub struct Assembly {
    pub store: Arc<KeyStore>,
    pub notify: Option<Notify>,
}

pub fn assemble(
    cfg: &Config,
    plan: Plan,
    cwd: &Path,
    ctx: Arc<Ctx>,
    extra_tools: Vec<Box<dyn Tool>>,
    assembly: Assembly,
) -> Result<Runtime> {
    let provider = guarded_named(&plan.entries(), switch_sink(&assembly.notify))?;
    let providers = Providers {
        cfg: cfg.clone(),
        store: assembly.store.clone(),
        main: provider.clone(),
        main_model: plan.model.clone(),
        notify: assembly.notify.clone(),
    };
    let skills = enabled_skills(cfg, context::discover_skills());
    let agents = agents::load();
    let budget = cfg.general.context_budget.unwrap_or(DEFAULT_BUDGET);
    let parts = context::system_parts(cwd, &skills, budget);
    let system = parts.join();

    let mut registry = ToolRegistry::new();
    for tool in zarn_tools::builtin_list() {
        registry.register(redacting(tool));
    }
    if !skills.is_empty() {
        registry.register(redacting(Box::new(SkillTool::new(skills.clone()))));
    }
    if !agents.is_empty() {
        let task = TaskTool::new(
            agents.iter().map(|a| a.def.clone()).collect(),
            providers.factory(),
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
        parts,
        skills,
        agents,
        providers,
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
