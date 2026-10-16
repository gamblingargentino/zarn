use std::path::{Component, Path, PathBuf};
use zarn_core::types::Content;

pub const MAX_FILES: usize = 5_000;
pub const ATTACH_BYTES: usize = 50_000;
pub const IMAGE_BYTES: u64 = 5 * 1024 * 1024;

pub fn list_files(cwd: &Path) -> Vec<String> {
    let mut files: Vec<String> = ignore::WalkBuilder::new(cwd)
        .hidden(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|e| {
            e.path()
                .strip_prefix(cwd)
                .ok()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
        })
        .take(MAX_FILES)
        .collect();
    files.sort();
    files
}

pub fn score(query: &str, candidate: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let c: Vec<char> = candidate.to_lowercase().chars().collect();
    let name_start = candidate.rfind('/').map(|i| i + 1).unwrap_or(0);
    let mut score: i64 = 0;
    let mut qi = 0;
    let mut last: Option<usize> = None;
    for (i, ch) in c.iter().enumerate() {
        if qi < q.len() && *ch == q[qi] {
            score += 1;
            if last.is_some_and(|l| l + 1 == i) {
                score += 5;
            }
            if i == 0 || matches!(c[i - 1], '/' | '_' | '-' | '.') {
                score += 3;
            }
            if i >= name_start {
                score += 2;
            }
            last = Some(i);
            qi += 1;
        }
    }
    if qi < q.len() {
        return None;
    }
    if candidate.to_lowercase().contains(&query.to_lowercase()) {
        score += 20;
    }
    Some(score * 1000 - candidate.len() as i64)
}

pub fn filter<'a>(files: &'a [String], query: &str, limit: usize) -> Vec<&'a String> {
    let mut scored: Vec<(i64, &String)> = files
        .iter()
        .filter_map(|f| score(query, f).map(|s| (s, f)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    scored.into_iter().take(limit).map(|(_, f)| f).collect()
}

pub fn token_at(input: &str, cursor: usize) -> Option<(usize, String)> {
    let chars: Vec<char> = input.chars().collect();
    let end = cursor.min(chars.len());
    let mut start = end;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    let word: String = chars[start..end].iter().collect();
    let rest = word.strip_prefix('@')?;
    if rest.contains(':') && !rest.contains('/') && rest.split(':').next().is_some_and(|s| !s.is_empty()) {
        return None;
    }
    Some((start, rest.to_string()))
}

pub fn mentioned(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for word in text.split_whitespace() {
        let Some(rest) = word.strip_prefix('@') else {
            continue;
        };
        let rest = rest.trim_end_matches([',', '.', ';', ')', '!', '?']);
        if !rest.is_empty() && !found.iter().any(|f: &String| f == rest) {
            found.push(rest.to_string());
        }
    }
    found
}

pub fn inside(cwd: &Path, path: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    let full = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
    let mut parts: Vec<Component> = Vec::new();
    for part in full.components() {
        match part {
            Component::ParentDir => {
                parts.pop();
            }
            Component::CurDir => {}
            other => parts.push(other),
        }
    }
    let normal: PathBuf = parts.iter().collect();
    let real = normal.canonicalize().ok()?;
    let root = cwd.canonicalize().ok()?;
    real.starts_with(&root).then_some(real)
}

pub fn image_mime(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    if lower.ends_with(".png") {
        Some("image/png")
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if lower.ends_with(".gif") {
        Some("image/gif")
    } else if lower.ends_with(".webp") {
        Some("image/webp")
    } else {
        None
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Expanded {
    pub content: Vec<Content>,
    pub notes: Vec<String>,
    pub attached: Vec<String>,
}

pub fn expand(text: &str, cwd: &Path, vision: bool, cap: usize) -> Expanded {
    let mut out = Expanded::default();
    let mut blocks = Vec::new();
    for path in mentioned(text) {
        if path.contains(':') && !path.contains('/') && !Path::new(&path).exists() {
            continue;
        }
        let Some(real) = inside(cwd, &path) else {
            if cwd.join(&path).exists() || Path::new(&path).is_absolute() {
                out.notes.push(format!(
                    "@{path} is outside the working directory; it was not attached"
                ));
            }
            continue;
        };
        if real.is_dir() {
            continue;
        }
        if let Some(mime) = image_mime(&path) {
            if !vision {
                out.notes.push(format!(
                    "@{path} is an image, but this model does not accept images; it was not attached"
                ));
                continue;
            }
            match std::fs::metadata(&real) {
                Ok(meta) if meta.len() > IMAGE_BYTES => {
                    out.notes.push(format!("@{path} is over 5 MB; it was not attached"));
                }
                Ok(_) => match std::fs::read(&real) {
                    Ok(data) => {
                        out.content.push(Content::Image {
                            mime: mime.to_string(),
                            data,
                        });
                        out.attached.push(path);
                    }
                    Err(err) => out.notes.push(format!("@{path} could not be read: {err}")),
                },
                Err(err) => out.notes.push(format!("@{path} could not be read: {err}")),
            }
            continue;
        }
        if path.to_lowercase().ends_with(".pdf") {
            out.notes.push(format!(
                "@{path} is a PDF; zarn cannot extract PDF text yet, so it was not attached"
            ));
            continue;
        }
        let bytes = match std::fs::read(&real) {
            Ok(bytes) => bytes,
            Err(err) => {
                out.notes.push(format!("@{path} could not be read: {err}"));
                continue;
            }
        };
        let Ok(body) = String::from_utf8(bytes) else {
            out.notes.push(format!("@{path} is not text; it was not attached"));
            continue;
        };
        let total = body.len();
        let mut end = total.min(cap);
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        let mut block = format!("<file path=\"{path}\">\n{}", &body[..end]);
        if end < total {
            block.push_str(&format!(
                "\n[truncated: showing the first {end} of {total} bytes; read the file for the rest]"
            ));
            out.notes.push(format!("@{path} was truncated to {end} of {total} bytes"));
        }
        block.push_str("\n</file>");
        blocks.push(block);
        out.attached.push(path);
    }
    let mut message = text.to_string();
    if !blocks.is_empty() {
        message.push_str("\n\nAttached files:\n");
        message.push_str(&blocks.join("\n"));
    }
    out.content.insert(0, Content::Text(message));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for path in [
            "src/main.rs",
            "src/mentions.rs",
            "src/deep/menu_item.rs",
            "README.md",
            "target/debug/build.log",
            "notes/secret.txt",
        ] {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            std::fs::write(root.join(path), path).unwrap();
        }
        std::fs::write(root.join(".gitignore"), "target/\nnotes/secret.txt\n").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref").unwrap();
        dir
    }

    #[test]
    fn listing_respects_gitignore_and_skips_git() {
        let dir = tree();
        let files = list_files(dir.path());
        assert!(files.contains(&"src/main.rs".to_string()));
        assert!(files.contains(&".gitignore".to_string()));
        assert!(!files.iter().any(|f| f.starts_with("target/")));
        assert!(!files.iter().any(|f| f.contains("secret")));
        assert!(!files.iter().any(|f| f.starts_with(".git/")));
    }

    #[test]
    fn fuzzy_filtering_ranks_tight_matches_first() {
        let files: Vec<String> = ["src/main.rs", "src/mentions.rs", "src/deep/menu_item.rs", "README.md"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let found = filter(&files, "ment", 10);
        assert_eq!(found[0], "src/mentions.rs");
        assert!(filter(&files, "mnrs", 10).iter().any(|f| *f == "src/main.rs"));
        assert!(filter(&files, "zzz", 10).is_empty());
        assert_eq!(filter(&files, "", 2).len(), 2);
    }

    #[test]
    fn the_token_under_the_cursor_is_found() {
        assert_eq!(token_at("look at @src/ma", 15), Some((8, "src/ma".into())));
        assert_eq!(token_at("plain words", 5), None);
        assert_eq!(token_at("@", 1), Some((0, String::new())));
    }

    #[test]
    fn mentioned_files_are_attached_with_a_cap_and_a_note() {
        let dir = tree();
        std::fs::write(dir.path().join("big.txt"), "y".repeat(500)).unwrap();
        let out = expand("check @README.md and @big.txt.", dir.path(), false, 100);
        let Content::Text(text) = &out.content[0] else { panic!() };
        assert!(text.contains("<file path=\"README.md\">\nREADME.md\n</file>"));
        assert!(text.contains("[truncated: showing the first 100 of 500 bytes"));
        assert!(!text.contains(&"y".repeat(101)));
        assert_eq!(out.attached, vec!["README.md", "big.txt"]);
        assert!(out.notes[0].contains("truncated"));
    }

    #[test]
    fn paths_outside_the_working_directory_are_refused() {
        let dir = tree();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let text = format!("read @{} and @../../etc/passwd", outside.path().display());
        let out = expand(&text, &dir.path().join("src"), false, 1000);
        assert!(out.attached.is_empty());
        assert!(out.notes.iter().all(|n| n.contains("outside the working directory")));
        assert!(!out.notes.is_empty());
    }

    #[test]
    fn images_attach_only_for_vision_models() {
        let dir = tree();
        std::fs::write(dir.path().join("shot.png"), [137u8, 80, 78, 71]).unwrap();
        let with = expand("see @shot.png", dir.path(), true, 1000);
        assert!(matches!(&with.content[1], Content::Image { mime, data } if mime == "image/png" && data.len() == 4));
        let without = expand("see @shot.png", dir.path(), false, 1000);
        assert_eq!(without.content.len(), 1);
        assert!(without.notes[0].contains("does not accept images"));
    }
}
