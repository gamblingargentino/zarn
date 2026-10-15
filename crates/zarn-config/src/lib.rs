use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use zarn_providers::ProviderSpec;
use zarn_tools::permissions::Policy;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProfileSpec {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub fallback: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub providers: HashMap<String, ProviderSpec>,
    pub profiles: HashMap<String, ProfileSpec>,
    pub permissions: HashMap<String, Policy>,
}

impl Config {
    pub fn merge(&mut self, other: Config) {
        for (k, v) in other.providers {
            self.providers.entry(k).or_insert(v);
        }
        for (k, v) in other.profiles {
            self.profiles.entry(k).or_insert(v);
        }
        for (k, v) in other.permissions {
            self.permissions.entry(k).or_insert(v);
        }
    }

    pub fn load() -> anyhow::Result<Self> {
        let mut config = Config::default();

        let local_config = PathBuf::from(".zarn/config.toml");
        if local_config.exists() {
            let loaded = Self::load_file(&local_config)?;
            config.merge(loaded);
        }

        let home = dirs::home_dir().context("cannot determine home directory")?;
        let home_config = home.join(".config/zarn/config.toml");
        if home_config.exists() {
            let loaded = Self::load_file(&home_config)?;
            config.merge(loaded);
        }

        Ok(config)
    }

    fn load_file(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config from {}", path.display()))?;
        toml::from_str(&contents)
            .with_context(|| format!("invalid TOML in {}", path.display()))
    }
}
