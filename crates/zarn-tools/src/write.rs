use async_trait::async_trait;
use serde_json::{Value, json};

use zarn_core::tools::{Ctx, Risk, Tool, ToolOutput};
use zarn_core::types::ToolSpec;
use anyhow::Result;

pub struct Write;

impl Write {
    pub fn new() -> Self {
        Write
    }
}

#[async_trait]
impl Tool for Write {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write".to_string(),
            description: "Write content to a file. Creates parent directories. Errors if file exists and has not been read this session.".to_string(),
            schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    }
                },
                "required": ["path", "content"]
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
            .ok_or_else(|| anyhow::anyhow!("path is required"))?;

        let content = args
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("'content' parameter is required"))?;

        let resolved = ctx.resolve(path);

        if resolved.exists() && !ctx.was_read(&resolved) {
            return Ok(ToolOutput::error(
                "File must be read before writing in this session"
            ));
        }

        if resolved.exists()
            && let Some(reason) = crate::freshness::stale(ctx, &resolved)
        {
            return Ok(ToolOutput::error(reason));
        }

        match crate::fsx::write_atomic(&resolved, content) {
            Ok(()) => {
                ctx.mark_read(&resolved);
                crate::freshness::record(ctx, &resolved);
                Ok(ToolOutput::ok(format!(
                    "wrote {} bytes to {path}",
                    content.len()
                )))
            }
            Err(err) => Ok(ToolOutput::error(format!("cannot write {path}: {err}"))),
        }
    }
}

impl Default for Write {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(tag: &str) -> Ctx {
        let dir = std::env::temp_dir().join(format!(
            "zarn-write-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Ctx::new(dir)
    }

    #[tokio::test]
    async fn it_creates_a_new_file_and_its_parent_directories() {
        let ctx = fixture("new");
        let path = ctx.cwd().join("a/b/c.txt");
        let out = Write
            .run(
                json!({ "path": path.to_string_lossy(), "content": "hello" }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!out.is_error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        std::fs::remove_dir_all(ctx.cwd()).ok();
    }

    #[tokio::test]
    async fn an_existing_unread_file_is_refused_and_left_alone() {
        let ctx = fixture("unread");
        let path = ctx.cwd().join("existing.txt");
        std::fs::write(&path, "original").unwrap();

        let out = Write
            .run(
                json!({ "path": path.to_string_lossy(), "content": "clobbered" }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(out.is_error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
        std::fs::remove_dir_all(ctx.cwd()).ok();
    }

    #[tokio::test]
    async fn an_existing_file_that_was_read_can_be_replaced() {
        let ctx = fixture("read");
        let path = ctx.cwd().join("existing.txt");
        std::fs::write(&path, "original").unwrap();
        ctx.mark_read(&path);

        let out = Write
            .run(
                json!({ "path": path.to_string_lossy(), "content": "replaced" }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!out.is_error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "replaced");
        std::fs::remove_dir_all(ctx.cwd()).ok();
    }

    #[tokio::test]
    async fn a_missing_content_argument_is_an_error() {
        let ctx = fixture("missing");
        let path = ctx.cwd().join("x.txt");
        assert!(
            Write
                .run(json!({ "path": path.to_string_lossy() }), &ctx)
                .await
                .is_err()
        );
        std::fs::remove_dir_all(ctx.cwd()).ok();
    }
}

