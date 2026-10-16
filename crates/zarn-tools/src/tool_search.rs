use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use zarn_core::tools::{Ctx, Risk, Tool, ToolOutput};
use zarn_core::types::ToolSpec;

pub const DEFAULT_THRESHOLD: usize = 40;

#[derive(Default)]
pub struct Deferred {
    names: HashSet<String>,
    loaded: RwLock<HashSet<String>>,
}

impl Deferred {
    pub fn new(names: impl IntoIterator<Item = String>) -> Arc<Self> {
        Arc::new(Deferred {
            names: names.into_iter().collect(),
            loaded: RwLock::default(),
        })
    }

    pub fn visible(&self, name: &str) -> bool {
        !self.names.contains(name) || self.loaded.read().is_ok_and(|l| l.contains(name))
    }

    pub fn load(&self, name: &str) {
        if let Ok(mut loaded) = self.loaded.write() {
            loaded.insert(name.to_string());
        }
    }

    pub fn loaded(&self) -> Vec<String> {
        let mut all: Vec<String> = self.loaded.read().map(|l| l.iter().cloned().collect()).unwrap_or_default();
        all.sort();
        all
    }

    pub fn hidden(&self) -> usize {
        self.names.len() - self.loaded.read().map(|l| l.len()).unwrap_or(0)
    }
}

pub struct ToolSearch {
    catalog: Vec<ToolSpec>,
    deferred: Arc<Deferred>,
}

impl ToolSearch {
    pub fn new(catalog: Vec<ToolSpec>, deferred: Arc<Deferred>) -> Self {
        ToolSearch { catalog, deferred }
    }
}

pub fn score(spec: &ToolSpec, words: &[String]) -> usize {
    let name = spec.name.to_lowercase();
    let description = spec.description.to_lowercase();
    words
        .iter()
        .map(|w| {
            if name.contains(w.as_str()) {
                3
            } else if description.contains(w.as_str()) {
                1
            } else {
                0
            }
        })
        .sum()
}

#[async_trait]
impl Tool for ToolSearch {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "tool_search".to_string(),
            description: format!(
                "{} MCP tools are not listed to save context. Search them by keyword (or pass select:name1,name2) and the matches become callable from your next step.",
                self.catalog.len()
            ),
            schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Keywords, or select:<name>[,<name>] to load exact tools" },
                    "limit": { "type": "integer", "description": "Most tools to load (default 5)" }
                },
                "required": ["query"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Safe
    }

    async fn run(&self, args: Value, _ctx: &Ctx) -> Result<ToolOutput> {
        let query = args.get("query").and_then(Value::as_str).ok_or_else(|| anyhow!("query is required"))?;
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(5) as usize;
        let picked: Vec<&ToolSpec> = match query.trim().strip_prefix("select:") {
            Some(list) => {
                let wanted: Vec<&str> = list.split(',').map(str::trim).collect();
                self.catalog.iter().filter(|s| wanted.contains(&s.name.as_str())).collect()
            }
            None => {
                let words: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_string).collect();
                let mut ranked: Vec<(usize, &ToolSpec)> = self
                    .catalog
                    .iter()
                    .map(|s| (score(s, &words), s))
                    .filter(|(n, _)| *n > 0)
                    .collect();
                ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
                ranked.into_iter().take(limit).map(|(_, s)| s).collect()
            }
        };
        if picked.is_empty() {
            return Ok(ToolOutput::ok(format!("no MCP tools match {query}; try other keywords")));
        }
        let lines: Vec<String> = picked
            .iter()
            .map(|s| {
                self.deferred.load(&s.name);
                format!("{}: {}", s.name, s.description)
            })
            .collect();
        Ok(ToolOutput::ok(format!(
            "loaded {} tool{}; call them directly from your next step:\n{}",
            lines.len(),
            if lines.len() == 1 { "" } else { "s" },
            lines.join("\n")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, description: &str) -> ToolSpec {
        ToolSpec { name: name.into(), description: description.into(), schema: json!({ "type": "object" }) }
    }

    #[tokio::test]
    async fn searching_loads_the_best_matches() {
        let catalog = vec![
            spec("gh__create_issue", "open a GitHub issue"),
            spec("gh__list_prs", "list pull requests"),
            spec("db__query", "run a SQL query"),
        ];
        let deferred = Deferred::new(catalog.iter().map(|s| s.name.clone()));
        let tool = ToolSearch::new(catalog, deferred.clone());
        assert!(!deferred.visible("gh__create_issue"));
        assert!(deferred.visible("read"));
        let out = tool.run(json!({ "query": "issue github" }), &Ctx::new(".")).await.unwrap();
        assert!(out.content.starts_with("loaded 1 tool;"), "{}", out.content);
        assert!(deferred.visible("gh__create_issue"));
        assert!(!deferred.visible("db__query"));
        tool.run(json!({ "query": "select:db__query, gh__list_prs" }), &Ctx::new(".")).await.unwrap();
        assert_eq!(deferred.loaded(), vec!["db__query", "gh__create_issue", "gh__list_prs"]);
        assert_eq!(deferred.hidden(), 0);
        let none = tool.run(json!({ "query": "weather" }), &Ctx::new(".")).await.unwrap();
        assert!(none.content.starts_with("no MCP tools match"));
    }
}
