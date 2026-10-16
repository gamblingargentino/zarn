use crate::layers::Scope;
use crate::paths;
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use zarn_tools::permissions::{Overlay, Policy};

pub const BUILTIN_NAMES: [&str; 4] = ["default", "plan", "accept-edits", "bypass"];
pub const CYCLE_SKIPS: [&str; 1] = ["bypass"];
pub const PLAN_PROMPT: &str = "You are in plan mode. Investigate with read-only tools only, then present a concrete, step-by-step plan by calling exit_plan. Do not edit files or run commands until the user approves the plan.";
pub const BYPASS_RISK: &str = "bypass mode runs every tool, including shell commands and file writes, without asking. Only rules you set to deny still apply.";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModeDef {
    pub name: String,
    pub description: String,
    pub tools: Vec<String>,
    pub permissions: BTreeMap<String, Policy>,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub prompt: String,
    pub allow_all: bool,
    pub safe_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Builtin,
    User,
    Project,
}

impl Source {
    pub fn label(&self) -> &'static str {
        match self {
            Source::Builtin => "built-in",
            Source::User => "user",
            Source::Project => "project",
        }
    }

    pub fn scope(&self) -> Option<Scope> {
        match self {
            Source::Builtin => None,
            Source::User => Some(Scope::User),
            Source::Project => Some(Scope::Project),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModeFile {
    pub def: ModeDef,
    pub source: Source,
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Loaded {
    pub modes: Vec<ModeFile>,
    pub warnings: Vec<String>,
}

impl Loaded {
    pub fn find(&self, name: &str) -> Option<&ModeFile> {
        self.modes.iter().find(|m| m.def.name == name)
    }

    pub fn names(&self) -> Vec<String> {
        self.modes.iter().map(|m| m.def.name.clone()).collect()
    }

    pub fn next_after(&self, current: &str) -> String {
        let cycle: Vec<&ModeFile> = self
            .modes
            .iter()
            .filter(|m| !CYCLE_SKIPS.contains(&m.def.name.as_str()))
            .collect();
        let at = cycle.iter().position(|m| m.def.name == current);
        let next = match at {
            Some(i) => (i + 1) % cycle.len().max(1),
            None => 0,
        };
        cycle
            .get(next)
            .map(|m| m.def.name.clone())
            .unwrap_or_else(|| "default".into())
    }
}

pub fn is_builtin(name: &str) -> bool {
    BUILTIN_NAMES.contains(&name)
}

pub fn builtin(name: &str) -> Option<ModeDef> {
    let def = |description: &str| ModeDef {
        name: name.to_string(),
        description: description.to_string(),
        ..ModeDef::default()
    };
    match name {
        "default" => Some(def("ask before edits and commands, as configured")),
        "plan" => Some(ModeDef {
            prompt: PLAN_PROMPT.to_string(),
            safe_only: true,
            ..def("read-only investigation, then a plan for approval")
        }),
        "accept-edits" => Some(ModeDef {
            permissions: BTreeMap::from([
                ("edit".to_string(), Policy::Allow),
                ("write".to_string(), Policy::Allow),
                ("multiedit".to_string(), Policy::Allow),
                ("notebook_edit".to_string(), Policy::Allow),
            ]),
            ..def("file edits run without asking; everything else as configured")
        }),
        "bypass" => Some(ModeDef {
            allow_all: true,
            ..def("every tool runs without asking")
        }),
        _ => None,
    }
}

pub fn overlay(def: &ModeDef) -> Overlay {
    Overlay {
        name: def.name.clone(),
        policies: def
            .permissions
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect::<HashMap<_, _>>(),
        allow_all: def.allow_all,
        safe_only: def.safe_only,
    }
}

pub fn prompt_section(def: &ModeDef) -> String {
    if def.prompt.trim().is_empty() {
        return String::new();
    }
    format!(
        "Active mode: {} ({}).\n{}",
        def.name,
        def.description,
        def.prompt.trim()
    )
}

fn parse_policy(word: &str) -> Option<Policy> {
    match word.trim().to_lowercase().as_str() {
        "allow" => Some(Policy::Allow),
        "ask" => Some(Policy::Ask),
        "deny" => Some(Policy::Deny),
        _ => None,
    }
}

fn list(value: &str) -> Vec<String> {
    value
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|t| t.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

pub fn parse(source: &str) -> Result<ModeDef, String> {
    let source = source.trim_start_matches('\u{feff}');
    let rest = source
        .strip_prefix("---")
        .ok_or("it does not start with a --- frontmatter block")?;
    let end = rest
        .find("\n---")
        .ok_or("its frontmatter block is not closed with ---")?;
    let (front, body) = rest.split_at(end);
    let body = body.trim_start_matches("\n---").trim();
    let mut def = ModeDef {
        prompt: body.to_string(),
        ..ModeDef::default()
    };
    for (n, line) in front.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(format!("frontmatter line {} has no 'key:'", n + 1));
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match key.trim() {
            "name" => def.name = value.to_string(),
            "description" => def.description = value.to_string(),
            "tools" => def.tools = list(value),
            "profile" => def.profile = Some(value.to_string()).filter(|v| !v.is_empty()),
            "model" => def.model = Some(value.to_string()).filter(|v| !v.is_empty()),
            "permissions" => {
                for entry in list(value) {
                    let Some((tool, policy)) = entry.split_once('=') else {
                        return Err(format!("permission '{entry}' is not tool=policy"));
                    };
                    let Some(policy) = parse_policy(policy) else {
                        return Err(format!(
                            "permission '{entry}' must be allow, ask or deny"
                        ));
                    };
                    def.permissions.insert(tool.trim().to_string(), policy);
                }
            }
            other => return Err(format!("unknown frontmatter key '{other}'")),
        }
    }
    if def.name.is_empty() {
        return Err("it has no name".into());
    }
    if !crate::catalog::valid_name(&def.name) {
        return Err(format!(
            "'{}' is not a valid mode name: use letters, digits, '-' and '_'",
            def.name
        ));
    }
    Ok(def)
}

pub fn render(def: &ModeDef) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("name: {}\n", def.name));
    out.push_str(&format!(
        "description: {}\n",
        def.description.replace('\n', " ")
    ));
    if !def.tools.is_empty() {
        out.push_str(&format!("tools: {}\n", def.tools.join(", ")));
    }
    if !def.permissions.is_empty() {
        let perms = def
            .permissions
            .iter()
            .map(|(tool, policy)| {
                let word = match policy {
                    Policy::Allow => "allow",
                    Policy::Ask => "ask",
                    Policy::Deny => "deny",
                };
                format!("{tool}={word}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("permissions: {perms}\n"));
    }
    if let Some(profile) = &def.profile {
        out.push_str(&format!("profile: {profile}\n"));
    }
    if let Some(model) = &def.model {
        out.push_str(&format!("model: {model}\n"));
    }
    out.push_str("---\n\n");
    out.push_str(def.prompt.trim());
    out.push('\n');
    out
}

pub fn dir(scope: Scope) -> Option<PathBuf> {
    match scope {
        Scope::User => paths::user_dir().map(|d| d.join("modes")),
        _ => Some(paths::project_dir().join("modes")),
    }
}

fn read_dir(dir: &Path, source: Source, out: &mut Loaded) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    paths.sort();
    for path in paths {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                out.warnings
                    .push(format!("skipped mode file {}: {err}", path.display()));
                continue;
            }
        };
        match parse(&text) {
            Err(why) => out.warnings.push(format!(
                "skipped mode file {}: {why}",
                path.display()
            )),
            Ok(def) if is_builtin(&def.name) => out.warnings.push(format!(
                "rejected mode file {}: '{}' is a built-in mode and cannot be redefined; rename it",
                path.display(),
                def.name
            )),
            Ok(def) => {
                out.modes.retain(|m| m.def.name != def.name);
                out.modes.push(ModeFile {
                    def,
                    source,
                    path: Some(path),
                });
            }
        }
    }
}

pub fn load_from(user: Option<&Path>, project: &Path) -> Loaded {
    let mut loaded = Loaded {
        modes: BUILTIN_NAMES
            .iter()
            .filter_map(|n| builtin(n))
            .map(|def| ModeFile {
                def,
                source: Source::Builtin,
                path: None,
            })
            .collect(),
        warnings: Vec::new(),
    };
    if let Some(user) = user {
        read_dir(user, Source::User, &mut loaded);
    }
    read_dir(project, Source::Project, &mut loaded);
    let (builtins, mut custom): (Vec<ModeFile>, Vec<ModeFile>) = loaded
        .modes
        .into_iter()
        .partition(|m| m.source == Source::Builtin);
    custom.sort_by(|a, b| a.def.name.cmp(&b.def.name));
    loaded.modes = builtins.into_iter().chain(custom).collect();
    loaded
}

pub fn load() -> Loaded {
    let project = dir(Scope::Project).unwrap_or_default();
    load_from(dir(Scope::User).as_deref(), &project)
}

pub fn save_in(dir: &Path, def: &ModeDef) -> Result<PathBuf> {
    if is_builtin(&def.name) {
        bail!("'{}' is a built-in mode; pick another name", def.name);
    }
    if !crate::catalog::valid_name(&def.name) {
        bail!(
            "'{}' is not a valid mode name: use letters, digits, '-' and '_'",
            def.name
        );
    }
    let text = render(def);
    parse(&text).map_err(|why| anyhow::anyhow!("the mode would not load back: {why}"))?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!("{}.md", def.name));
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub fn save(def: &ModeDef, scope: Scope) -> Result<PathBuf> {
    let dir = dir(scope).context("cannot determine a home directory for user modes")?;
    save_in(&dir, def)
}

pub fn delete(file: &ModeFile) -> Result<()> {
    match &file.path {
        None => bail!("'{}' is built in and cannot be deleted", file.def.name),
        Some(path) => {
            std::fs::remove_file(path).with_context(|| format!("removing {}", path.display()))
        }
    }
}

pub fn describe(file: &ModeFile) -> String {
    let def = &file.def;
    let mut lines = vec![
        format!("name         {}", def.name),
        format!("source       {}", file.source.label()),
        format!("description  {}", def.description),
    ];
    if let Some(path) = &file.path {
        lines.push(format!("file         {}", path.display()));
    }
    lines.push(format!(
        "tools        {}",
        if def.safe_only {
            "read-only tools and exit_plan".to_string()
        } else if def.tools.is_empty() {
            "all".to_string()
        } else {
            def.tools.join(", ")
        }
    ));
    let perms = if def.allow_all {
        "every tool allowed (deny rules still apply)".to_string()
    } else if def.permissions.is_empty() {
        "as configured".to_string()
    } else {
        render(def)
            .lines()
            .find_map(|l| l.strip_prefix("permissions: ").map(str::to_string))
            .unwrap_or_default()
    };
    lines.push(format!("permissions  {perms}"));
    if let Some(profile) = &def.profile {
        lines.push(format!("profile      {profile}"));
    }
    if let Some(model) = &def.model {
        lines.push(format!("model        {model}"));
    }
    if !def.prompt.trim().is_empty() {
        lines.push(String::new());
        lines.push(def.prompt.trim().to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVIEW: &str = "---\nname: review\ndescription: read-only code review\ntools: read, glob, grep\npermissions: edit=deny, write=deny, bash=ask\nprofile: default\nmodel: \n---\nReview the code. Do not modify files.\n";

    fn write(dir: &Path, file: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(file), body).unwrap();
    }

    #[test]
    fn the_documented_example_parses() {
        let def = parse(REVIEW).unwrap();
        assert_eq!(def.name, "review");
        assert_eq!(def.tools, vec!["read", "glob", "grep"]);
        assert_eq!(def.permissions["edit"], Policy::Deny);
        assert_eq!(def.permissions["bash"], Policy::Ask);
        assert_eq!(def.profile.as_deref(), Some("default"));
        assert!(def.model.is_none());
        assert!(def.prompt.starts_with("Review the code"));
    }

    #[test]
    fn builtins_and_both_directories_load_and_project_wins() {
        let user = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write(user.path(), "review.md", REVIEW);
        write(
            user.path(),
            "docs.md",
            "---\nname: docs\ndescription: writes docs\n---\nWrite docs.",
        );
        write(
            project.path(),
            "review.md",
            &REVIEW.replace("read-only code review", "project review"),
        );
        let loaded = load_from(Some(user.path()), project.path());
        assert_eq!(
            loaded.names(),
            vec!["default", "plan", "accept-edits", "bypass", "docs", "review"]
        );
        let review = loaded.find("review").unwrap();
        assert_eq!(review.def.description, "project review");
        assert_eq!(review.source, Source::Project);
        assert_eq!(loaded.find("docs").unwrap().source, Source::User);
        assert!(loaded.warnings.is_empty());
    }

    #[test]
    fn a_file_using_a_builtin_name_is_rejected_naming_the_file() {
        let user = tempfile::tempdir().unwrap();
        write(user.path(), "mine.md", "---\nname: plan\ndescription: x\n---\nbody");
        let loaded = load_from(Some(user.path()), Path::new("/nonexistent"));
        assert_eq!(loaded.find("plan").unwrap().source, Source::Builtin);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("mine.md"));
        assert!(loaded.warnings[0].contains("built-in"));
    }

    #[test]
    fn a_malformed_file_is_skipped_with_a_warning_and_the_rest_still_load() {
        let project = tempfile::tempdir().unwrap();
        write(project.path(), "bad.md", "no frontmatter here");
        write(
            project.path(),
            "worse.md",
            "---\nname: worse\npermissions: edit=maybe\n---\n",
        );
        write(project.path(), "good.md", "---\nname: good\ndescription: ok\n---\n");
        let loaded = load_from(None, project.path());
        assert!(loaded.find("good").is_some());
        assert!(loaded.find("worse").is_none());
        assert_eq!(loaded.warnings.len(), 2);
        assert!(loaded.warnings.iter().any(|w| w.contains("bad.md")));
        assert!(loaded.warnings.iter().any(|w| w.contains("allow, ask or deny")));
    }

    #[test]
    fn cycling_walks_the_modes_and_skips_bypass() {
        let loaded = load_from(None, Path::new("/nonexistent"));
        assert_eq!(loaded.next_after("default"), "plan");
        assert_eq!(loaded.next_after("plan"), "accept-edits");
        assert_eq!(loaded.next_after("accept-edits"), "default");
        assert_eq!(loaded.next_after("bypass"), "default");
    }

    #[test]
    fn a_saved_mode_round_trips_and_builtins_cannot_be_saved_or_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let def = parse(REVIEW).unwrap();
        let path = save_in(dir.path(), &def).unwrap();
        assert_eq!(parse(&std::fs::read_to_string(path).unwrap()).unwrap(), def);
        assert!(save_in(dir.path(), &builtin("plan").unwrap()).is_err());
        let file = ModeFile {
            def: builtin("default").unwrap(),
            source: Source::Builtin,
            path: None,
        };
        assert!(delete(&file).is_err());
    }

    fn gate(mode: &str) -> zarn_tools::permissions::Permissions {
        zarn_tools::permissions::Permissions::headless()
            .with_tool("rm_everything", Policy::Deny)
            .with_overlay(overlay(&builtin(mode).unwrap()))
    }

    #[test]
    fn each_builtin_policy_resolves_through_permissions() {
        use zarn_core::tools::Risk;
        let default = gate("default");
        assert_eq!(default.policy_for("edit", Risk::Write), Policy::Ask);
        assert_eq!(default.policy_for("bash", Risk::Execute), Policy::Ask);
        assert_eq!(default.policy_for("read", Risk::Safe), Policy::Allow);

        let accept = gate("accept-edits");
        assert_eq!(accept.policy_for("edit", Risk::Write), Policy::Allow);
        assert_eq!(accept.policy_for("write", Risk::Write), Policy::Allow);
        assert_eq!(accept.policy_for("bash", Risk::Execute), Policy::Ask);

        let bypass = gate("bypass");
        assert_eq!(bypass.policy_for("bash", Risk::Execute), Policy::Allow);
        assert_eq!(bypass.policy_for("mcp__x__y", Risk::Execute), Policy::Allow);
        assert_eq!(bypass.policy_for("rm_everything", Risk::Execute), Policy::Deny);

        let plan = gate("plan");
        assert_eq!(plan.policy_for("write", Risk::Write), Policy::Deny);
        assert_eq!(plan.policy_for("grep", Risk::Safe), Policy::Allow);
    }

    #[test]
    fn overlays_reflect_each_builtin() {
        assert!(overlay(&builtin("bypass").unwrap()).allow_all);
        assert!(overlay(&builtin("plan").unwrap()).safe_only);
        assert_eq!(
            overlay(&builtin("accept-edits").unwrap()).policies["edit"],
            Policy::Allow
        );
        assert!(overlay(&builtin("default").unwrap()).is_empty());
    }
}
