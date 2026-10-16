use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use zarn_providers::ProviderSpec;
use zarn_tools::permissions::Policy;

pub const LOCAL_PATH: &str = ".zarn/config.toml";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ProfileSpec {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub fallback: Option<Vec<String>>,
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
    #[serde(default)]
    pub providers: HashMap<String, ProviderSpec>,
    #[serde(default)]
    pub profiles: HashMap<String, ProfileSpec>,
    #[serde(default)]
    pub permissions: HashMap<String, Policy>,
}

impl Config {
    pub fn merge(&mut self, other: Config) {
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
        for (name, policy) in other.permissions {
            self.permissions.entry(name).or_insert(policy);
        }
    }

    pub fn home_path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".config/zarn/config.toml"))
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
                    "provider '{name}' sets api_key inline. Secrets never belong in a config file: delete that line and set api_key_env = \"SOME_ENV_VAR\" instead, naming the variable that holds the key."
                );
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

    pub fn resolve_key(&self, name: &str) -> Result<Option<String>> {
        let Some(spec) = self.providers.get(name) else {
            return Ok(None);
        };
        let Some(var) = &spec.api_key_env else {
            return Ok(None);
        };
        match std::env::var(var) {
            Ok(key) => Ok(Some(key)),
            Err(_) => bail!(
                "provider '{name}' reads its key from {var}, but that environment variable is not set"
            ),
        }
    }

    pub fn chain_for(&self, profile: &str) -> Result<Vec<ProviderSpec>> {
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
                resolved.api_key = self.resolve_key(name)?;
                Ok(resolved)
            })
            .collect()
    }

    pub fn starter() -> &'static str {
        STARTER
    }
}

const STARTER: &str = r#"[providers.openai]
kind = "openai"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"

[providers.anthropic]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[providers.local]
kind = "ollama"
base_url = "http://localhost:11434"

[profiles.default]
provider = "anthropic"
model = "claude-sonnet-5"
fallback = ["openai"]

[permissions]
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
            "[providers.openai]\nkind = \"openai\"\napi_key = \"sk-secret\"\n",
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
            "[providers.openai]\nkind = \"openai\"\napi_key = \"sk-secret\"\napi_key_env = \"OPENAI_API_KEY\"\n",
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
    fn a_missing_env_var_is_reported_by_name() {
        let mut config = Config::default();
        config.providers.insert(
            "openai".into(),
            ProviderSpec {
                kind: "openai".into(),
                api_key_env: Some("ZARN_TEST_DEFINITELY_UNSET".into()),
                ..ProviderSpec::default()
            },
        );
        let err = config.resolve_key("openai").unwrap_err().to_string();
        assert!(err.contains("ZARN_TEST_DEFINITELY_UNSET"));
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
        let chain = config.chain_for("default").unwrap();

        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].kind, "openai");
        assert_eq!(chain[1].kind, "anthropic");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_starter_config_is_valid_and_names_no_inline_keys() {
        let parsed: Config = toml::from_str(Config::starter()).unwrap();
        assert!(parsed.validate().is_ok());
        assert!(parsed.providers.values().all(|p| p.api_key.is_none()));
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let config = Config::load_from(&[PathBuf::from("definitely/not/here.toml")]).unwrap();
        assert!(config.providers.is_empty());
    }
}
