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

pub fn system_prompt(cwd: &Path, skills: &[Skill], budget: u32) -> String {
    let mut parts = vec![format!(
        "You are zarn, a coding agent working in {}. Use the tools to read, search, edit and run code. Be concise.",
        cwd.display()
    )];
    let memory = memory_section(&memory_files(cwd, paths::user_dir().as_deref()), budget);
    if !memory.is_empty() {
        parts.push(format!(
            "Instructions from {MEMORY_FILE} files. Follow them.\n\n{memory}"
        ));
    }
    let index = skill::index(skills);
    if !index.is_empty() {
        parts.push(index);
    }
    parts.join("\n\n")
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
