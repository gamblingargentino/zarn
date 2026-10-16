use crate::paths;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zarn_core::tools::Tool;
use zarn_tools::skill::{self, Skill};

pub const MEMORY_FILE: &str = "ZARN.md";
pub const DEFAULT_BUDGET: u32 = 8_000;
pub const IMPORT_DEPTH: usize = 5;
pub const MEMORY_TOOLS: [&str; 5] = ["read", "edit", "write", "multiedit", "notebook_edit"];

fn import_target(line: &str) -> Option<&str> {
    let t = line.trim();
    let rest = t.strip_prefix('@')?;
    if rest.is_empty() || rest.contains(char::is_whitespace) {
        return None;
    }
    Some(rest)
}

fn import_path(base: &Path, target: &str) -> PathBuf {
    if let Some(rest) = target.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    let p = Path::new(target);
    if p.is_absolute() { p.to_path_buf() } else { base.join(p) }
}

fn expand_in(text: &str, base: &Path, chain: &mut Vec<PathBuf>) -> String {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(target) = import_target(line) else {
            out.push(line.to_string());
            continue;
        };
        let path = import_path(base, target);
        let key = path.canonicalize().unwrap_or_else(|_| path.clone());
        if chain.contains(&key) {
            out.push(format!("[import @{target} skipped: it would import itself in a cycle]"));
            continue;
        }
        if chain.len() > IMPORT_DEPTH {
            out.push(format!("[import @{target} skipped: imports nest deeper than {IMPORT_DEPTH}]"));
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(body) => {
                chain.push(key);
                let dir = path.parent().map(Path::to_path_buf).unwrap_or_else(|| base.to_path_buf());
                out.push(expand_in(body.trim_end(), &dir, chain));
                chain.pop();
            }
            Err(_) => out.push(format!("[import @{target} not found]")),
        }
    }
    out.join("\n")
}

pub fn expand_imports(text: &str, file: &Path) -> String {
    let base = file.parent().map(Path::to_path_buf).unwrap_or_default();
    let key = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    let mut chain = vec![key];
    expand_in(text, &base, &mut chain)
}

#[derive(Debug)]
pub struct NestedMemory {
    root: PathBuf,
    loaded: std::sync::Mutex<std::collections::HashSet<PathBuf>>,
    left: std::sync::Mutex<usize>,
}

impl NestedMemory {
    pub fn new(root: &Path) -> Arc<Self> {
        Arc::new(NestedMemory {
            root: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
            loaded: Default::default(),
            left: std::sync::Mutex::new(DEFAULT_BUDGET as usize),
        })
    }

    pub fn set_budget(&self, tokens: usize) {
        if let Ok(mut left) = self.left.lock() {
            *left = tokens;
        }
    }

    pub fn reset(&self) {
        if let Ok(mut loaded) = self.loaded.lock() {
            loaded.clear();
        }
    }

    pub fn loaded(&self) -> Vec<PathBuf> {
        let mut all: Vec<PathBuf> = self.loaded.lock().map(|l| l.iter().cloned().collect()).unwrap_or_default();
        all.sort();
        all
    }

    pub fn reached(&self, file: &Path) -> Option<String> {
        let file = file.canonicalize().ok()?;
        let dir = file.parent()?;
        if !dir.starts_with(&self.root) || dir == self.root {
            return None;
        }
        let mut dirs: Vec<&Path> = dir.ancestors().take_while(|d| *d != self.root.as_path()).collect();
        dirs.reverse();
        let mut blocks = Vec::new();
        for d in dirs {
            let path = d.join(MEMORY_FILE);
            if !path.is_file() {
                continue;
            }
            {
                let Ok(mut loaded) = self.loaded.lock() else { continue };
                if !loaded.insert(path.clone()) {
                    continue;
                }
            }
            let Ok(raw) = std::fs::read_to_string(&path) else { continue };
            let text = expand_imports(raw.trim(), &path);
            if text.trim().is_empty() {
                continue;
            }
            let rel = path.strip_prefix(&self.root).unwrap_or(&path).display().to_string();
            let Ok(mut left) = self.left.lock() else { continue };
            if *left == 0 {
                blocks.push(format!("[{rel} omitted: the {MEMORY_FILE} token budget is used up]"));
                continue;
            }
            let (kept, clipped) = clip(&text, *left);
            *left = left.saturating_sub(estimate_tokens(&kept));
            let scope = d.strip_prefix(&self.root).unwrap_or(d).display().to_string();
            let mut block = format!(
                "<memory source=\"nested\" path=\"{rel}\">\n{}\n</memory>\nThese instructions from {rel} apply to files under {scope}/.",
                kept.trim_end()
            );
            if clipped {
                block.push_str(&format!("\n[{rel} truncated to fit the {MEMORY_FILE} token budget]"));
            }
            blocks.push(block);
        }
        if blocks.is_empty() { None } else { Some(blocks.join("\n\n")) }
    }
}

pub struct WithMemory {
    inner: Box<dyn Tool>,
    memory: Arc<NestedMemory>,
}

impl WithMemory {
    pub fn wrap(inner: Box<dyn Tool>, memory: Arc<NestedMemory>) -> Box<dyn Tool> {
        if MEMORY_TOOLS.contains(&inner.spec().name.as_str()) {
            Box::new(WithMemory { inner, memory })
        } else {
            inner
        }
    }
}

#[async_trait::async_trait]
impl Tool for WithMemory {
    fn spec(&self) -> zarn_core::types::ToolSpec {
        self.inner.spec()
    }

    fn risk(&self) -> zarn_core::tools::Risk {
        self.inner.risk()
    }

    async fn run(&self, args: serde_json::Value, ctx: &zarn_core::tools::Ctx) -> anyhow::Result<zarn_core::tools::ToolOutput> {
        let mut out = self.inner.run(args.clone(), ctx).await?;
        if out.is_error {
            return Ok(out);
        }
        let path = args.get("path").or_else(|| args.get("file_path")).and_then(serde_json::Value::as_str);
        if let Some(path) = path
            && let Some(extra) = self.memory.reached(&ctx.resolve(path))
        {
            out.content.push_str("\n\n");
            out.content.push_str(&extra);
        }
        Ok(out)
    }
}

pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

fn clip(text: &str, tokens: usize) -> (String, bool) {
    if estimate_tokens(text) <= tokens {
        return (text.to_string(), false);
    }
    let keep: String = text.chars().take(tokens * 4).collect();
    (keep, true)
}

#[derive(Clone, Debug, PartialEq)]
pub struct MemoryFile {
    pub label: &'static str,
    pub path: PathBuf,
    pub text: String,
}

pub fn memory_files(project_root: &Path, user_dir: Option<&Path>) -> Vec<MemoryFile> {
    let mut found = Vec::new();
    let candidates = [
        ("project", Some(project_root.join(MEMORY_FILE))),
        ("user", user_dir.map(|d| d.join(MEMORY_FILE))),
    ];
    for (label, path) in candidates {
        let Some(path) = path else { continue };
        if let Ok(text) = std::fs::read_to_string(&path)
            && !text.trim().is_empty()
        {
            let text = expand_imports(&text, &path);
            found.push(MemoryFile { label, path, text });
        }
    }
    found
}

pub fn memory_used(files: &[MemoryFile], budget: u32) -> usize {
    files
        .iter()
        .map(|f| estimate_tokens(f.text.trim()))
        .sum::<usize>()
        .min(budget as usize)
}

pub fn memory_section(files: &[MemoryFile], budget: u32) -> String {
    let mut remaining = budget as usize;
    let mut out = Vec::new();
    for file in files {
        if remaining == 0 {
            out.push(format!(
                "[{} {} omitted: the context budget of {budget} tokens is used up]",
                file.label,
                MEMORY_FILE
            ));
            continue;
        }
        let (text, clipped) = clip(file.text.trim(), remaining);
        remaining = remaining.saturating_sub(estimate_tokens(&text));
        let mut block = format!(
            "<memory source=\"{}\" path=\"{}\">\n{}\n</memory>",
            file.label,
            file.path.display(),
            text.trim_end()
        );
        if clipped {
            block.push_str(&format!(
                "\n[{} {} truncated to fit a {budget} token budget]",
                file.label, MEMORY_FILE
            ));
        }
        out.push(block);
    }
    out.join("\n\n")
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemParts {
    pub base: String,
    pub memory: String,
    pub skills: String,
    pub mode: String,
}

impl SystemParts {
    pub fn join(&self) -> String {
        [&self.base, &self.memory, &self.skills, &self.mode]
            .iter()
            .filter(|p| !p.is_empty())
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

pub fn system_parts_in(cwd: &Path, user_dir: Option<&Path>, skills: &[Skill], budget: u32) -> SystemParts {
    let base = format!(
        "You are zarn, a coding agent working in {0}. Use the tools to read, search, edit and run code. Be concise.\n\n{1} Relative paths in tool calls resolve against {0}; MCP servers may use their own root, so give them absolute paths.",
        cwd.display(),
        zarn_tools::bash::environment_note()
    );
    let memory = memory_section(&memory_files(cwd, user_dir), budget);
    let memory = if memory.is_empty() {
        memory
    } else {
        format!("Instructions from {MEMORY_FILE} files. Follow them.\n\n{memory}")
    };
    SystemParts {
        base,
        memory,
        skills: skill::index(skills),
        mode: String::new(),
    }
}

pub fn system_parts(cwd: &Path, skills: &[Skill], budget: u32) -> SystemParts {
    system_parts_in(cwd, paths::user_dir().as_deref(), skills, budget)
}

pub fn system_prompt(cwd: &Path, skills: &[Skill], budget: u32) -> String {
    system_parts(cwd, skills, budget).join()
}

pub fn breakdown(
    count: &dyn Fn(&[zarn_core::types::Message]) -> u32,
    parts: &SystemParts,
    tools: &[zarn_core::types::ToolSpec],
    messages: &[zarn_core::types::Message],
) -> Vec<(&'static str, u32)> {
    use zarn_core::types::Message;
    let text = |t: &str| {
        if t.is_empty() {
            0
        } else {
            count(&[Message::System(t.to_string())])
        }
    };
    let tool_text = serde_json::to_string(tools).unwrap_or_default();
    let conversation: Vec<Message> = messages
        .iter()
        .filter(|m| !matches!(m, Message::System(_)))
        .cloned()
        .collect();
    vec![
        ("system prompt", text(&parts.base) + text(&parts.mode)),
        ("memory", text(&parts.memory)),
        ("skills index", text(&parts.skills)),
        ("tools", if tools.is_empty() { 0 } else { text(&tool_text) }),
        ("messages", if conversation.is_empty() { 0 } else { count(&conversation) }),
    ]
}

pub fn render_breakdown(rows: &[(&'static str, u32)], window: u32) -> String {
    let total: u32 = rows.iter().map(|(_, n)| n).sum();
    let mut out: Vec<String> = rows
        .iter()
        .map(|(label, n)| format!("{label:<14} {n:>8} tokens"))
        .collect();
    let pct = if window == 0 { 0 } else { u64::from(total) * 100 / u64::from(window) };
    out.push(format!("{:<14} {total:>8} tokens of {window} ({pct}%)", "total"));
    out.join("\n")
}

fn hash_into(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x100_0000_01b3);
    }
}

fn hash_file(hash: &mut u64, path: &Path) {
    hash_into(hash, path.to_string_lossy().as_bytes());
    match std::fs::read(path) {
        Ok(bytes) => hash_into(hash, &bytes),
        Err(_) => hash_into(hash, b"missing"),
    }
}

pub fn fingerprint_in(cwd: &Path, user_dir: Option<&Path>, skill_roots: &[PathBuf]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    hash_file(&mut hash, &cwd.join(MEMORY_FILE));
    if let Some(user) = user_dir {
        hash_file(&mut hash, &user.join(MEMORY_FILE));
    }
    for root in skill_roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        dirs.sort();
        for dir in dirs {
            hash_file(&mut hash, &dir.join("SKILL.md"));
        }
    }
    hash
}

pub fn fingerprint(cwd: &Path) -> u64 {
    let (user, project) = skills_roots();
    let mut roots = vec![project];
    roots.extend(user);
    fingerprint_in(cwd, paths::user_dir().as_deref(), &roots)
}

pub fn skills_roots() -> (Option<PathBuf>, PathBuf) {
    (
        paths::user_dir().map(|d| d.join("skills")),
        paths::project_dir().join("skills"),
    )
}

pub fn discover_skills() -> Vec<Skill> {
    let (user, project) = skills_roots();
    let mut found = skill::discover(user.as_deref(), &project);
    found.extend(crate::plugins::skills(&crate::plugins::installed()));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_and_user_memory_are_both_injected_project_first() {
        let project = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join(MEMORY_FILE), "use tabs").unwrap();
        std::fs::write(user.path().join(MEMORY_FILE), "be terse").unwrap();
        let files = memory_files(project.path(), Some(user.path()));
        let section = memory_section(&files, DEFAULT_BUDGET);
        let p = section.find("use tabs").unwrap();
        let u = section.find("be terse").unwrap();
        assert!(p < u);
    }

    #[test]
    fn memory_over_budget_is_truncated_and_marked() {
        let project = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join(MEMORY_FILE), "x".repeat(4_000)).unwrap();
        std::fs::write(user.path().join(MEMORY_FILE), "never shown").unwrap();
        let files = memory_files(project.path(), Some(user.path()));
        let section = memory_section(&files, 100);
        assert!(section.contains("truncated"));
        assert!(!section.contains("never shown"));
        assert!(section.contains("omitted"));
        assert!(section.contains(&"x".repeat(400)));
        assert!(!section.contains(&"x".repeat(401)));
    }

    #[test]
    fn editing_memory_or_a_skill_changes_the_fingerprint_and_the_prompt() {
        let project = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        let skills = user.path().join("skills");
        std::fs::create_dir_all(skills.join("pdf")).unwrap();
        std::fs::write(skills.join("pdf/SKILL.md"), "---\nname: pdf\ndescription: a\n---\n").unwrap();
        std::fs::write(project.path().join(MEMORY_FILE), "use tabs").unwrap();
        let roots = vec![skills.clone()];
        let first = fingerprint_in(project.path(), Some(user.path()), &roots);
        assert_eq!(first, fingerprint_in(project.path(), Some(user.path()), &roots));

        std::fs::write(project.path().join(MEMORY_FILE), "use spaces").unwrap();
        let second = fingerprint_in(project.path(), Some(user.path()), &roots);
        assert_ne!(first, second);
        let prompt = system_parts_in(project.path(), Some(user.path()), &[], DEFAULT_BUDGET).join();
        assert!(prompt.contains("use spaces"));

        std::fs::write(skills.join("pdf/SKILL.md"), "---\nname: pdf\ndescription: b\n---\n").unwrap();
        assert_ne!(second, fingerprint_in(project.path(), Some(user.path()), &roots));

        std::fs::write(user.path().join(MEMORY_FILE), "user rule").unwrap();
        let third = fingerprint_in(project.path(), Some(user.path()), &roots);
        std::fs::remove_file(user.path().join(MEMORY_FILE)).unwrap();
        assert_ne!(third, fingerprint_in(project.path(), Some(user.path()), &roots));
    }

    #[test]
    fn the_breakdown_counts_each_part_separately() {
        let parts = SystemParts {
            base: "abcd".repeat(10),
            memory: "m".repeat(40),
            skills: String::new(),
            mode: String::new(),
        };
        let count = |msgs: &[zarn_core::types::Message]| -> u32 {
            msgs.iter().map(|m| m.text().len() as u32 / 4).sum()
        };
        let messages = vec![
            zarn_core::types::Message::System(parts.join()),
            zarn_core::types::Message::user("x".repeat(80)),
        ];
        let rows = breakdown(&count, &parts, &[], &messages);
        assert_eq!(rows[0], ("system prompt", 10));
        assert_eq!(rows[1], ("memory", 10));
        assert_eq!(rows[2], ("skills index", 0));
        assert_eq!(rows[3], ("tools", 0));
        assert_eq!(rows[4], ("messages", 20));
        assert!(render_breakdown(&rows, 400).contains("40 tokens of 400 (10%)"));
    }

    #[test]
    fn the_system_prompt_lists_skills_by_name_and_description_only() {
        let dir = tempfile::tempdir().unwrap();
        let skill = Skill {
            name: "pdf".into(),
            description: "fill PDF forms".into(),
            dir: dir.path().to_path_buf(),
            source: zarn_tools::skill::Source::User,
        };
        std::fs::write(dir.path().join("SKILL.md"), "---\nname: pdf\ndescription: fill PDF forms\n---\nSECRET BODY").unwrap();
        let prompt = system_prompt(dir.path(), &[skill], DEFAULT_BUDGET);
        assert!(prompt.contains("pdf: fill PDF forms"));
        assert!(!prompt.contains("SECRET BODY"));
    }
}
