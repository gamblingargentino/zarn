pub const FIELDS: [&str; 7] = ["model", "mode", "profile", "context", "cost", "cwd", "branch"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    Text(String),
    Field(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    pub source: String,
    parts: Vec<Part>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Values {
    pub model: String,
    pub mode: String,
    pub profile: String,
    pub context: String,
    pub cost: String,
    pub cwd: String,
    pub branch: String,
}

impl Values {
    fn get(&self, field: &str) -> &str {
        match field {
            "model" => &self.model,
            "mode" => &self.mode,
            "profile" => &self.profile,
            "context" => &self.context,
            "cost" => &self.cost,
            "cwd" => &self.cwd,
            _ => &self.branch,
        }
    }
}

impl Template {
    pub fn parse(source: &str) -> Result<Template, String> {
        let mut parts = Vec::new();
        let mut text = String::new();
        let mut chars = source.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    text.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    text.push('}');
                }
                '{' => {
                    let mut name = String::new();
                    let mut closed = false;
                    for n in chars.by_ref() {
                        if n == '}' {
                            closed = true;
                            break;
                        }
                        name.push(n);
                    }
                    if !closed {
                        return Err(format!("'{{{name}' is not closed with }}"));
                    }
                    let Some(field) = FIELDS.iter().find(|f| **f == name.trim()) else {
                        return Err(format!(
                            "{{{name}}} is not a status line field; use {}",
                            FIELDS.map(|f| format!("{{{f}}}")).join(", ")
                        ));
                    };
                    if !text.is_empty() {
                        parts.push(Part::Text(std::mem::take(&mut text)));
                    }
                    parts.push(Part::Field(field));
                }
                '}' => return Err("a lone } needs to be written as }}".into()),
                '\n' | '\r' => return Err("the status line is a single line".into()),
                other => text.push(other),
            }
        }
        if !text.is_empty() {
            parts.push(Part::Text(text));
        }
        Ok(Template {
            source: source.to_string(),
            parts,
        })
    }

    pub fn render(&self, values: &Values) -> String {
        self.parts
            .iter()
            .map(|p| match p {
                Part::Text(t) => t.as_str(),
                Part::Field(f) => values.get(f),
            })
            .collect()
    }
}

pub fn preview(source: &str, values: &Values) -> Result<String, String> {
    if source.trim().is_empty() {
        return Ok("(empty: the built-in status line is used)".into());
    }
    Template::parse(source).map(|t| t.render(values))
}

pub fn branch(cwd: &std::path::Path) -> String {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let git = d.join(".git");
        let head = if git.is_file() {
            std::fs::read_to_string(&git)
                .ok()
                .and_then(|s| s.trim().strip_prefix("gitdir:").map(|p| d.join(p.trim()).join("HEAD")))
                .and_then(|p| std::fs::read_to_string(p).ok())
        } else {
            std::fs::read_to_string(git.join("HEAD")).ok()
        };
        if let Some(head) = head {
            let head = head.trim();
            return match head.strip_prefix("ref: refs/heads/") {
                Some(name) => name.to_string(),
                None => head.chars().take(7).collect(),
            };
        }
        dir = d.parent();
    }
    String::new()
}

pub fn short_path(path: &std::path::Path, home: Option<&std::path::Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> Values {
        Values {
            model: "m1".into(),
            mode: "plan".into(),
            profile: "work".into(),
            context: "12%".into(),
            cost: "$0.0100".into(),
            cwd: "~/proj".into(),
            branch: "main".into(),
        }
    }

    #[test]
    fn every_field_renders() {
        let t = Template::parse("{model} {mode} {profile} {context} {cost} {cwd} {branch} {{x}}").unwrap();
        assert_eq!(t.render(&values()), "m1 plan work 12% $0.0100 ~/proj main {x}");
    }

    #[test]
    fn invalid_templates_explain_themselves() {
        assert!(Template::parse("{nope}").unwrap_err().contains("not a status line field"));
        assert!(Template::parse("{model").unwrap_err().contains("not closed"));
        assert!(Template::parse("a } b").unwrap_err().contains("}}"));
        assert!(Template::parse("a\nb").is_err());
    }

    #[test]
    fn the_branch_comes_from_head() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        assert_eq!(branch(&dir.path().join("sub")), "feature/x");
        std::fs::write(dir.path().join(".git/HEAD"), "0123456789abcdef\n").unwrap();
        assert_eq!(branch(dir.path()), "0123456");
    }

    #[test]
    fn paths_under_home_are_shortened() {
        let home = std::path::Path::new("/h");
        assert_eq!(short_path(std::path::Path::new("/h/p"), Some(home)), "~/p");
        assert_eq!(short_path(std::path::Path::new("/x"), Some(home)), "/x");
    }
}
