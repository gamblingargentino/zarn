use crate::{Config, paths};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    Local,
    Project,
    User,
}

pub const LOCAL_FILE: &str = "config.local.toml";

impl Scope {
    pub const ALL: [Scope; 3] = [Scope::Local, Scope::Project, Scope::User];

    pub fn label(&self) -> &'static str {
        match self {
            Scope::Local => "local",
            Scope::Project => "project",
            Scope::User => "user",
        }
    }

    pub fn parse(text: &str) -> Option<Scope> {
        match text.trim() {
            "local" | "me" | "just for me" => Some(Scope::Local),
            "project" => Some(Scope::Project),
            "user" | "" => Some(Scope::User),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Layers {
    pub local_path: PathBuf,
    pub project_path: PathBuf,
    pub user_path: Option<PathBuf>,
    pub local: Config,
    pub project: Config,
    pub user: Config,
}

pub fn local_beside(project_path: &Path) -> PathBuf {
    project_path
        .parent()
        .map(|d| d.join(LOCAL_FILE))
        .unwrap_or_else(|| PathBuf::from(LOCAL_FILE))
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
        let local_path = local_beside(&project_path);
        let local = read_layer(&local_path)?;
        let project = read_layer(&project_path)?;
        let user = match &user_path {
            Some(path) => read_layer(path)?,
            None => Config::default(),
        };
        let layers = Layers {
            local_path,
            project_path,
            user_path,
            local,
            project,
            user,
        };
        layers.merged().validate()?;
        Ok(layers)
    }

    pub fn merged(&self) -> Config {
        let mut merged = self.local.clone();
        merged.merge(self.project.clone());
        merged.merge(self.user.clone());
        crate::plugins::contribute(&mut merged);
        merged
    }

    pub fn layer(&self, scope: Scope) -> &Config {
        match scope {
            Scope::Local => &self.local,
            Scope::Project => &self.project,
            Scope::User => &self.user,
        }
    }

    pub fn layer_mut(&mut self, scope: Scope) -> &mut Config {
        match scope {
            Scope::Local => &mut self.local,
            Scope::Project => &mut self.project,
            Scope::User => &mut self.user,
        }
    }

    pub fn path(&self, scope: Scope) -> Option<&Path> {
        match scope {
            Scope::Local => Some(&self.local_path),
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

    pub fn find(&self, has: impl Fn(&Config) -> bool) -> Scope {
        if has(&self.local) {
            Scope::Local
        } else if has(&self.project) {
            Scope::Project
        } else {
            Scope::User
        }
    }

    pub fn provider_scope(&self, name: &str) -> Scope {
        self.find(|c| c.providers.contains_key(name))
    }

    pub fn profile_scope(&self, name: &str) -> Scope {
        self.find(|c| c.profiles.contains_key(name))
    }

    pub fn permission_scope(&self, tool: &str) -> Scope {
        self.find(|c| c.permissions.contains_key(tool))
    }

    pub fn skill_scope(&self, name: &str) -> Scope {
        self.find(|c| c.skills.contains_key(name))
    }

    pub fn mcp_scope(&self, name: &str) -> Scope {
        self.find(|c| c.mcp.contains_key(name))
    }

    pub fn general_scope(&self, field: GeneralField) -> Scope {
        self.find(|c| {
            let g = &c.general;
            match field {
            GeneralField::DefaultProfile => g.default_profile.is_some(),
            GeneralField::Accent => g.accent.is_some(),
            GeneralField::Banner => g.banner.is_some(),
            GeneralField::ShowCost => g.show_cost.is_some(),
            GeneralField::ContextBudget => g.context_budget.is_some(),
            GeneralField::AutoCompact => g.auto_compact.is_some(),
            GeneralField::Mode => g.mode.is_some(),
            GeneralField::ReadOnly => g.auto_allow_read_only.is_some(),
            GeneralField::Sandbox => g.sandbox.is_some(),
            GeneralField::Vim => g.vim.is_some(),
            GeneralField::Theme => g.theme.is_some(),
            GeneralField::Notifications => g.notifications.is_some(),
            GeneralField::StatusLine => g.status_line.is_some(),
            GeneralField::Mouse => g.mouse.is_some(),
            }
        })
    }

    pub fn limits_scope(&self, day: bool) -> Scope {
        self.find(|c| if day { c.limits.day_usd.is_some() } else { c.limits.session_usd.is_some() })
    }

    pub fn routing_scope(&self) -> Scope {
        self.find(|c| c.routing.enabled.is_some())
    }

    pub fn role_scope(&self, role: &str) -> Scope {
        self.find(|c| c.routing.roles.contains_key(role))
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
    ReadOnly,
    Sandbox,
    Vim,
    Theme,
    Notifications,
    StatusLine,
    Mouse,
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

    #[test]
    fn the_local_layer_sits_above_project_and_takes_its_own_writes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LOCAL_FILE), "[general]\naccent = \"red\"\n").unwrap();
        let mut l = layers(dir.path(), "[general]\naccent = \"cyan\"\nbanner = false\n", "");
        assert_eq!(l.local_path, dir.path().join(LOCAL_FILE));
        let merged = l.merged();
        assert_eq!(merged.general.accent.as_deref(), Some("red"));
        assert_eq!(merged.general.banner, Some(false));
        assert_eq!(l.general_scope(GeneralField::Accent), Scope::Local);
        assert_eq!(l.general_scope(GeneralField::Banner), Scope::Project);
        l.layer_mut(Scope::Local).general.vim = Some(true);
        l.save(Scope::Local).unwrap();
        let local = std::fs::read_to_string(dir.path().join(LOCAL_FILE)).unwrap();
        assert!(local.contains("vim = true"));
        assert!(!std::fs::read_to_string(dir.path().join("project.toml")).unwrap().contains("vim"));
        assert_eq!(Scope::parse("just for me"), Some(Scope::Local));
        assert_eq!(Scope::parse("nowhere"), None);
    }
}
