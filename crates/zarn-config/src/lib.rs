pub mod agents;
pub mod auth;
pub mod catalog;
pub mod context;
pub mod doctor;
pub mod guard;
pub mod health;
pub mod hooks;
pub mod layers;
pub mod modes;
pub mod ops;
pub mod paths;
pub mod pricing;
pub mod quota;
pub mod runtime;
pub mod spend;
pub mod titles;

use anyhow::{Context, Result, bail};
use auth::{KeySource, KeyStore};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use zarn_core::cost::Rates;
use zarn_providers::ProviderSpec;
use zarn_tools::mcp::ServerConfig;
use zarn_tools::permissions::{Policy, Rules};

pub const LOCAL_PATH: &str = ".zarn/config.toml";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ProfileSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct General {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_cost: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_budget: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_cache_minutes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_commands: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_allow_read_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vim: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notifications: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_line: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouse: Option<bool>,
}

impl General {
    fn fill_from(&mut self, other: General) {
        self.default_profile = self.default_profile.take().or(other.default_profile);
        self.accent = self.accent.take().or(other.accent);
        self.banner = self.banner.or(other.banner);
        self.show_cost = self.show_cost.or(other.show_cost);
        self.context_budget = self.context_budget.or(other.context_budget);
        self.auto_compact = self.auto_compact.or(other.auto_compact);
        self.mode = self.mode.take().or(other.mode);
        self.discovery_cache_minutes = self.discovery_cache_minutes.or(other.discovery_cache_minutes);
        self.read_only_commands = self.read_only_commands.take().or(other.read_only_commands);
        self.auto_allow_read_only = self.auto_allow_read_only.or(other.auto_allow_read_only);
        self.sandbox = self.sandbox.or(other.sandbox);
        self.vim = self.vim.or(other.vim);
        self.theme = self.theme.take().or(other.theme);
        self.notifications = self.notifications.or(other.notifications);
        self.status_line = self.status_line.take().or(other.status_line);
        self.mouse = self.mouse.or(other.mouse);
    }

    pub fn vim(&self) -> bool {
        self.vim.unwrap_or(false)
    }

    pub fn sandbox(&self) -> bool {
        self.sandbox.unwrap_or(false)
    }

    pub fn notifications(&self) -> bool {
        self.notifications.unwrap_or(true)
    }

    pub fn mouse(&self) -> bool {
        self.mouse.unwrap_or(false)
    }

    pub fn tool_options(&self) -> zarn_tools::Options {
        zarn_tools::Options {
            sandbox: self.sandbox(),
        }
    }

    pub fn safe_commands(&self) -> Option<Vec<String>> {
        if !self.auto_allow_read_only.unwrap_or(true) {
            return None;
        }
        Some(
            self.read_only_commands
                .clone()
                .unwrap_or_else(zarn_tools::safe_shell::defaults),
        )
    }

    pub fn is_empty(&self) -> bool {
        *self == General::default()
    }

    pub fn banner(&self) -> bool {
        self.banner.unwrap_or(true)
    }

    pub fn show_cost(&self) -> bool {
        self.show_cost.unwrap_or(true)
    }

    pub fn auto_compact(&self) -> bool {
        self.auto_compact.unwrap_or(true)
    }

    pub fn mode(&self) -> String {
        self.mode.clone().unwrap_or_else(|| "default".to_string())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PermissionTable {
    #[serde(flatten)]
    pub rules: Rules,
    #[serde(flatten)]
    pub tools: BTreeMap<String, Policy>,
}

impl PermissionTable {
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty() && self.rules.is_empty()
    }
}

impl std::ops::Deref for PermissionTable {
    type Target = BTreeMap<String, Policy>;

    fn deref(&self) -> &Self::Target {
        &self.tools
    }
}

impl std::ops::DerefMut for PermissionTable {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.tools
    }
}

pub const ROLES: [&str; 8] = [
    "main", "subagent", "explore", "plan", "compact", "title", "init", "review",
];

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct RoleSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Routing {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roles: BTreeMap<String, RoleSpec>,
}

impl Routing {
    pub fn is_empty(&self) -> bool {
        *self == Routing::default()
    }

    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(false)
    }

    fn fill_from(&mut self, other: Routing) {
        self.enabled = self.enabled.or(other.enabled);
        for (role, spec) in other.roles {
            let entry = self.roles.entry(role).or_default();
            entry.provider = entry.provider.take().or(spec.provider);
            entry.model = entry.model.take().or(spec.model);
        }
    }
}

impl ProfileSpec {
    fn fill_from(&mut self, other: ProfileSpec) {
        self.provider = self.provider.take().or(other.provider);
        self.model = self.model.take().or(other.model);
        self.fallback = self.fallback.take().or(other.fallback);
    }
}

fn fill_provider(into: &mut ProviderSpec, from: ProviderSpec) {
    if into.kind.is_empty() {
        into.kind = from.kind;
    }
    into.base_url = into.base_url.take().or(from.base_url);
    into.api_key = into.api_key.take().or(from.api_key);
    into.api_key_env = into.api_key_env.take().or(from.api_key_env);
    into.api_key_helper = into.api_key_helper.take().or(from.api_key_helper);
    into.context_window = into.context_window.or(from.context_window);
    into.max_output = into.max_output.or(from.max_output);
    into.vision = into.vision.or(from.vision);
    into.tools = into.tools.take().or(from.tools);
    for (key, value) in from.extra {
        into.extra.entry(key).or_insert(value);
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default, skip_serializing_if = "General::is_empty")]
    pub general: General,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: BTreeMap<String, ProviderSpec>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, ProfileSpec>,
    #[serde(default, skip_serializing_if = "PermissionTable::is_empty")]
    pub permissions: PermissionTable,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub skills: BTreeMap<String, bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp: BTreeMap<String, ServerConfig>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pricing: BTreeMap<String, Rates>,
    #[serde(default, skip_serializing_if = "Routing::is_empty")]
    pub routing: Routing,
    #[serde(default, skip_serializing_if = "spend::Limits::is_empty")]
    pub limits: spend::Limits,
    #[serde(default, skip_serializing_if = "hooks::Hooks::is_empty")]
    pub hooks: hooks::Hooks,
}

impl Config {
    pub fn merge(&mut self, other: Config) {
        self.general.fill_from(other.general);
        for (name, enabled) in other.skills {
            self.skills.entry(name).or_insert(enabled);
        }
        for (name, spec) in other.providers {
            match self.providers.get_mut(&name) {
                Some(existing) => fill_provider(existing, spec),
                None => {
                    self.providers.insert(name, spec);
                }
            }
        }
        for (name, spec) in other.profiles {
            match self.profiles.get_mut(&name) {
                Some(existing) => existing.fill_from(spec),
                None => {
                    self.profiles.insert(name, spec);
                }
            }
        }
        for (name, policy) in other.permissions.tools {
            self.permissions.entry(name).or_insert(policy);
        }
        self.permissions.rules.extend(&other.permissions.rules);
        for (name, server) in other.mcp {
            self.mcp.entry(name).or_insert(server);
        }
        for (prefix, rates) in other.pricing {
            self.pricing.entry(prefix).or_insert(rates);
        }
        self.routing.fill_from(other.routing);
        self.limits.fill_from(other.limits);
        self.hooks.fill_from(other.hooks);
    }

    pub fn home_path() -> Option<PathBuf> {
        paths::user_config()
    }

    pub fn sources() -> Vec<PathBuf> {
        let mut paths = vec![PathBuf::from(LOCAL_PATH)];
        if let Some(home) = Self::home_path() {
            paths.push(home);
        }
        paths
    }

    pub fn load() -> Result<Self> {
        Self::load_from(&Self::sources())
    }

    pub fn load_from(paths: &[PathBuf]) -> Result<Self> {
        let mut config = Config::default();
        for path in paths {
            if path.exists() {
                config.merge(Self::load_file(path)?);
            }
        }
        config.validate()?;
        Ok(config)
    }

    pub fn load_file(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("reading config from {}", path.display()))?;
        let parsed: Config = toml::from_str(&contents)
            .with_context(|| format!("invalid TOML in {}", path.display()))?;
        Ok(parsed)
    }

    pub fn validate(&self) -> Result<()> {
        for (name, spec) in &self.providers {
            if spec.api_key.is_some() {
                bail!(
                    "provider '{name}' sets api_key inline. Secrets never belong in a config file: delete that line and run `zarn login {name}` to keep the key in the OS credential store, or set api_key_env = \"SOME_ENV_VAR\" naming the variable that holds it."
                );
            }
        }
        spend::validate(self)?;
        self.hooks.validate()?;
        for (_, rule) in self.permissions.rules.list() {
            zarn_tools::permissions::parse_rule(&rule).map_err(anyhow::Error::msg)?;
        }
        for (role, spec) in &self.routing.roles {
            if !ROLES.contains(&role.as_str()) {
                bail!(
                    "routing role '{role}' is not a role zarn knows; use one of {}",
                    ROLES.join(", ")
                );
            }
            if let Some(provider) = &spec.provider
                && !self.providers.contains_key(provider)
            {
                bail!(
                    "routing role '{role}' names provider '{provider}', which is not configured"
                );
            }
        }
        if let Some(mode) = &self.general.mode
            && !catalog::valid_name(mode)
        {
            bail!("general.mode '{mode}' is not a valid mode name");
        }
        for (prefix, rates) in &self.pricing {
            if [rates.input, rates.output, rates.cached]
                .iter()
                .any(|v| !v.is_finite() || *v < 0.0)
            {
                bail!("pricing for '{prefix}' has a negative or invalid rate");
            }
        }
        for (name, profile) in &self.profiles {
            if let Some(provider) = &profile.provider
                && !self.providers.contains_key(provider)
            {
                bail!("profile '{name}' names provider '{provider}', which is not configured");
            }
            for fallback in profile.fallback.iter().flatten() {
                if !self.providers.contains_key(fallback) {
                    bail!(
                        "profile '{name}' falls back to provider '{fallback}', which is not configured"
                    );
                }
            }
        }
        Ok(())
    }

    pub fn resolve_key(&self, store: &KeyStore, name: &str) -> Option<(String, KeySource)> {
        let spec = self.providers.get(name)?;
        store.resolve_with(name, spec.api_key_env.as_deref(), spec.api_key_helper.as_deref())
    }

    pub fn chain_for(&self, store: &KeyStore, profile: &str) -> Result<Vec<ProviderSpec>> {
        let Some(spec) = self.profiles.get(profile) else {
            bail!("no profile named '{profile}'");
        };

        let mut names = Vec::new();
        if let Some(provider) = &spec.provider {
            names.push(provider.clone());
        }
        names.extend(spec.fallback.iter().flatten().cloned());
        if names.is_empty() {
            bail!("profile '{profile}' names no provider");
        }

        names
            .iter()
            .map(|name| {
                let mut resolved = self
                    .providers
                    .get(name)
                    .cloned()
                    .with_context(|| format!("provider '{name}' is not configured"))?;
                resolved.api_key = self.resolve_key(store, name).map(|(key, _)| key);
                Ok(resolved)
            })
            .collect()
    }

    pub fn apply_permissions(
        &self,
        gate: &zarn_tools::permissions::Permissions,
        cwd: &Path,
    ) -> Vec<String> {
        gate.set_policies(self.permissions.tools.clone());
        gate.set_cwd(cwd);
        gate.set_safe_commands(self.general.safe_commands());
        gate.set_rules(&self.permissions.rules)
    }

    pub fn skill_enabled(&self, name: &str) -> bool {
        self.skills.get(name).copied().unwrap_or(true)
    }

    pub fn default_profile(&self) -> String {
        self.general
            .default_profile
            .clone()
            .unwrap_or_else(|| "default".to_string())
    }

    pub fn has_provider(&self) -> bool {
        !self.providers.is_empty()
    }

    pub fn to_toml(&self) -> Result<String> {
        let mut clean = self.clone();
        for spec in clean.providers.values_mut() {
            spec.api_key = None;
        }
        Ok(toml::to_string_pretty(&clean)?)
    }

    pub fn starter() -> &'static str {
        STARTER
    }
}

const STARTER: &str = r#"[permissions]
read = "allow"
glob = "allow"
grep = "allow"
edit = "ask"
write = "ask"
bash = "ask"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        path
    }

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zarn-config-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_inline_api_key_is_always_rejected_and_says_why() {
        let dir = temp("inline");
        let path = write(
            &dir,
            "config.toml",
            "[providers.openai]\nkind = \"openai\"\napi_key = \"test-key-not-real\"\n",
        );
        let err = Config::load_from(&[path]).unwrap_err().to_string();
        assert!(err.contains("api_key_env"));
        assert!(err.contains("openai"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn an_inline_key_is_rejected_even_alongside_api_key_env() {
        let dir = temp("both");
        let path = write(
            &dir,
            "config.toml",
            "[providers.openai]\nkind = \"openai\"\napi_key = \"test-key-not-real\"\napi_key_env = \"OPENAI_API_KEY\"\n",
        );
        assert!(Config::load_from(&[path]).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_later_source_fills_missing_fields_of_a_section_it_shares() {
        let dir = temp("fill");
        let local = write(
            &dir,
            "local.toml",
            "[profiles.default]\nprovider = \"a\"\n\n[providers.a]\nkind = \"openai\"\n",
        );
        let home = write(
            &dir,
            "home.toml",
            "[profiles.default]\nprovider = \"b\"\nmodel = \"from-home\"\nfallback = [\"a\"]\n\n[providers.b]\nkind = \"anthropic\"\n",
        );

        let config = Config::load_from(&[local, home]).unwrap();
        let profile = &config.profiles["default"];

        assert_eq!(profile.provider.as_deref(), Some("a"));
        assert_eq!(profile.model.as_deref(), Some("from-home"));
        assert_eq!(profile.fallback.as_deref(), Some(&["a".to_string()][..]));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_later_source_fills_missing_fields_of_a_shared_provider() {
        let dir = temp("provider");
        let local = write(&dir, "local.toml", "[providers.openai]\nkind = \"openai\"\n");
        let home = write(
            &dir,
            "home.toml",
            "[providers.openai]\nkind = \"openai\"\napi_key_env = \"OPENAI_API_KEY\"\ncontext_window = 64000\n",
        );

        let config = Config::load_from(&[local, home]).unwrap();
        let spec = &config.providers["openai"];
        assert_eq!(spec.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
        assert_eq!(spec.context_window, Some(64000));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_profile_naming_an_unconfigured_provider_is_rejected() {
        let dir = temp("dangling");
        let path = write(&dir, "config.toml", "[profiles.default]\nprovider = \"ghost\"\n");
        let err = Config::load_from(&[path]).unwrap_err().to_string();
        assert!(err.contains("ghost"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_chain_is_the_provider_then_its_fallbacks_in_order() {
        let dir = temp("chain");
        let path = write(
            &dir,
            "config.toml",
            "[providers.a]\nkind = \"openai\"\n\n[providers.b]\nkind = \"anthropic\"\n\n[profiles.default]\nprovider = \"a\"\nfallback = [\"b\"]\n",
        );
        let config = Config::load_from(&[path]).unwrap();
        let store = KeyStore::new(std::sync::Arc::new(auth::MemoryBackend::new()), dir.join("auth.json"));
        let chain = config.chain_for(&store, "default").unwrap();

        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].kind, "openai");
        assert_eq!(chain[1].kind, "anthropic");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_starter_config_is_valid_and_ships_no_provider() {
        let parsed: Config = toml::from_str(Config::starter()).unwrap();
        assert!(parsed.validate().is_ok());
        assert!(parsed.providers.is_empty());
    }

    #[test]
    fn permission_rules_and_tool_policies_share_one_table() {
        let cfg: Config = toml::from_str(
            "[permissions]\nbash = \"ask\"\nread = \"allow\"\nallow = [\"bash(git diff:*)\", \"edit(src/**)\"]\ndeny = [\"bash(rm -rf:*)\"]\n",
        )
        .unwrap();
        assert_eq!(cfg.permissions.get("bash"), Some(&Policy::Ask));
        assert_eq!(cfg.permissions.rules.allow.len(), 2);
        assert_eq!(cfg.permissions.rules.deny, vec!["bash(rm -rf:*)"]);
        assert!(!cfg.permissions.contains_key("allow"));
        let text = cfg.to_toml().unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.permissions, cfg.permissions);
        let bad: Config = toml::from_str("[permissions]\ndeny = [\"bash(oops\"]\n").unwrap();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let config = Config::load_from(&[PathBuf::from("definitely/not/here.toml")]).unwrap();
        assert!(config.providers.is_empty());
    }
}
