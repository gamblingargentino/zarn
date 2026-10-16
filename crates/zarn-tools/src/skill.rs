use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use zarn_core::tools::{Ctx, Risk, Tool, ToolOutput};
use zarn_core::types::ToolSpec;

pub const FILE: &str = "SKILL.md";
const MAX_LISTED_FILES: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    User,
    Project,
}

impl Source {
    pub fn label(&self) -> &'static str {
        match self {
            Source::User => "user",
            Source::Project => "project",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
    pub source: Source,
}

impl Skill {
    pub fn path(&self) -> PathBuf {
        self.dir.join(FILE)
    }

    pub fn body(&self) -> Result<String> {
        Ok(std::fs::read_to_string(self.path())?)
    }

    pub fn instructions(&self) -> Result<String> {
        let text = self.body()?;
        Ok(match split_frontmatter(&text) {
            Some((_, body)) => body.trim().to_string(),
            None => text.trim().to_string(),
        })
    }

    pub fn supporting_files(&self) -> Vec<String> {
        let mut found = Vec::new();
        collect(&self.dir, &self.dir, &mut found);
        found.sort();
        found.truncate(MAX_LISTED_FILES);
        found
    }
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if out.len() >= MAX_LISTED_FILES {
            return;
        }
        if path.is_dir() {
            collect(root, &path, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            if rel != FILE {
                out.push(rel);
            }
        }
    }
}

fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text
        .strip_prefix("---\r\n")
        .or_else(|| text.strip_prefix("---\n"))?;
    let end = rest.find("\n---")?;
    let front = &rest[..end];
    let after = &rest[end + 4..];
    let body = after.split_once('\n').map(|(_, b)| b).unwrap_or("");
    Some((front, body))
}

fn unquote(value: &str) -> String {
    let v = value.trim();
    let stripped = v
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')));
    stripped.unwrap_or(v).to_string()
}

pub fn parse(text: &str) -> Option<(Option<String>, String)> {
    let (front, _) = split_frontmatter(text)?;
    let mut name = None;
    let mut description = String::new();
    let lines: Vec<&str> = front.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let value = if matches!(value, "|" | ">" | "|-" | ">-" | "|+" | ">+") {
            let mut parts = Vec::new();
            while i < lines.len() && (lines[i].starts_with(char::is_whitespace) || lines[i].is_empty())
            {
                parts.push(lines[i].trim());
                i += 1;
            }
            let sep = if value.starts_with('|') { "\n" } else { " " };
            parts.join(sep).trim().to_string()
        } else {
            unquote(value)
        };
        match key.trim() {
            "name" => name = Some(value).filter(|v| !v.is_empty()),
            "description" => description = value,
            _ => {}
        }
    }
    Some((name, description))
}

fn read_root(root: &Path, source: Source) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|dir| dir.join(FILE).is_file())
        .filter_map(|dir| {
            let text = std::fs::read_to_string(dir.join(FILE)).ok()?;
            let folder = dir.file_name()?.to_string_lossy().to_string();
            let (name, description) = match parse(&text) {
                Some(parsed) => parsed,
                None => {
                    tracing::warn!(path = %dir.display(), "skill has no frontmatter");
                    (None, String::new())
                }
            };
            Some(Skill {
                name: name.unwrap_or(folder),
                description,
                dir,
                source,
            })
        })
        .collect()
}

pub fn discover(user_root: Option<&Path>, project_root: &Path) -> Vec<Skill> {
    let mut skills: Vec<Skill> = Vec::new();
    let user = user_root
        .map(|root| read_root(root, Source::User))
        .unwrap_or_default();
    for skill in user.into_iter().chain(read_root(project_root, Source::Project)) {
        skills.retain(|s| s.name != skill.name);
        skills.push(skill);
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

pub fn index(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let listed = skills
        .iter()
        .map(|s| format!("- {}: {}", s.name, s.description))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Skills are packaged instructions for specific tasks. When a task matches one, call the skill tool with its name before starting, and follow what it returns.\n{listed}"
    )
}

pub fn render(skill: &Skill, arguments: &str) -> Result<String> {
    let mut out = format!(
        "<skill name=\"{}\" dir=\"{}\">\n{}\n</skill>",
        skill.name,
        skill.dir.display(),
        skill.instructions()?
    );
    let files = skill.supporting_files();
    if !files.is_empty() {
        out.push_str(&format!(
            "\nSupporting files, relative to {}:\n{}",
            skill.dir.display(),
            files
                .iter()
                .map(|f| format!("- {f}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if !arguments.trim().is_empty() {
        out.push_str(&format!("\n\nArguments: {}", arguments.trim()));
    }
    Ok(out)
}

pub struct SkillTool {
    skills: Vec<Skill>,
}

impl SkillTool {
    pub fn new(skills: Vec<Skill>) -> Self {
        SkillTool { skills }
    }

    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "skill".to_string(),
            description: "Load the full instructions of a skill listed in the system prompt. Call it when a task matches a skill's description, then follow the instructions it returns.".to_string(),
            schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The skill to load",
                        "enum": self.skills.iter().map(|s| s.name.clone()).collect::<Vec<_>>()
                    }
                },
                "required": ["name"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Safe
    }

    async fn run(&self, args: Value, ctx: &Ctx) -> Result<ToolOutput> {
        let Some(name) = args.get("name").and_then(Value::as_str) else {
            return Ok(ToolOutput::error("skill requires a name"));
        };
        let Some(skill) = self.skills.iter().find(|s| s.name == name) else {
            let known = self
                .skills
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Ok(ToolOutput::error(format!(
                "no skill named {name}; available: {known}"
            )));
        };
        ctx.mark_read(skill.path());
        match render(skill, "") {
            Ok(text) => Ok(ToolOutput::ok(text)),
            Err(err) => Ok(ToolOutput::error(format!("could not read {name}: {err}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_gives_name_and_description() {
        let (name, description) =
            parse("---\nname: pdf\ndescription: \"Work with PDFs\"\n---\nbody").unwrap();
        assert_eq!(name.as_deref(), Some("pdf"));
        assert_eq!(description, "Work with PDFs");
    }

    #[test]
    fn a_folded_description_is_joined() {
        let (_, description) =
            parse("---\nname: x\ndescription: >\n  first line\n  second line\n---\n").unwrap();
        assert_eq!(description, "first line second line");
    }

    #[test]
    fn crlf_frontmatter_parses() {
        let (name, _) = parse("---\r\nname: win\r\ndescription: d\r\n---\r\nbody").unwrap();
        assert_eq!(name.as_deref(), Some("win"));
    }

    #[test]
    fn no_frontmatter_is_none() {
        assert!(parse("# just markdown").is_none());
    }
}
