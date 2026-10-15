use crate::fsx;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use zarn_core::tools::{Ctx, Risk, Tool, ToolOutput};
use zarn_core::types::ToolSpec;

pub struct Edit;

impl Edit {
    pub fn new() -> Self {
        Edit
    }
}

impl Default for Edit {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for Edit {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".to_string(),
            description:
                "Replace an exact string in a file. The file must have been read this session. Fails unless old_string appears exactly once, or replace_all is set."
                    .to_string(),
            schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file to edit" },
                    "old_string": { "type": "string", "description": "Exact text to replace" },
                    "new_string": { "type": "string", "description": "Text to put in its place" },
                    "replace_all": { "type": "boolean", "description": "Replace every occurrence" }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Write
    }

    async fn run(&self, args: Value, ctx: &Ctx) -> Result<ToolOutput> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("path is required"))?;
        let old_string = args
            .get("old_string")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("old_string is required"))?;
        let new_string = args
            .get("new_string")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("new_string is required"))?;
        let replace_all = args
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if old_string.is_empty() {
            return Ok(ToolOutput::error(
                "old_string must not be empty; it would match between every character",
            ));
        }
        if old_string == new_string {
            return Ok(ToolOutput::error(
                "old_string and new_string are identical, nothing to do",
            ));
        }

        let resolved = ctx.resolve(path);
        if !ctx.was_read(&resolved) {
            return Ok(ToolOutput::error(format!(
                "read {path} before editing it"
            )));
        }

        let content = match std::fs::read_to_string(&resolved) {
            Ok(content) => content,
            Err(err) => return Ok(ToolOutput::error(format!("cannot read {path}: {err}"))),
        };

        let count = content.matches(old_string).count();
        if count == 0 {
            return Ok(ToolOutput::error(format!(
                "old_string does not appear in {path}"
            )));
        }
        if count > 1 && !replace_all {
            return Ok(ToolOutput::error(format!(
                "old_string appears {count} times in {path}; make it unique or set replace_all"
            )));
        }

        let updated = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };

        match fsx::write_atomic(&resolved, &updated) {
            Ok(()) => Ok(ToolOutput::ok(format!(
                "replaced {} occurrence{} in {path}",
                if replace_all { count } else { 1 },
                if replace_all && count != 1 { "s" } else { "" }
            ))),
            Err(err) => Ok(ToolOutput::error(format!("cannot write {path}: {err}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str, body: &str) -> (std::path::PathBuf, Ctx) {
        let dir = std::env::temp_dir().join(format!(
            "zarn-edit-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("file.txt");
        std::fs::write(&path, body).unwrap();
        let ctx = Ctx::new(dir);
        (path, ctx)
    }

    fn args(path: &std::path::Path, old: &str, new: &str) -> Value {
        json!({ "path": path.to_string_lossy(), "old_string": old, "new_string": new })
    }

    #[tokio::test]
    async fn it_replaces_a_unique_string() {
        let (path, ctx) = fixture("unique", "alpha beta gamma");
        ctx.mark_read(&path);
        let out = Edit.run(args(&path, "beta", "delta"), &ctx).await.unwrap();
        assert!(!out.is_error);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alpha delta gamma"
        );
    }

    #[tokio::test]
    async fn an_unread_file_is_refused_and_left_alone() {
        let (path, ctx) = fixture("unread", "alpha");
        let out = Edit.run(args(&path, "alpha", "beta"), &ctx).await.unwrap();
        assert!(out.is_error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha");
    }

    #[tokio::test]
    async fn an_ambiguous_match_is_refused_and_leaves_the_file_unchanged() {
        let (path, ctx) = fixture("ambiguous", "x\nx\n");
        ctx.mark_read(&path);
        let out = Edit.run(args(&path, "x", "y"), &ctx).await.unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("appears 2 times"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x\nx\n");
    }

    #[tokio::test]
    async fn replace_all_takes_every_occurrence() {
        let (path, ctx) = fixture("all", "x\nx\n");
        ctx.mark_read(&path);
        let mut a = args(&path, "x", "y");
        a["replace_all"] = json!(true);
        let out = Edit.run(a, &ctx).await.unwrap();
        assert!(!out.is_error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "y\ny\n");
    }

    #[tokio::test]
    async fn an_empty_old_string_cannot_shred_the_file() {
        let (path, ctx) = fixture("empty", "alpha");
        ctx.mark_read(&path);
        let mut a = args(&path, "", "X");
        a["replace_all"] = json!(true);
        let out = Edit.run(a, &ctx).await.unwrap();
        assert!(out.is_error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha");
    }

    #[tokio::test]
    async fn a_string_that_is_absent_is_an_error() {
        let (path, ctx) = fixture("absent", "alpha");
        ctx.mark_read(&path);
        let out = Edit.run(args(&path, "zeta", "beta"), &ctx).await.unwrap();
        assert!(out.is_error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha");
    }
}
