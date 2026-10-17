use crate::paths;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Style {
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub path: Option<PathBuf>,
}

pub const DEFAULT: &str = "default";

const BUILTIN: [(&str, &str, &str); 4] = [
    (DEFAULT, "zarn's normal replies", ""),
    (
        "explanatory",
        "explains the reasons behind its choices while it works",
        "Output style: explanatory. While you work, explain your choices so the user learns the codebase and the craft. Before or after a meaningful step, add a short note headed 'Insight' with two or three points about why you chose this approach, the patterns or conventions the code follows, and the trade-offs you weighed. Keep insights specific to this code, not general advice, and keep them brief so the work stays the focus.",
    ),
    (
        "learning",
        "teaches by asking you to write small, meaningful pieces yourself",
        "Output style: learning. Help the user learn by doing. Do the routine work yourself, but when you reach a small piece of real decision-making, such as a core condition, a design choice or an error-handling policy of about 5 to 10 lines, stop and ask the user to write it. Prepare the surrounding code, mark the spot with a TODO(human) line, explain what the piece must do and the trade-offs, and wait for their answer before continuing. Ask for at most one such piece per reply, and give brief 'Insight' notes about why the code is shaped the way it is.",
    ),
    (
        "concise",
        "short answers, no preamble or recap",
        "Output style: concise. Answer in as few words as the task allows. No preamble, no restating the question, no closing summary or offers of more help. Prefer a code block or a short list over prose. Report results, not process.",
    ),
];

pub fn builtin_names() -> Vec<&'static str> {
    BUILTIN.iter().map(|(name, _, _)| *name).collect()
}

fn builtins() -> Vec<Style> {
    BUILTIN
        .iter()
        .map(|(name, description, prompt)| Style {
            name: name.to_string(),
            description: description.to_string(),
            prompt: prompt.to_string(),
            path: None,
        })
        .collect()
}

pub fn parse(name: &str, text: &str) -> Style {
    let text = text.trim_start_matches('\u{feff}');
    let mut description = String::new();
    let mut body = text;
    if let Some(rest) = text.strip_prefix("---")
        && let Some(end) = rest.find("\n---")
    {
        for line in rest[..end].lines() {
            if let Some((key, value)) = line.split_once(':')
                && key.trim() == "description"
            {
                description = value.trim().trim_matches('"').trim_matches('\'').to_string();
            }
        }
        body = rest[end..].trim_start_matches("\n---");
    }
    let prompt = body.trim().to_string();
    if description.is_empty() {
        description = prompt.lines().next().unwrap_or("").chars().take(70).collect();
    }
    Style { name: name.to_string(), description, prompt, path: None }
}

fn read_dir(dir: &Path, out: &mut Vec<Style>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    files.sort();
    for path in files {
        let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(str::to_lowercase) else {
            continue;
        };
        if !crate::catalog::valid_name(&name) || name == DEFAULT {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut style = parse(&name, &text);
        style.path = Some(path);
        out.retain(|s| s.name != name);
        out.push(style);
    }
}

pub fn load_from(user: Option<&Path>, project: &Path) -> Vec<Style> {
    let mut styles = builtins();
    if let Some(user) = user {
        read_dir(user, &mut styles);
    }
    read_dir(project, &mut styles);
    styles
}

pub fn load() -> Vec<Style> {
    let user = paths::user_dir().map(|d| d.join("styles"));
    load_from(user.as_deref(), &paths::project_dir().join("styles"))
}

pub fn find(styles: &[Style], name: &str) -> Option<Style> {
    styles.iter().find(|s| s.name.eq_ignore_ascii_case(name.trim())).cloned()
}

pub fn section(styles: &[Style], chosen: Option<&str>) -> String {
    chosen
        .and_then(|name| find(styles, name))
        .map(|style| style.prompt)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_come_first_and_files_add_or_replace_styles() {
        let user = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(user.path().join("pirate.md"), "Talk like a pirate.").unwrap();
        std::fs::write(
            project.path().join("Pirate.md"),
            "---\ndescription: \"project pirate\"\n---\nTalk like a project pirate.",
        )
        .unwrap();
        std::fs::write(project.path().join("default.md"), "cannot replace the default").unwrap();
        std::fs::write(project.path().join("notes.txt"), "ignored").unwrap();
        let styles = load_from(Some(user.path()), project.path());
        let names: Vec<&str> = styles.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["default", "explanatory", "learning", "concise", "pirate"]);
        let pirate = find(&styles, "PIRATE").unwrap();
        assert_eq!(pirate.description, "project pirate");
        assert_eq!(pirate.prompt, "Talk like a project pirate.");
        assert!(section(&styles, Some("default")).is_empty());
        assert!(section(&styles, None).is_empty());
        assert!(section(&styles, Some("missing")).is_empty());
        assert!(section(&styles, Some("explanatory")).starts_with("Output style: explanatory."));
        assert_eq!(parse("x", "First line here\nmore").description, "First line here");
    }
}
