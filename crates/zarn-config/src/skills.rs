use crate::paths;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use zarn_tools::skill::{self, FILE};

const SEARCH_DEPTH: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    User,
    Project,
}

impl Place {
    pub fn from_flag(project: bool) -> Self {
        if project { Place::Project } else { Place::User }
    }

    pub fn root(&self) -> Result<PathBuf> {
        match self {
            Place::User => Ok(paths::user_dir()
                .context("no home directory, so there is no user skills folder; use --project")?
                .join("skills")),
            Place::Project => Ok(paths::project_dir().join("skills")),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Place::User => "user",
            Place::Project => "project",
        }
    }

    pub fn commands_root(&self) -> Result<PathBuf> {
        Ok(self.root()?.with_file_name("commands"))
    }
}

pub fn scaffold_command(root: &Path, name: &str, description: &str) -> Result<PathBuf> {
    if !valid_name(name) {
        bail!("'{name}' is not a valid command name: use lowercase letters, digits, '-' and '_'");
    }
    let path = root.join(format!("{name}.md"));
    if path.exists() {
        bail!("a command named {name} already exists at {}", path.display());
    }
    let description = if description.trim().is_empty() { "what this command does" } else { description.trim() };
    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    std::fs::write(
        &path,
        format!(
            "---\ndescription: {}\nargument-hint: [what to work on]\n---\n\nWrite the prompt this command sends. $ARGUMENTS is replaced with what you type after /{name}.\n\n$ARGUMENTS\n",
            description.replace('\n', " ")
        ),
    )
    .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').chars().take(64).collect()
}

pub fn template(name: &str, description: &str) -> String {
    let description = if description.trim().is_empty() {
        "Say what this skill does and when to use it; this line is all the model sees until it loads the skill."
    } else {
        description.trim()
    };
    format!(
        "---\nname: {name}\ndescription: {}\n---\n\n# {name}\n\n## When to use\n\n- \n\n## Steps\n\n1. \n\n## Check the result\n\n- \n",
        description.replace('\n', " ")
    )
}

pub fn scaffold(root: &Path, name: &str, description: &str) -> Result<PathBuf> {
    if !valid_name(name) {
        bail!("'{name}' is not a valid skill name: use lowercase letters, digits, '-' and '_'");
    }
    let dir = root.join(name);
    if dir.join(FILE).exists() {
        bail!("a skill named {name} already exists at {}", dir.display());
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(FILE);
    std::fs::write(&path, template(name, description)).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub fn creator_prompt(name: &str, request: &str, root: &Path) -> String {
    let target = if name.is_empty() {
        format!("{}/<name>/{FILE}", root.display())
    } else {
        format!("{}/{name}/{FILE}", root.display())
    };
    let request = if request.trim().is_empty() {
        "(nothing yet: ask me what the skill should do)".to_string()
    } else {
        request.trim().to_string()
    };
    format!(
        "Create a new zarn skill. A skill is a folder holding a {FILE} file: YAML frontmatter with `name` (lowercase letters, digits and hyphens, matching the folder) and `description` (one or two sentences on what it does and when to use it; it is all the model sees until it loads the skill), followed by Markdown instructions. Scripts, templates and reference files can sit next to {FILE}, referenced by relative path.

Steps:
1. If the request below leaves out what the skill should do, its inputs or its output, ask me up to three short questions and wait for my answers.
2. Look at this project and at any existing skills for conventions worth following.
3. Write {target} and any supporting files. Keep the instructions concrete: when to use the skill, what it needs, numbered steps with the exact commands, how to check the result, and known pitfalls. Prefer a script for any step that must be exact.
4. Read the file back, check the frontmatter, and tell me the path and how to use it. zarn picks it up automatically; /skills lists it.

Request: {request}"
    )
}

pub fn find_skill_dirs(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(root, 0, &mut found);
    found.sort();
    found
}

fn walk(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    if dir.join(FILE).is_file() {
        found.push(dir.to_path_buf());
        return;
    }
    if depth >= SEARCH_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let hidden = entry.file_name().to_string_lossy().starts_with('.');
        if path.is_dir() && !hidden && entry.file_type().is_ok_and(|t| !t.is_symlink()) {
            walk(&path, depth + 1, found);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitSource {
    pub url: String,
    pub branch: Option<String>,
    pub subdir: Option<String>,
}

pub fn github_tree(source: &str) -> Option<GitSource> {
    let rest = source
        .strip_prefix("https://github.com/")
        .or_else(|| source.strip_prefix("http://github.com/"))?;
    let parts: Vec<&str> = rest.trim_end_matches('/').split('/').collect();
    if parts.len() < 2 {
        return None;
    }
    let url = format!("https://github.com/{}/{}", parts[0], parts[1].trim_end_matches(".git"));
    match parts.get(2) {
        None => Some(GitSource { url, branch: None, subdir: None }),
        Some(&"tree") | Some(&"blob") if parts.len() >= 4 => {
            let mut subdir: Vec<&str> = parts[4..].to_vec();
            if subdir.last() == Some(&FILE) {
                subdir.pop();
            }
            Some(GitSource {
                url,
                branch: Some(parts[3].to_string()),
                subdir: Some(subdir.join("/")).filter(|s| !s.is_empty()),
            })
        }
        _ => None,
    }
}

struct Staging(PathBuf);

impl Staging {
    fn new() -> Result<Self> {
        let unique = format!(
            "zarn-skill-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let dir = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&dir)?;
        Ok(Staging(dir))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn clone(source: &GitSource, into: &Path) -> Result<()> {
    let mut git = std::process::Command::new("git");
    git.args(["clone", "--depth", "1", "--quiet", "--no-recurse-submodules"]);
    if let Some(branch) = &source.branch {
        git.args(["--branch", branch]);
    }
    let status = git
        .arg(&source.url)
        .arg(into)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .context("running git to fetch the skill (is git installed?)")?;
    if !status.success() {
        bail!("git clone {} failed", source.url);
    }
    Ok(())
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

fn is_raw_file(source: &str) -> bool {
    (source.starts_with("https://") || source.starts_with("http://")) && source.ends_with(FILE) && !source.contains("github.com/")
}

async fn download(url: &str, into: &Path) -> Result<()> {
    let response = reqwest::get(url).await.with_context(|| format!("downloading {url}"))?;
    if !response.status().is_success() {
        bail!("downloading {url} failed: HTTP {}", response.status().as_u16());
    }
    let text = response.text().await?;
    if skill::parse(&text).is_none() {
        bail!("{url} is not a {FILE}: it has no --- frontmatter block");
    }
    std::fs::create_dir_all(into)?;
    std::fs::write(into.join(FILE), text)?;
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Installed {
    pub added: Vec<(String, PathBuf)>,
    pub skipped: Vec<String>,
}

impl Installed {
    pub fn summary(&self, place: Place) -> String {
        let mut parts = Vec::new();
        if !self.added.is_empty() {
            let names: Vec<&str> = self.added.iter().map(|(n, _)| n.as_str()).collect();
            parts.push(format!(
                "installed {} in the {} skills folder; review {} with /skills show <name> before you rely on {}",
                names.join(", "),
                place.label(),
                if names.len() == 1 { "it" } else { "them" },
                if names.len() == 1 { "it" } else { "them" }
            ));
        }
        if !self.skipped.is_empty() {
            parts.push(format!(
                "skipped {} (already installed; remove it first to replace it)",
                self.skipped.join(", ")
            ));
        }
        parts.join("; ")
    }
}

pub async fn install(root: &Path, source: &str) -> Result<Installed> {
    let source = source.trim();
    if source.is_empty() {
        bail!("give a git URL, a GitHub folder link, a path, or a link to a {FILE}");
    }
    let staging = Staging::new()?;
    let fetched = staging.0.join("src");
    let base = if is_raw_file(source) {
        let name = source.trim_end_matches(FILE).trim_end_matches('/').rsplit('/').next().unwrap_or("skill");
        let dir = fetched.join(slug(name));
        download(source, &dir).await?;
        fetched.clone()
    } else if let Some(git) = github_tree(source) {
        let target = fetched.clone();
        let spec = git.clone();
        tokio::task::spawn_blocking(move || clone(&spec, &target)).await??;
        match &git.subdir {
            Some(sub) => fetched.join(sub),
            None => fetched.clone(),
        }
    } else if crate::plugins::is_remote(source) {
        let target = fetched.clone();
        let spec = GitSource { url: source.to_string(), branch: None, subdir: None };
        tokio::task::spawn_blocking(move || clone(&spec, &target)).await??;
        fetched.clone()
    } else {
        let local = PathBuf::from(source);
        let local = if local.file_name().is_some_and(|f| f == FILE) {
            local.parent().map(Path::to_path_buf).unwrap_or(local)
        } else {
            local
        };
        if !local.is_dir() {
            bail!("{source} is not a folder, a {FILE}, a git URL or a link to a {FILE}");
        }
        local
    };
    let dirs = find_skill_dirs(&base);
    if dirs.is_empty() {
        bail!("no {FILE} found in {source}");
    }
    let mut done = Installed::default();
    for dir in dirs {
        let text = std::fs::read_to_string(dir.join(FILE))?;
        let folder = dir.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
        let named = skill::parse(&text).and_then(|(name, _)| name).unwrap_or(folder);
        let name = if valid_name(&named) { named } else { slug(&named) };
        if name.is_empty() {
            continue;
        }
        let target = root.join(&name);
        if target.exists() {
            done.skipped.push(name);
            continue;
        }
        copy_tree(&dir, &target)?;
        done.added.push((name, target));
    }
    if done.added.is_empty() && done.skipped.is_empty() {
        bail!("no usable skill found in {source}");
    }
    Ok(done)
}

pub fn remove(name: &str) -> Result<PathBuf> {
    if name.contains(':') {
        bail!("{name} comes from a plugin; remove the plugin with /plugins instead");
    }
    let found = crate::context::discover_skills()
        .into_iter()
        .find(|s| s.name == name)
        .with_context(|| format!("no skill named {name}"))?;
    let (user, project) = crate::context::skills_roots();
    let inside = |root: &Path| {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        found.dir.canonicalize().unwrap_or_else(|_| found.dir.clone()).starts_with(root)
    };
    if !(user.as_deref().is_some_and(inside) || inside(&project)) {
        bail!("{name} is not in a zarn skills folder; delete {} yourself", found.dir.display());
    }
    std::fs::remove_dir_all(&found.dir).with_context(|| format!("removing {}", found.dir.display()))?;
    Ok(found.dir)
}

pub fn show(name: &str) -> Result<String> {
    let found = crate::context::discover_skills()
        .into_iter()
        .find(|s| s.name == name)
        .with_context(|| format!("no skill named {name}"))?;
    let mut out = format!("{} ({})\n{}\n\n{}", found.name, found.source.label(), found.path().display(), found.body()?.trim());
    let files = found.supporting_files();
    if !files.is_empty() {
        out.push_str(&format!("\n\nfiles: {}", files.join(", ")));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_links_name_the_repo_branch_and_folder() {
        assert_eq!(
            github_tree("https://github.com/acme/skills/tree/main/docs/pdf"),
            Some(GitSource {
                url: "https://github.com/acme/skills".into(),
                branch: Some("main".into()),
                subdir: Some("docs/pdf".into()),
            })
        );
        assert_eq!(
            github_tree("https://github.com/acme/skills/blob/dev/pdf/SKILL.md").unwrap().subdir.as_deref(),
            Some("pdf")
        );
        assert_eq!(github_tree("https://github.com/acme/skills.git").unwrap().subdir, None);
        assert_eq!(github_tree("https://gitlab.com/acme/skills"), None);
    }

    #[test]
    fn names_are_slugged_and_checked() {
        assert_eq!(slug("Fill PDF forms!"), "fill-pdf-forms");
        assert!(valid_name("pdf-forms"));
        assert!(!valid_name("PDF forms"));
    }

    #[test]
    fn a_scaffold_parses_and_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = scaffold(dir.path(), "notes", "keep meeting notes").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(skill::parse(&text), Some((Some("notes".into()), "keep meeting notes".into())));
        assert!(scaffold(dir.path(), "notes", "again").is_err());
        assert!(scaffold(dir.path(), "Bad Name", "").is_err());
    }

    #[tokio::test]
    async fn installs_every_skill_in_a_folder_and_skips_ones_already_there() {
        let source = tempfile::tempdir().unwrap();
        for (folder, name) in [("a", "alpha"), ("nested/b", "beta")] {
            let dir = source.path().join("skills").join(folder);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(FILE), format!("---\nname: {name}\ndescription: d\n---\nbody")).unwrap();
            std::fs::write(dir.join("helper.sh"), "echo hi").unwrap();
        }
        let root = tempfile::tempdir().unwrap();
        let done = install(root.path(), &source.path().to_string_lossy()).await.unwrap();
        let names: Vec<&str> = done.added.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["alpha", "beta"]);
        assert!(root.path().join("beta/helper.sh").is_file());

        let again = install(root.path(), &source.path().join("skills/a/SKILL.md").to_string_lossy())
            .await
            .unwrap();
        assert_eq!(again.skipped, ["alpha"]);
        assert!(install(root.path(), "/no/such/place").await.is_err());
    }
}
