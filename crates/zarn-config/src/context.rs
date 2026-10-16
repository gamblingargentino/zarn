use crate::paths;
use std::path::{Path, PathBuf};
use zarn_tools::skill::{self, Skill};

pub const MEMORY_FILE: &str = "ZARN.md";
pub const DEFAULT_BUDGET: u32 = 8_000;

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
            found.push(MemoryFile { label, path, text });
        }
    }
    found
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
        "You are zarn, a coding agent working in {}. Use the tools to read, search, edit and run code. Be concise.",
        cwd.display()
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
    skill::discover(user.as_deref(), &project)
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
