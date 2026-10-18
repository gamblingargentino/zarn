use crate::fsx;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use zarn_core::tools::{Ctx, Risk, Tool, ToolOutput};
use zarn_core::types::ToolSpec;

#[derive(Default)]
pub struct NotebookEdit;

impl NotebookEdit {
    pub fn new() -> Self {
        NotebookEdit
    }
}

fn source_lines(text: &str) -> Value {
    let mut lines: Vec<Value> = text.split_inclusive('\n').map(|l| Value::String(l.to_string())).collect();
    if lines.is_empty() {
        lines.push(Value::String(String::new()));
    }
    Value::Array(lines)
}

fn new_cell(kind: &str, source: &str) -> Value {
    let mut cell = Map::new();
    cell.insert("cell_type".into(), json!(kind));
    cell.insert("metadata".into(), json!({}));
    cell.insert("source".into(), source_lines(source));
    if kind == "code" {
        cell.insert("execution_count".into(), Value::Null);
        cell.insert("outputs".into(), json!([]));
    }
    Value::Object(cell)
}

fn find(cells: &[Value], args: &Value) -> std::result::Result<Option<usize>, String> {
    if let Some(id) = args.get("cell_id").and_then(Value::as_str) {
        return cells
            .iter()
            .position(|c| c.get("id").and_then(Value::as_str) == Some(id))
            .map(Some)
            .ok_or_else(|| format!("no cell with id {id}"));
    }
    match args.get("cell_index").and_then(Value::as_u64) {
        Some(i) => Ok(Some(i as usize)),
        None => Ok(None),
    }
}

pub fn edit(notebook: &mut Value, args: &Value) -> std::result::Result<String, String> {
    let cells = notebook
        .get_mut("cells")
        .and_then(Value::as_array_mut)
        .ok_or("this file has no cells array; is it a Jupyter notebook?")?;
    let mode = args.get("edit_mode").and_then(Value::as_str).unwrap_or("replace");
    let source = args.get("new_source").and_then(Value::as_str).unwrap_or("");
    let at = find(cells, args)?;
    match mode {
        "replace" => {
            let i = at.ok_or("replace needs cell_index or cell_id")?;
            let count = cells.len();
            let cell = cells.get_mut(i).ok_or(format!("cell {i} does not exist; the notebook has {count} cells"))?;
            cell["source"] = source_lines(source);
            if let Some(kind) = args.get("cell_type").and_then(Value::as_str) {
                if kind != "code" && kind != "markdown" && kind != "raw" {
                    return Err(format!("cell_type must be code, markdown or raw, not {kind}"));
                }
                if cell["cell_type"] != kind {
                    *cell = new_cell(kind, source);
                }
            }
            if cell["cell_type"] == "code" {
                cell["outputs"] = json!([]);
                cell["execution_count"] = Value::Null;
            }
            Ok(format!("replaced cell {i}"))
        }
        "insert" => {
            let kind = args.get("cell_type").and_then(Value::as_str).unwrap_or("code");
            if kind != "code" && kind != "markdown" && kind != "raw" {
                return Err(format!("cell_type must be code, markdown or raw, not {kind}"));
            }
            let i = at.map(|i| i + 1).unwrap_or(0).min(cells.len());
            cells.insert(i, new_cell(kind, source));
            Ok(format!("inserted a {kind} cell at {i}"))
        }
        "delete" => {
            let i = at.ok_or("delete needs cell_index or cell_id")?;
            if i >= cells.len() {
                return Err(format!("cell {i} does not exist; the notebook has {} cells", cells.len()));
            }
            cells.remove(i);
            Ok(format!("deleted cell {i}"))
        }
        other => Err(format!("edit_mode must be replace, insert or delete, not {other}")),
    }
}

#[async_trait]
impl Tool for NotebookEdit {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "notebook_edit".to_string(),
            description: "Edit a Jupyter notebook (.ipynb): replace a cell's source, insert a new cell after a given one (or at the top), or delete a cell. Cells are addressed by cell_id or zero-based cell_index. Replacing a code cell clears its outputs. The notebook must have been read this session.".to_string(),
            schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "cell_index": { "type": "integer" },
                    "cell_id": { "type": "string" },
                    "new_source": { "type": "string" },
                    "cell_type": { "type": "string", "enum": ["code", "markdown", "raw"] },
                    "edit_mode": { "type": "string", "enum": ["replace", "insert", "delete"] }
                },
                "required": ["path"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Write
    }

    async fn run(&self, args: Value, ctx: &Ctx) -> Result<ToolOutput> {
        let path = args.get("path").and_then(Value::as_str).ok_or_else(|| anyhow!("path is required"))?;
        if !path.ends_with(".ipynb") {
            return Ok(ToolOutput::error(format!("{path} is not a .ipynb notebook; use edit for other files")));
        }
        let resolved = ctx.resolve(path);
        if !ctx.was_read(&resolved) {
            return Ok(ToolOutput::error(format!("read {path} before editing it")));
        }
        if let Some(reason) = crate::freshness::stale(ctx, &resolved) {
            return Ok(ToolOutput::error(reason));
        }
        let text = match std::fs::read_to_string(&resolved) {
            Ok(text) => text,
            Err(err) => return Ok(ToolOutput::error(format!("cannot read {path}: {err}"))),
        };
        let mut notebook: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(err) => return Ok(ToolOutput::error(format!("{path} is not valid JSON: {err}"))),
        };
        let summary = match edit(&mut notebook, &args) {
            Ok(s) => s,
            Err(err) => return Ok(ToolOutput::error(format!("{err}; {path} was not changed"))),
        };
        let mut out = Vec::new();
        let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
        let mut ser = serde_json::Serializer::with_formatter(&mut out, formatter);
        serde::Serialize::serialize(&notebook, &mut ser)?;
        let mut body = String::from_utf8(out)?;
        body.push('\n');
        match fsx::write_atomic(&resolved, &body) {
            Ok(()) => {
                crate::freshness::record(ctx, &resolved);
                Ok(ToolOutput::ok(format!("{summary} in {path}")))
            }
            Err(err) => Ok(ToolOutput::error(format!("cannot write {path}: {err}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NB: &str = r##"{"cells":[{"cell_type":"markdown","id":"intro","metadata":{},"source":["# Title\n"]},{"cell_type":"code","id":"c1","execution_count":3,"metadata":{},"outputs":[{"output_type":"stream","text":["old\n"]}],"source":["print(1)\n"]}],"metadata":{},"nbformat":4,"nbformat_minor":5}"##;

    fn setup() -> (tempfile::TempDir, Ctx) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("n.ipynb"), NB).unwrap();
        let ctx = Ctx::new(dir.path());
        ctx.mark_read(dir.path().join("n.ipynb"));
        crate::freshness::record(&ctx, &dir.path().join("n.ipynb"));
        (dir, ctx)
    }

    fn cells(dir: &tempfile::TempDir) -> Vec<Value> {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join("n.ipynb")).unwrap()).unwrap();
        v["cells"].as_array().unwrap().clone()
    }

    #[tokio::test]
    async fn replacing_a_code_cell_clears_its_outputs() {
        let (dir, ctx) = setup();
        let out = NotebookEdit::new()
            .run(json!({ "path": "n.ipynb", "cell_id": "c1", "new_source": "x = 2\nprint(x)" }), &ctx)
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        let c = cells(&dir);
        assert_eq!(c[1]["source"], json!(["x = 2\n", "print(x)"]));
        assert_eq!(c[1]["outputs"], json!([]));
        assert_eq!(c[1]["execution_count"], Value::Null);
    }

    #[tokio::test]
    async fn cells_can_be_inserted_and_deleted() {
        let (dir, ctx) = setup();
        let tool = NotebookEdit::new();
        tool.run(json!({ "path": "n.ipynb", "cell_index": 0, "edit_mode": "insert", "cell_type": "markdown", "new_source": "notes" }), &ctx)
            .await
            .unwrap();
        assert_eq!(cells(&dir).len(), 3);
        assert_eq!(cells(&dir)[1]["source"], json!(["notes"]));
        let out = tool.run(json!({ "path": "n.ipynb", "cell_id": "intro", "edit_mode": "delete" }), &ctx).await.unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(cells(&dir).len(), 2);
        assert_eq!(cells(&dir)[0]["source"], json!(["notes"]));
    }

    #[tokio::test]
    async fn bad_targets_leave_the_notebook_alone() {
        let (dir, ctx) = setup();
        let out = NotebookEdit::new()
            .run(json!({ "path": "n.ipynb", "cell_index": 9, "new_source": "x" }), &ctx)
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("cell 9 does not exist"));
        assert_eq!(cells(&dir).len(), 2);
        let other = NotebookEdit::new().run(json!({ "path": "a.py" }), &ctx).await.unwrap();
        assert!(other.content.contains("not a .ipynb"));
    }
}
