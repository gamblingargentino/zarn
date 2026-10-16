use crate::layers::Scope;
use crate::paths;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use zarn_core::subagent::{AgentDef, parse_definition};

#[derive(Clone, Debug, PartialEq)]
pub struct AgentFile {
    pub def: AgentDef,
    pub scope: Scope,
    pub path: PathBuf,
}

pub fn dir(scope: Scope) -> Option<PathBuf> {
    match scope {
        Scope::Project | Scope::Local => Some(paths::project_dir().join("agents")),
        Scope::User => paths::user_dir().map(|d| d.join("agents")),
    }
}

fn read_dir(dir: &Path, scope: Scope) -> Vec<AgentFile> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .filter_map(|path| {
            let source = std::fs::read_to_string(&path).ok()?;
            let def = parse_definition(&source)?;
            Some(AgentFile { def, scope, path })
        })
        .collect()
}

pub fn load_from(user: Option<&Path>, project: &Path) -> Vec<AgentFile> {
    let mut found: Vec<AgentFile> = Vec::new();
    for file in user
        .map(|u| read_dir(u, Scope::User))
        .unwrap_or_default()
        .into_iter()
        .chain(read_dir(project, Scope::Project))
    {
        found.retain(|f| f.def.name != file.def.name);
        found.push(file);
    }
    found.sort_by(|a, b| a.def.name.cmp(&b.def.name));
    found
}

pub fn load() -> Vec<AgentFile> {
    let project = dir(Scope::Project).unwrap_or_default();
    let mut found = load_from(dir(Scope::User).as_deref(), &project);
    found.extend(crate::plugins::agents(&crate::plugins::installed()));
    found
}

pub fn render(def: &AgentDef) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("name: {}\n", def.name));
    out.push_str(&format!("description: {}\n", def.description.replace('\n', " ")));
    if !def.tools.is_empty() {
        out.push_str(&format!("tools: {}\n", def.tools.join(", ")));
    }
    if let Some(isolation) = &def.isolation {
        out.push_str(&format!("isolation: {isolation}\n"));
    }
    if let Some(model) = &def.model {
        out.push_str(&format!("model: {model}\n"));
    }
    out.push_str("---\n\n");
    out.push_str(def.instructions.trim());
    out.push('\n');
    out
}

pub fn save_in(dir: &Path, def: &AgentDef) -> Result<PathBuf> {
    if !crate::catalog::valid_name(&def.name) {
        bail!(
            "'{}' is not a valid agent name: use letters, digits, '-' and '_'",
            def.name
        );
    }
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!("{}.md", def.name));
    std::fs::write(&path, render(def)).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub fn save(def: &AgentDef, scope: Scope) -> Result<PathBuf> {
    let dir = dir(scope).context("cannot determine a home directory for user agents")?;
    save_in(&dir, def)
}

pub fn delete(file: &AgentFile) -> Result<()> {
    std::fs::remove_file(&file.path).with_context(|| format!("removing {}", file.path.display()))
}

pub fn split_tools(text: &str) -> Vec<String> {
    text.split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(name: &str, description: &str) -> AgentDef {
        AgentDef {
            name: name.into(),
            description: description.into(),
            tools: vec!["read".into(), "grep".into()],
            model: None,
            instructions: "Review carefully.".into(),
            isolation: Some("worktree".into()),
        }
    }

    #[test]
    fn a_saved_agent_parses_back_to_the_same_definition() {
        let dir = tempfile::tempdir().unwrap();
        let path = save_in(dir.path(), &def("reviewer", "reviews diffs")).unwrap();
        let parsed = parse_definition(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(parsed, def("reviewer", "reviews diffs"));
    }

    #[test]
    fn a_project_agent_shadows_a_user_agent_of_the_same_name() {
        let user = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        save_in(user.path(), &def("a", "from user")).unwrap();
        save_in(user.path(), &def("b", "only user")).unwrap();
        save_in(project.path(), &def("a", "from project")).unwrap();
        let found = load_from(Some(user.path()), project.path());
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].def.description, "from project");
        assert_eq!(found[0].scope, Scope::Project);
        assert_eq!(found[1].scope, Scope::User);
    }

    #[test]
    fn a_bad_name_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(save_in(dir.path(), &def("../escape", "x")).is_err());
    }
}
