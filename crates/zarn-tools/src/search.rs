use crate::budget;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use globset::GlobBuilder;
use ignore::WalkBuilder;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use zarn_core::tools::{Ctx, Risk, Tool, ToolOutput};
use zarn_core::types::ToolSpec;

const MAX_HITS: usize = 500;

fn walk(root: &Path) -> impl Iterator<Item = PathBuf> {
    WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| e.into_path())
}

fn root_of(args: &Value, ctx: &Ctx) -> PathBuf {
    args.get("path")
        .and_then(Value::as_str)
        .map(|p| ctx.resolve(p))
        .unwrap_or_else(|| ctx.cwd().to_path_buf())
}

fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

pub struct Glob;

impl Glob {
    pub fn new() -> Self {
        Glob
    }
}

impl Default for Glob {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for Glob {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "glob".to_string(),
            description:
                "Find files matching a glob pattern such as *.rs or src/**/*.rs, newest first. Respects .gitignore."
                    .to_string(),
            schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern, e.g. src/**/*.rs" },
                    "path": { "type": "string", "description": "Directory to search, defaults to the working directory" }
                },
                "required": ["pattern"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Safe
    }

    async fn run(&self, args: Value, ctx: &Ctx) -> Result<ToolOutput> {
        let pattern = args
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("pattern is required"))?
            .to_string();
        let root = root_of(&args, ctx);

        let matcher = match GlobBuilder::new(&pattern)
            .literal_separator(true)
            .build()
            .map(|g| g.compile_matcher())
        {
            Ok(matcher) => matcher,
            Err(err) => return Ok(ToolOutput::error(format!("bad pattern {pattern}: {err}"))),
        };

        let found = tokio::task::spawn_blocking(move || {
            let mut hits: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
            for path in walk(&root) {
                let candidate = relative(&path, &root);
                if !matcher.is_match(&candidate) && !matcher.is_match(&path) {
                    continue;
                }
                let modified = path
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                hits.push((modified, path));
            }
            hits.sort_by_key(|(time, _)| std::cmp::Reverse(*time));
            hits
        })
        .await?;

        if found.is_empty() {
            return Ok(ToolOutput::ok(format!("no files match {pattern}")));
        }

        let listed: Vec<String> = found
            .iter()
            .take(MAX_HITS)
            .map(|(_, p)| p.to_string_lossy().to_string())
            .collect();

        let mut text = listed.join("\n");
        if found.len() > MAX_HITS {
            text.push_str(&format!(
                "\n[{} more matches not shown]",
                found.len() - MAX_HITS
            ));
        }
        Ok(ToolOutput::ok(budget::truncate(
            &text,
            budget::DEFAULT_TOKEN_BUDGET,
        )))
    }
}

pub struct Grep;

impl Grep {
    pub fn new() -> Self {
        Grep
    }
}

impl Default for Grep {
    fn default() -> Self {
        Self::new()
    }
}

async fn ripgrep(pattern: &str, root: &Path) -> Option<std::process::Output> {
    tokio::process::Command::new("rg")
        .args(["--line-number", "--no-heading", "--color=never", "--", pattern])
        .arg(root)
        .output()
        .await
        .ok()
}

fn grep_fallback(pattern: &str, root: &Path) -> Result<Vec<String>, String> {
    let regex = regex::Regex::new(pattern).map_err(|e| e.to_string())?;
    let mut hits = Vec::new();

    for path in walk(root) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            if regex.is_match(line) {
                hits.push(format!(
                    "{}:{}:{}",
                    path.to_string_lossy(),
                    index + 1,
                    line.trim_end()
                ));
                if hits.len() >= MAX_HITS {
                    return Ok(hits);
                }
            }
        }
    }
    Ok(hits)
}

#[async_trait]
impl Tool for Grep {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".to_string(),
            description:
                "Search file contents for a regular expression. Returns path:line:text. Respects .gitignore."
                    .to_string(),
            schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regular expression to search for" },
                    "path": { "type": "string", "description": "Directory to search, defaults to the working directory" }
                },
                "required": ["pattern"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Safe
    }

    async fn run(&self, args: Value, ctx: &Ctx) -> Result<ToolOutput> {
        let pattern = args
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("pattern is required"))?
            .to_string();
        let root = root_of(&args, ctx);

        if let Some(output) = ripgrep(&pattern, &root).await {
            match output.status.code() {
                Some(0) => {
                    let text = String::from_utf8_lossy(&output.stdout).to_string();
                    return Ok(ToolOutput::ok(budget::truncate(
                        &text,
                        budget::DEFAULT_TOKEN_BUDGET,
                    )));
                }
                Some(1) => {
                    return Ok(ToolOutput::ok(format!("no matches for {pattern}")));
                }
                _ => {
                    let err = String::from_utf8_lossy(&output.stderr).to_string();
                    return Ok(ToolOutput::error(format!("ripgrep failed: {}", err.trim())));
                }
            }
        }

        let searched = pattern.clone();
        let hits = tokio::task::spawn_blocking(move || grep_fallback(&searched, &root)).await?;

        match hits {
            Err(err) => Ok(ToolOutput::error(format!("bad pattern {pattern}: {err}"))),
            Ok(hits) if hits.is_empty() => {
                Ok(ToolOutput::ok(format!("no matches for {pattern}")))
            }
            Ok(hits) => Ok(ToolOutput::ok(budget::truncate(
                &hits.join("\n"),
                budget::DEFAULT_TOKEN_BUDGET,
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(tag: &str) -> Ctx {
        let dir = std::env::temp_dir().join(format!(
            "zarn-search-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        std::fs::write(dir.join("top.rs"), "fn top() {}\n").unwrap();
        std::fs::write(dir.join("src/mid.rs"), "fn mid() {}\n").unwrap();
        std::fs::write(dir.join("src/deep/low.rs"), "fn low() {}\nneedle here\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "needle here too\n").unwrap();
        Ctx::new(dir)
    }

    fn cleanup(ctx: &Ctx) {
        std::fs::remove_dir_all(ctx.cwd()).ok();
    }

    #[tokio::test]
    async fn a_flat_pattern_matches_only_the_top_level() {
        let ctx = tree("flat");
        let out = Glob.run(json!({ "pattern": "*.rs" }), &ctx).await.unwrap();
        assert!(out.content.contains("top.rs"));
        assert!(!out.content.contains("mid.rs"));
        cleanup(&ctx);
    }

    #[tokio::test]
    async fn a_double_star_pattern_reaches_nested_directories() {
        let ctx = tree("deep");
        let out = Glob
            .run(json!({ "pattern": "src/**/*.rs" }), &ctx)
            .await
            .unwrap();
        assert!(out.content.contains("low.rs"));
        assert!(!out.is_error);
        cleanup(&ctx);
    }

    #[tokio::test]
    async fn a_pattern_with_regex_metacharacters_is_treated_as_a_glob() {
        let ctx = tree("meta");
        let out = Glob.run(json!({ "pattern": "*.txt" }), &ctx).await.unwrap();
        assert!(out.content.contains("notes.txt"));
        assert!(!out.content.contains(".rs"));
        cleanup(&ctx);
    }

    #[tokio::test]
    async fn an_invalid_pattern_is_an_error_not_a_panic() {
        let ctx = tree("bad");
        let out = Glob.run(json!({ "pattern": "a[" }), &ctx).await.unwrap();
        assert!(out.is_error);
        cleanup(&ctx);
    }

    #[tokio::test]
    async fn grep_finds_matches_with_line_numbers() {
        let ctx = tree("grep");
        let out = Grep.run(json!({ "pattern": "needle" }), &ctx).await.unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("needle"));
        assert!(out.content.contains(":2:") || out.content.contains(":1:"));
        cleanup(&ctx);
    }

    #[tokio::test]
    async fn grep_reports_no_matches_rather_than_failing() {
        let ctx = tree("nomatch");
        let out = Grep
            .run(json!({ "pattern": "zzzzznotfound" }), &ctx)
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("no matches"));
        cleanup(&ctx);
    }

    #[test]
    fn the_fallback_agrees_with_the_documented_output_shape() {
        let ctx = tree("fallback");
        let hits = grep_fallback("needle", ctx.cwd()).unwrap();
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|h| h.matches(':').count() >= 2));
        cleanup(&ctx);
    }
}
