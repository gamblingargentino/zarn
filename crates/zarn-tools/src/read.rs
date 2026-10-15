use crate::budget;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use zarn_core::tools::{Ctx, Risk, Tool, ToolOutput};
use zarn_core::types::ToolSpec;

pub struct Read;

impl Read {
    pub fn new() -> Self {
        Read
    }
}

impl Default for Read {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for Read {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read".to_string(),
            description:
                "Read a file and return its contents with line numbers. Optionally start at a line offset and limit how many lines come back."
                    .to_string(),
            schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the file to read" },
                    "offset": { "type": "integer", "description": "First line to return, 1-based" },
                    "limit": { "type": "integer", "description": "How many lines to return" }
                },
                "required": ["path"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Safe
    }

    async fn run(&self, args: Value, ctx: &Ctx) -> Result<ToolOutput> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("path is required"))?;
        let offset = args
            .get("offset")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1) as usize;
        let limit = args.get("limit").and_then(Value::as_u64).map(|l| l as usize);

        let resolved = ctx.resolve(path);
        let contents = match std::fs::read(&resolved) {
            Ok(bytes) => bytes,
            Err(err) => return Ok(ToolOutput::error(format!("cannot read {path}: {err}"))),
        };

        let text = match String::from_utf8(contents) {
            Ok(text) => text,
            Err(_) => return Ok(ToolOutput::error(format!("{path} is not valid UTF-8"))),
        };

        ctx.mark_read(&resolved);

        let lines: Vec<&str> = text.lines().collect();
        if lines.is_empty() {
            return Ok(ToolOutput::ok(format!("{path} is empty")));
        }
        if offset > lines.len() {
            return Ok(ToolOutput::error(format!(
                "{path} has {} lines, offset {offset} is past the end",
                lines.len()
            )));
        }

        let start = offset - 1;
        let end = limit.map(|l| (start + l).min(lines.len())).unwrap_or(lines.len());

        let mut numbered = String::new();
        for (index, line) in lines[start..end].iter().enumerate() {
            numbered.push_str(&format!("{}\t{line}\n", start + index + 1));
        }

        if end < lines.len() {
            numbered.push_str(&format!(
                "\n[stopped at line {end} of {}; pass offset {} to continue]",
                lines.len(),
                end + 1
            ));
        }

        Ok(ToolOutput::ok(budget::truncate(
            &numbered,
            budget::DEFAULT_TOKEN_BUDGET,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(tag: &str, body: &str) -> (std::path::PathBuf, Ctx) {
        let dir = std::env::temp_dir().join(format!(
            "zarn-read-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("file.txt");
        std::fs::write(&path, body).unwrap();
        (path, Ctx::new(dir))
    }

    #[tokio::test]
    async fn it_numbers_lines_from_one() {
        let (path, ctx) = fixture("basic", "alpha\nbeta\n");
        let out = Read
            .run(json!({ "path": path.to_string_lossy() }), &ctx)
            .await
            .unwrap();
        assert!(out.content.starts_with("1\talpha"));
        assert!(out.content.contains("2\tbeta"));
    }

    #[tokio::test]
    async fn offset_and_limit_select_a_window_keeping_real_line_numbers() {
        let body = (1..=10).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        let (path, ctx) = fixture("window", &body);
        let out = Read
            .run(
                json!({ "path": path.to_string_lossy(), "offset": 4, "limit": 2 }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(out.content.contains("4\tline4"));
        assert!(out.content.contains("5\tline5"));
        assert!(!out.content.contains("6\tline6"));
        assert!(out.content.contains("pass offset 6 to continue"));
    }

    #[tokio::test]
    async fn reading_marks_the_file_so_edit_and_write_accept_it() {
        let (path, ctx) = fixture("mark", "x\n");
        assert!(!ctx.was_read(&path));
        Read.run(json!({ "path": path.to_string_lossy() }), &ctx)
            .await
            .unwrap();
        assert!(ctx.was_read(&path));
    }

    #[tokio::test]
    async fn a_missing_file_is_an_error_and_is_not_marked_read() {
        let (path, ctx) = fixture("missing", "x\n");
        let gone = path.parent().unwrap().join("nope.txt");
        let out = Read
            .run(json!({ "path": gone.to_string_lossy() }), &ctx)
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(!ctx.was_read(&gone));
    }

    #[tokio::test]
    async fn an_oversized_file_is_truncated_with_a_marker() {
        let body = (0..40_000)
            .map(|i| format!("line {i} of a very long file"))
            .collect::<Vec<_>>()
            .join("\n");
        let (path, ctx) = fixture("big", &body);
        let out = Read
            .run(json!({ "path": path.to_string_lossy() }), &ctx)
            .await
            .unwrap();
        assert!(out.content.contains("truncated:"));
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn an_offset_past_the_end_says_so() {
        let (path, ctx) = fixture("past", "one\ntwo\n");
        let out = Read
            .run(json!({ "path": path.to_string_lossy(), "offset": 99 }), &ctx)
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("past the end"));
    }
}
