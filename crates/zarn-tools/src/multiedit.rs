use crate::fsx;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use zarn_core::tools::{Ctx, Risk, Tool, ToolOutput};
use zarn_core::types::ToolSpec;

#[derive(Default)]
pub struct MultiEdit;

impl MultiEdit {
    pub fn new() -> Self {
        MultiEdit
    }
}

pub fn apply(content: &str, edits: &[Value]) -> std::result::Result<(String, usize), String> {
    let mut text = content.to_string();
    let mut total = 0;
    for (i, edit) in edits.iter().enumerate() {
        let n = i + 1;
        let old = edit
            .get("old_string")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("edit {n} needs old_string"))?;
        let new = edit
            .get("new_string")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("edit {n} needs new_string"))?;
        let all = edit.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
        if old.is_empty() {
            return Err(format!("edit {n}: old_string must not be empty"));
        }
        if old == new {
            return Err(format!("edit {n}: old_string and new_string are identical"));
        }
        let count = text.matches(old).count();
        if count == 0 {
            return Err(format!("edit {n}: old_string does not appear in the file (after the earlier edits)"));
        }
        if count > 1 && !all {
            return Err(format!("edit {n}: old_string appears {count} times; make it unique or set replace_all"));
        }
        text = if all { text.replace(old, new) } else { text.replacen(old, new, 1) };
        total += if all { count } else { 1 };
    }
    Ok((text, total))
}

#[async_trait]
impl Tool for MultiEdit {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "multiedit".to_string(),
            description: "Apply several exact-string edits to one file in order, atomically: if any edit fails, the file is left untouched. The file must have been read this session.".to_string(),
            schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_string": { "type": "string" },
                                "new_string": { "type": "string" },
                                "replace_all": { "type": "boolean" }
                            },
                            "required": ["old_string", "new_string"]
                        }
                    }
                },
                "required": ["path", "edits"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Write
    }

    async fn run(&self, args: Value, ctx: &Ctx) -> Result<ToolOutput> {
        let path = args.get("path").and_then(Value::as_str).ok_or_else(|| anyhow!("path is required"))?;
        let edits = args.get("edits").and_then(Value::as_array).ok_or_else(|| anyhow!("edits is required"))?;
        if edits.is_empty() {
            return Ok(ToolOutput::error("edits is empty; nothing to do"));
        }
        let resolved = ctx.resolve(path);
        if !ctx.was_read(&resolved) {
            return Ok(ToolOutput::error(format!("read {path} before editing it")));
        }
        if let Some(reason) = crate::freshness::stale(ctx, &resolved) {
            return Ok(ToolOutput::error(reason));
        }
        let content = match std::fs::read_to_string(&resolved) {
            Ok(content) => content,
            Err(err) => return Ok(ToolOutput::error(format!("cannot read {path}: {err}"))),
        };
        let (updated, total) = match apply(&content, edits) {
            Ok(done) => done,
            Err(err) => return Ok(ToolOutput::error(format!("{err}; {path} was not changed"))),
        };
        match fsx::write_atomic(&resolved, &updated) {
            Ok(()) => {
                crate::freshness::record(ctx, &resolved);
                Ok(ToolOutput::ok(format!(
                    "applied {} edits ({total} replacements) to {path}",
                    edits.len()
                )))
            }
            Err(err) => Ok(ToolOutput::error(format!("cannot write {path}: {err}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(body: &str) -> (tempfile::TempDir, Ctx) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), body).unwrap();
        let ctx = Ctx::new(dir.path());
        ctx.mark_read(dir.path().join("f.txt"));
        crate::freshness::record(&ctx, &dir.path().join("f.txt"));
        (dir, ctx)
    }

    #[tokio::test]
    async fn edits_apply_in_order() {
        let (dir, ctx) = setup("alpha beta gamma beta");
        let out = MultiEdit::new()
            .run(
                json!({ "path": "f.txt", "edits": [
                    { "old_string": "alpha", "new_string": "ALPHA" },
                    { "old_string": "beta", "new_string": "B", "replace_all": true },
                    { "old_string": "ALPHA B", "new_string": "start" }
                ]}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(dir.path().join("f.txt")).unwrap(), "start gamma B");
        assert!(out.content.contains("3 edits (4 replacements)"));
    }

    #[tokio::test]
    async fn one_failing_edit_leaves_the_file_untouched() {
        let (dir, ctx) = setup("one two three");
        let out = MultiEdit::new()
            .run(
                json!({ "path": "f.txt", "edits": [
                    { "old_string": "one", "new_string": "1" },
                    { "old_string": "missing", "new_string": "x" }
                ]}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("edit 2"));
        assert_eq!(std::fs::read_to_string(dir.path().join("f.txt")).unwrap(), "one two three");
    }

    #[tokio::test]
    async fn an_unread_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "x").unwrap();
        let out = MultiEdit::new()
            .run(json!({ "path": "f.txt", "edits": [{ "old_string": "x", "new_string": "y" }] }), &Ctx::new(dir.path()))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("read f.txt before editing"));
    }
}
