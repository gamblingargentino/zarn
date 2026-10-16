use crate::hooks::Hooks;
use crate::paths;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use zarn_tools::mcp::ServerConfig;

pub const MANIFEST: &str = "plugin.toml";
pub const STATE: &str = "plugins.toml";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modes: Option<String>,
    #[serde(default, skip_serializing_if = "Hooks::is_empty")]
    pub hooks: Hooks,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp: BTreeMap<String, ServerConfig>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plugin {
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub entry: Entry,
}

impl Plugin {
    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    fn sub(&self, configured: &Option<String>, default: &str) -> Option<PathBuf> {
        let dir = self.dir.join(configured.as_deref().unwrap_or(default));
        dir.starts_with(&self.dir).then_some(dir).filter(|d| d.is_dir())
    }

    pub fn commands_dir(&self) -> Option<PathBuf> {
        self.sub(&self.manifest.commands, "commands")
    }

    pub fn agents_dir(&self) -> Option<PathBuf> {
        self.sub(&self.manifest.agents, "agents")
    }

    pub fn skills_dir(&self) -> Option<PathBuf> {
        self.sub(&self.manifest.skills, "skills")
    }

    pub fn modes_dir(&self) -> Option<PathBuf> {
        self.sub(&self.manifest.modes, "modes")
    }

    pub fn runs_code(&self) -> bool {
        !self.manifest.hooks.is_empty() || !self.manifest.mcp.is_empty()
    }

    pub fn code_hash(&self) -> String {
        let text = format!(
            "{}\n{}",
            serde_json::to_string(&self.manifest.hooks).unwrap_or_default(),
            serde_json::to_string(&self.manifest.mcp).unwrap_or_default()
        );
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in text.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        format!("{hash:016x}")
    }

    pub fn approved(&self) -> bool {
        self.runs_code() && self.entry.approved.as_deref() == Some(self.code_hash().as_str())
    }

    pub fn needs_approval(&self) -> bool {
        self.runs_code() && !self.approved()
    }

    pub fn status(&self) -> &'static str {
        if !self.runs_code() {
            "enabled"
        } else if self.approved() {
            "enabled, hooks and MCP approved"
        } else {
            "enabled; hooks and MCP wait for approval"
        }
    }

    pub fn code(&self) -> Vec<String> {
        let mut out = Vec::new();
        let hooks = &self.manifest.hooks;
        for (event, list) in [
            ("pre_tool", &hooks.pre_tool),
            ("post_tool", &hooks.post_tool),
            ("prompt_submit", &hooks.prompt_submit),
            ("turn_end", &hooks.turn_end),
        ] {
            for hook in list {
                out.push(format!("hook {event}: {}", hook.command));
            }
        }
        for (name, server) in &self.manifest.mcp {
            let what = match (&server.command, &server.url) {
                (Some(command), _) => format!("{command} {}", server.args.join(" ")).trim().to_string(),
                (None, Some(url)) => url.clone(),
                (None, None) => "nothing".into(),
            };
            out.push(format!("MCP server {}: {what}", mcp_name(&self.manifest.name, name)));
        }
        out
    }

    pub fn contributions(&self) -> String {
        let count = |dir: Option<PathBuf>, ext: Option<&str>| -> usize {
            let Some(dir) = dir else { return 0 };
            std::fs::read_dir(dir)
                .map(|e| {
                    e.filter_map(|e| e.ok())
                        .filter(|e| match ext {
                            Some(ext) => e.path().extension().is_some_and(|x| x == ext),
                            None => e.path().is_dir(),
                        })
                        .count()
                })
                .unwrap_or(0)
        };
        let parts = [
            (count(self.commands_dir(), Some("md")), "commands"),
            (count(self.agents_dir(), Some("md")), "agents"),
            (count(self.skills_dir(), None), "skills"),
            (count(self.modes_dir(), Some("md")), "modes"),
            (self.code().iter().filter(|c| c.starts_with("hook")).count(), "hooks"),
            (self.manifest.mcp.len(), "MCP servers"),
        ];
        let listed: Vec<String> = parts.iter().filter(|(n, _)| *n > 0).map(|(n, what)| format!("{n} {what}")).collect();
        if listed.is_empty() { "nothing".into() } else { listed.join(", ") }
    }
}

pub fn mcp_name(plugin: &str, server: &str) -> String {
    format!("{plugin}-{server}")
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn read_manifest(dir: &Path) -> Result<Manifest> {
    let path = dir.join(MANIFEST);
    let text = std::fs::read_to_string(&path).with_context(|| format!("no {MANIFEST} in {}", dir.display()))?;
    let manifest: Manifest = toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
    if !valid_name(&manifest.name) {
        bail!("plugin name '{}' must be letters, digits, - or _", manifest.name);
    }
    Ok(manifest)
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn at(config_dir: &Path) -> Self {
        Store { root: config_dir.to_path_buf() }
    }

    pub fn system() -> Option<Self> {
        paths::user_dir().map(|d| Store::at(&d))
    }

    pub fn dir(&self) -> PathBuf {
        self.root.join("plugins")
    }

    fn state_path(&self) -> PathBuf {
        self.root.join(STATE)
    }

    pub fn state(&self) -> BTreeMap<String, Entry> {
        std::fs::read_to_string(self.state_path())
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save_state(&self, state: &BTreeMap<String, Entry>) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        std::fs::write(self.state_path(), toml::to_string(state)?)?;
        Ok(())
    }

    pub fn installed(&self) -> Vec<Plugin> {
        let state = self.state();
        let Ok(entries) = std::fs::read_dir(self.dir()) else {
            return Vec::new();
        };
        let mut out: Vec<Plugin> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| {
                let dir = e.path();
                let manifest = read_manifest(&dir).ok()?;
                let entry = state.get(&manifest.name).cloned().unwrap_or_default();
                Some(Plugin { manifest, dir, entry })
            })
            .collect();
        out.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
        out
    }

    pub fn find(&self, name: &str) -> Option<Plugin> {
        self.installed().into_iter().find(|p| p.name() == name)
    }

    pub fn install(&self, source: &str) -> Result<Plugin> {
        let staging = Staging::new(&self.root)?;
        let fetched = staging.0.join("src");
        if is_remote(source) {
            let status = std::process::Command::new("git")
                .args(["clone", "--depth", "1", "--quiet", "--no-recurse-submodules", source])
                .arg(&fetched)
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdin(std::process::Stdio::null())
                .status()
                .context("running git to fetch the plugin")?;
            if !status.success() {
                bail!("git clone {source} failed");
            }
        } else {
            let from = PathBuf::from(source);
            if !from.is_dir() {
                bail!("{source} is not a directory or a git URL");
            }
            copy_tree(&from, &fetched)?;
        }
        let manifest = read_manifest(&fetched)?;
        let target = self.dir().join(&manifest.name);
        if target.exists() {
            bail!("a plugin named {} is already installed; remove it first", manifest.name);
        }
        std::fs::create_dir_all(self.dir())?;
        copy_tree(&fetched, &target)?;
        let mut state = self.state();
        state.insert(manifest.name.clone(), Entry { source: source.to_string(), approved: None });
        self.save_state(&state)?;
        let entry = state.remove(&manifest.name).unwrap_or_default();
        Ok(Plugin { manifest, dir: target, entry })
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        let plugin = self.find(name).ok_or_else(|| anyhow!("no plugin named {name}"))?;
        std::fs::remove_dir_all(&plugin.dir)?;
        let mut state = self.state();
        state.remove(name);
        self.save_state(&state)
    }

    pub fn approve(&self, name: &str) -> Result<Plugin> {
        let mut plugin = self.find(name).ok_or_else(|| anyhow!("no plugin named {name}"))?;
        if !plugin.runs_code() {
            bail!("{name} has no hooks or MCP servers to approve");
        }
        let mut state = self.state();
        let entry = state.entry(name.to_string()).or_default();
        entry.approved = Some(plugin.code_hash());
        plugin.entry = entry.clone();
        self.save_state(&state)?;
        Ok(plugin)
    }

    pub fn revoke(&self, name: &str) -> Result<()> {
        let mut state = self.state();
        if let Some(entry) = state.get_mut(name) {
            entry.approved = None;
        }
        self.save_state(&state)
    }
}

struct Staging(PathBuf);

impl Staging {
    fn new(root: &Path) -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = root.join(format!(".plugin-staging-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        Ok(Staging(dir))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn is_remote(source: &str) -> bool {
    source.starts_with("https://") || source.starts_with("http://") || source.starts_with("file://") || source.starts_with("git@") || source.starts_with("ssh://") || source.ends_with(".git")
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let kind = entry.file_type()?;
        let target = to.join(&name);
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

pub fn installed() -> Vec<Plugin> {
    Store::system().map(|s| s.installed()).unwrap_or_default()
}

pub fn contribute_from(cfg: &mut crate::Config, plugins: &[Plugin]) {
    for plugin in plugins.iter().filter(|p| p.approved()) {
        cfg.hooks.fill_from(plugin.manifest.hooks.clone());
        for (name, server) in &plugin.manifest.mcp {
            cfg.mcp.entry(mcp_name(plugin.name(), name)).or_insert_with(|| server.clone());
        }
    }
}

pub fn contribute(cfg: &mut crate::Config) {
    contribute_from(cfg, &installed());
}

pub fn agents(plugins: &[Plugin]) -> Vec<crate::agents::AgentFile> {
    let mut out = Vec::new();
    for plugin in plugins {
        let Some(dir) = plugin.agents_dir() else { continue };
        for mut file in crate::agents::load_from(Some(&dir), Path::new("")) {
            file.def.name = format!("{}:{}", plugin.name(), file.def.name);
            out.push(file);
        }
    }
    out
}

pub fn skills(plugins: &[Plugin]) -> Vec<zarn_tools::skill::Skill> {
    let mut out = Vec::new();
    for plugin in plugins {
        let Some(dir) = plugin.skills_dir() else { continue };
        for mut skill in zarn_tools::skill::discover(Some(&dir), Path::new("")) {
            skill.name = format!("{}:{}", plugin.name(), skill.name);
            out.push(skill);
        }
    }
    out
}

pub fn modes(plugins: &[Plugin]) -> Vec<crate::modes::ModeFile> {
    let mut out = Vec::new();
    for plugin in plugins {
        let Some(dir) = plugin.modes_dir() else { continue };
        for mut file in crate::modes::load_from(Some(&dir), Path::new("")).modes {
            if file.source == crate::modes::Source::Builtin {
                continue;
            }
            file.def.name = format!("{}:{}", plugin.name(), file.def.name);
            out.push(file);
        }
    }
    out
}

pub fn commands_into(commands: &mut zarn_core::commands::Commands, plugins: &[Plugin]) {
    for plugin in plugins {
        if let Some(dir) = plugin.commands_dir() {
            commands.load_namespaced(&dir, plugin.name());
        }
    }
}
