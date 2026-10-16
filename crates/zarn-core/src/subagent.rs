use crate::agent::{Agent, AgentConfig, AgentEvent};
use crate::provider::Provider;
use crate::tools::{Ctx, Risk, Tool, ToolOutput, ToolRegistry};
use crate::types::{Message, ToolSpec};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    pub tools: Vec<String>,
    pub model: Option<String>,
    pub instructions: String,
}

pub fn parse_definition(source: &str) -> Option<AgentDef> {
    let rest = source.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    let (front, body) = rest.split_at(end);
    let body = body.trim_start_matches("\n---").trim_start_matches('\n');

    let mut def = AgentDef {
        instructions: body.trim().to_string(),
        ..AgentDef::default()
    };

    for line in front.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match key.trim() {
            "name" => def.name = value.to_string(),
            "description" => def.description = value.to_string(),
            "model" => def.model = Some(value.to_string()).filter(|v| !v.is_empty()),
            "tools" => {
                def.tools = value
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .split(',')
                    .map(|t| t.trim().trim_matches('"').to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            }
            _ => {}
        }
    }

    if def.name.is_empty() { None } else { Some(def) }
}

pub fn load_definitions(dir: &Path) -> Vec<AgentDef> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut defs: Vec<AgentDef> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .filter_map(|e| {
            let source = std::fs::read_to_string(e.path()).ok()?;
            match parse_definition(&source) {
                Some(def) => Some(def),
                None => {
                    tracing::warn!(path = %e.path().display(), "skipping agent with no name");
                    None
                }
            }
        })
        .collect();

    defs.sort_by(|a, b| a.name.cmp(&b.name));
    defs
}

pub type ProviderFactory = Arc<dyn Fn(&AgentDef) -> Arc<dyn Provider> + Send + Sync>;
pub type ToolFactory = Arc<dyn Fn(&[String]) -> ToolRegistry + Send + Sync>;

pub struct TaskTool {
    defs: Vec<AgentDef>,
    providers: ProviderFactory,
    tools: ToolFactory,
    ctx: Arc<Ctx>,
    config: AgentConfig,
}

impl TaskTool {
    pub fn new(
        defs: Vec<AgentDef>,
        providers: ProviderFactory,
        tools: ToolFactory,
        ctx: Arc<Ctx>,
        config: AgentConfig,
    ) -> Self {
        TaskTool {
            defs,
            providers,
            tools,
            ctx,
            config,
        }
    }

    pub fn definitions(&self) -> &[AgentDef] {
        &self.defs
    }

    fn describe(&self) -> String {
        if self.defs.is_empty() {
            return "Delegate a task to a nested agent. No agents are defined.".to_string();
        }
        let listed = self
            .defs
            .iter()
            .map(|d| format!("{}: {}", d.name, d.description))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Delegate a self-contained task to a nested agent, which reports back only its final message. Available agents:\n{listed}"
        )
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "task".to_string(),
            description: self.describe(),
            schema: json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "description": "Name of the agent to run",
                        "enum": self.defs.iter().map(|d| d.name.clone()).collect::<Vec<_>>()
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The task for the agent, stated so it needs no other context"
                    }
                },
                "required": ["agent", "prompt"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Execute
    }

    async fn run(&self, args: Value, _ctx: &Ctx) -> Result<ToolOutput> {
        let name = args
            .get("agent")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("task requires an agent name"))?;
        let prompt = args
            .get("prompt")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("task requires a prompt"))?;

        let Some(def) = self.defs.iter().find(|d| d.name == name) else {
            return Ok(ToolOutput::error(format!("no agent named {name}")));
        };

        let provider = (self.providers)(def);
        let registry = Arc::new((self.tools)(&def.tools));
        let config = AgentConfig {
            model: def.model.clone().unwrap_or_else(|| self.config.model.clone()),
            ..self.config.clone()
        };

        let agent = Agent::new(provider, registry, self.ctx.clone(), config);
        let mut messages = Vec::new();
        if !def.instructions.is_empty() {
            messages.push(Message::System(def.instructions.clone()));
        }
        messages.push(Message::user(prompt));

        let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });

        let result = agent
            .run(&mut messages, &tx, &CancellationToken::new())
            .await;
        drop(tx);
        let _ = drain.await;

        if let Err(err) = result {
            return Ok(ToolOutput::error(format!("{name} failed: {err}")));
        }

        let final_text = messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Message::Assistant { .. } => Some(m.text()),
                _ => None,
            })
            .unwrap_or_default();

        if final_text.trim().is_empty() {
            return Ok(ToolOutput::error(format!("{name} returned nothing")));
        }
        Ok(ToolOutput::ok(final_text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_and_body_are_split() {
        let def = parse_definition(
            "---\nname: reviewer\ndescription: reviews a diff\nmodel: sonnet\ntools: Read, Grep\n---\n\nYou review diffs.\n",
        )
        .unwrap();

        assert_eq!(def.name, "reviewer");
        assert_eq!(def.description, "reviews a diff");
        assert_eq!(def.model.as_deref(), Some("sonnet"));
        assert_eq!(def.tools, vec!["Read", "Grep"]);
        assert_eq!(def.instructions, "You review diffs.");
    }

    #[test]
    fn a_bracketed_tools_list_parses_the_same_way() {
        let def =
            parse_definition("---\nname: a\ntools: [\"Read\", \"Write\"]\n---\nbody").unwrap();
        assert_eq!(def.tools, vec!["Read", "Write"]);
    }

    #[test]
    fn a_file_with_no_frontmatter_is_not_an_agent() {
        assert!(parse_definition("just a markdown file").is_none());
    }

    #[test]
    fn a_nameless_agent_is_rejected() {
        assert!(parse_definition("---\ndescription: no name\n---\nbody").is_none());
    }

    #[test]
    fn a_missing_model_stays_none_rather_than_empty() {
        let def = parse_definition("---\nname: a\nmodel:\n---\nbody").unwrap();
        assert!(def.model.is_none());
    }
}
