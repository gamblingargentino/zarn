use crate::{Config, paths};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    Project,
    User,
}

impl Scope {
    pub fn label(&self) -> &'static str {
        match self {
            Scope::Project => "project",
            Scope::User => "user",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Layers {
    pub project_path: PathBuf,
    pub user_path: Option<PathBuf>,
    pub project: Config,
    pub user: Config,
}

fn read_layer(path: &Path) -> Result<Config> {
    if path.exists() {
        Config::load_file(path)
    } else {
        Ok(Config::default())
    }
}

impl Layers {
    pub fn load() -> Result<Self> {
        Self::load_from(paths::project_config(), paths::user_config())
    }

    pub fn load_from(project_path: PathBuf, user_path: Option<PathBuf>) -> Result<Self> {
        let project = read_layer(&project_path)?;
        let user = match &user_path {
            Some(path) => read_layer(path)?,
            None => Config::default(),
        };
        let layers = Layers {
            project_path,
            user_path,
            project,
            user,
        };
        layers.merged().validate()?;
        Ok(layers)
    }

    pub fn merged(&self) -> Config {
        let mut merged = self.project.clone();
        merged.merge(self.user.clone());
        merged
    }

    pub fn layer(&self, scope: Scope) -> &Config {
        match scope {
            Scope::Project => &self.project,
            Scope::User => &self.user,
        }
    }

    pub fn layer_mut(&mut self, scope: Scope) -> &mut Config {
        match scope {
            Scope::Project => &mut self.project,
            Scope::User => &mut self.user,
        }
    }

    pub fn path(&self, scope: Scope) -> Option<&Path> {
        match scope {
            Scope::Project => Some(&self.project_path),
            Scope::User => self.user_path.as_deref(),
        }
    }

    pub fn save(&self, scope: Scope) -> Result<PathBuf> {
        let mut check = self.merged();
        for spec in check.providers.values_mut() {
            spec.api_key = None;
        }
        check.validate()?;
        let Some(path) = self.path(scope) else {
            bail!("cannot determine a home directory for the user config");
        };
        let body = self.layer(scope).to_toml()?;
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(path.to_path_buf())
    }

    fn pick(&self, in_project: bool) -> Scope {
        if in_project { Scope::Project } else { Scope::User }
    }

    pub fn provider_scope(&self, name: &str) -> Scope {
        self.pick(self.project.providers.contains_key(name))
    }

    pub fn profile_scope(&self, name: &str) -> Scope {
        self.pick(self.project.profiles.contains_key(name))
    }

    pub fn permission_scope(&self, tool: &str) -> Scope {
        self.pick(self.project.permissions.contains_key(tool))
    }

    pub fn skill_scope(&self, name: &str) -> Scope {
        self.pick(self.project.skills.contains_key(name))
    }

    pub fn mcp_scope(&self, name: &str) -> Scope {
        self.pick(self.project.mcp.contains_key(name))
    }

    pub fn general_scope(&self, field: GeneralField) -> Scope {
        let g = &self.project.general;
        self.pick(match field {
            GeneralField::DefaultProfile => g.default_profile.is_some(),
            GeneralField::Accent => g.accent.is_some(),
            GeneralField::Banner => g.banner.is_some(),
            GeneralField::ShowCost => g.show_cost.is_some(),
            GeneralField::ContextBudget => g.context_budget.is_some(),
            GeneralField::AutoCompact => g.auto_compact.is_some(),
            GeneralField::Mode => g.mode.is_some(),
        })
    }

    pub fn limits_scope(&self, day: bool) -> Scope {
        let l = &self.project.limits;
        self.pick(if day { l.day_usd.is_some() } else { l.session_usd.is_some() })
    }

    pub fn routing_scope(&self) -> Scope {
        self.pick(self.project.routing.enabled.is_some())
    }

    pub fn role_scope(&self, role: &str) -> Scope {
        self.pick(self.project.routing.roles.contains_key(role))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeneralField {
    DefaultProfile,
    Accent,
    Banner,
    ShowCost,
    ContextBudget,
    AutoCompact,
    Mode,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layers(dir: &Path, project: &str, user: &str) -> Layers {
        let p = dir.join("project.toml");
        let u = dir.join("user.toml");
        std::fs::write(&p, project).unwrap();
        std::fs::write(&u, user).unwrap();
        Layers::load_from(p, Some(u)).unwrap()
    }

    #[test]
    fn the_project_layer_wins_and_the_user_layer_fills_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let l = layers(
            dir.path(),
            "[general]\naccent = \"cyan\"\n",
            "[general]\naccent = \"red\"\nbanner = false\n",
        );
        let merged = l.merged();
        assert_eq!(merged.general.accent.as_deref(), Some("cyan"));
        assert_eq!(merged.general.banner, Some(false));
    }

    #[test]
    fn an_item_is_written_back_to_the_file_that_defines_it() {
        let dir = tempfile::tempdir().unwrap();
        let l = layers(
            dir.path(),
            "[permissions]\nbash = \"deny\"\n",
            "[permissions]\nread = \"allow\"\n",
        );
        assert_eq!(l.permission_scope("bash"), Scope::Project);
        assert_eq!(l.permission_scope("read"), Scope::User);
        assert_eq!(l.permission_scope("write"), Scope::User);
        assert_eq!(l.general_scope(GeneralField::Accent), Scope::User);
    }

    #[test]
    fn saving_round_trips_and_never_writes_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = layers(dir.path(), "", "[providers.a]\nkind = \"openai\"\n");
        l.user.providers.get_mut("a").unwrap().api_key = Some("test-key-not-real".into());
        let path = l.save(Scope::User).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("test-key-not-real"));
        assert!(!body.contains("api_key"));
        let again = Layers::load_from(dir.path().join("project.toml"), Some(path)).unwrap();
        assert_eq!(again.user.providers["a"].kind, "openai");
    }

    #[test]
    fn a_save_that_would_leave_a_dangling_profile_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = layers(
            dir.path(),
            "",
            "[providers.a]\nkind = \"openai\"\n\n[profiles.default]\nprovider = \"a\"\n",
        );
        l.user.providers.clear();
        assert!(l.save(Scope::User).is_err());
    }
}
