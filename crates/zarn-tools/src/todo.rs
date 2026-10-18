use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use zarn_core::session::Todo;
use zarn_core::tools::{Ctx, Risk, Tool, ToolOutput};
use zarn_core::types::ToolSpec;

pub const STATUSES: [&str; 3] = ["pending", "in_progress", "done"];

pub type TodoList = Arc<Mutex<Vec<Todo>>>;

pub struct TodoTool {
    list: TodoList,
}

impl TodoTool {
    pub fn new(list: TodoList) -> Self {
        TodoTool { list }
    }
}

pub fn mark(status: &str) -> &'static str {
    match status {
        "done" => "[x]",
        "in_progress" => "[~]",
        _ => "[ ]",
    }
}

pub fn render(todos: &[Todo]) -> String {
    if todos.is_empty() {
        return "the todo list is empty".into();
    }
    todos
        .iter()
        .map(|t| format!("{} {}", mark(&t.status), t.content))
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait]
impl Tool for TodoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "todo".to_string(),
            description: "Write the task checklist for this session. Send the whole list every time: each item has content and a status of pending, in_progress or done. Keep exactly one item in_progress while you work, and mark items done as soon as they are finished.".to_string(),
            schema: json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string" },
                                "status": { "type": "string", "enum": STATUSES }
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Safe
    }

    async fn run(&self, args: Value, _ctx: &Ctx) -> Result<ToolOutput> {
        let items = args
            .get("todos")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("todo requires a todos array"))?;
        let mut todos = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let content = item
                .get("content")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .ok_or_else(|| anyhow!("item {} has no content", i + 1))?;
            let status = item.get("status").and_then(Value::as_str).unwrap_or("pending");
            if !STATUSES.contains(&status) {
                return Ok(ToolOutput::error(format!(
                    "item {} has status '{status}'; use pending, in_progress or done",
                    i + 1
                )));
            }
            todos.push(Todo {
                content: content.to_string(),
                status: status.to_string(),
            });
        }
        let text = render(&todos);
        if let Ok(mut list) = self.list.lock() {
            *list = todos;
        }
        Ok(ToolOutput::ok(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_tool_replaces_the_shared_list_and_validates_statuses() {
        let list: TodoList = Arc::default();
        let tool = TodoTool::new(list.clone());
        let out = tool
            .run(
                json!({ "todos": [
                    { "content": "read the code", "status": "done" },
                    { "content": "write the fix", "status": "in_progress" },
                    { "content": "run the tests", "status": "pending" }
                ]}),
                &Ctx::new("."),
            )
            .await
            .unwrap();
        assert_eq!(out.content, "[x] read the code\n[~] write the fix\n[ ] run the tests");
        assert_eq!(list.lock().unwrap().len(), 3);
        let bad = tool
            .run(json!({ "todos": [{ "content": "x", "status": "maybe" }] }), &Ctx::new("."))
            .await
            .unwrap();
        assert!(bad.is_error);
        assert_eq!(list.lock().unwrap().len(), 3, "a bad update leaves the list alone");
        tool.run(json!({ "todos": [] }), &Ctx::new(".")).await.unwrap();
        assert!(list.lock().unwrap().is_empty());
    }
}
