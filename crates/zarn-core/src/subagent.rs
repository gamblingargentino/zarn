use crate::agent::{Agent, AgentConfig, AgentEvent, ToolGate};
use crate::provider::Event;
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

#[derive(Clone, Debug, PartialEq)]
pub enum TaskState {
    Started { prompt: String, background: bool },
    Tool(String),
    Tokens(u64),
    Finished { ok: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub struct TaskUpdate {
    pub id: String,
    pub agent: String,
    pub state: TaskState,
}

pub type TaskMonitor = Arc<dyn Fn(TaskUpdate) + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStatus {
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn label(&self) -> &'static str {
        match self {
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Job {
    pub id: String,
    pub agent: String,
    pub prompt: String,
    pub status: JobStatus,
    pub result: String,
    pub started: std::time::Instant,
    pub elapsed: Option<std::time::Duration>,
    pub delivered: bool,
    cancel: CancellationToken,
}

#[derive(Default)]
pub struct Background {
    jobs: std::sync::Mutex<Vec<Job>>,
    next: std::sync::atomic::AtomicUsize,
}

impl Background {
    pub fn new() -> Arc<Self> {
        Arc::new(Background::default())
    }

    fn start(&self, agent: &str, prompt: &str) -> (String, CancellationToken) {
        let n = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let id = format!("bg-{n}");
        let cancel = CancellationToken::new();
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.push(Job {
                id: id.clone(),
                agent: agent.to_string(),
                prompt: prompt.to_string(),
                status: JobStatus::Running,
                result: String::new(),
                started: std::time::Instant::now(),
                elapsed: None,
                delivered: false,
                cancel: cancel.clone(),
            });
        }
        (id, cancel)
    }

    fn finish(&self, id: &str, status: JobStatus, result: String) {
        if let Ok(mut jobs) = self.jobs.lock()
            && let Some(job) = jobs.iter_mut().find(|j| j.id == id)
            && job.status == JobStatus::Running
        {
            job.status = status;
            job.result = result;
            job.elapsed = Some(job.started.elapsed());
        }
    }

    pub fn list(&self) -> Vec<Job> {
        self.jobs.lock().map(|j| j.clone()).unwrap_or_default()
    }

    pub fn running(&self) -> usize {
        self.list().iter().filter(|j| j.status == JobStatus::Running).count()
    }

    pub fn cancel(&self, id: &str) -> Result<String, String> {
        let mut jobs = self.jobs.lock().map_err(|_| "lock poisoned".to_string())?;
        let job = jobs.iter_mut().find(|j| j.id == id).ok_or_else(|| format!("no background agent {id}"))?;
        if job.status != JobStatus::Running {
            return Err(format!("{id} already {}", job.status.label()));
        }
        job.cancel.cancel();
        job.status = JobStatus::Cancelled;
        job.result = "cancelled by the user".into();
        job.elapsed = Some(job.started.elapsed());
        Ok(format!("cancelled {id} ({})", job.agent))
    }

    pub fn take_finished(&self) -> Vec<Job> {
        let Ok(mut jobs) = self.jobs.lock() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for job in jobs.iter_mut() {
            if job.status != JobStatus::Running && !job.delivered {
                job.delivered = true;
                out.push(job.clone());
            }
        }
        out
    }

    pub fn cancel_all(&self) {
        for job in self.list() {
            if job.status == JobStatus::Running {
                let _ = self.cancel(&job.id);
            }
        }
    }
}

pub fn deliver(jobs: &[Job]) -> Option<String> {
    if jobs.is_empty() {
        return None;
    }
    let parts: Vec<String> = jobs
        .iter()
        .map(|j| format!("<background_result id=\"{}\" agent=\"{}\" status=\"{}\">\n{}\n</background_result>", j.id, j.agent, j.status.label(), j.result.trim()))
        .collect();
    Some(format!("Background agents finished since your last turn:\n{}", parts.join("\n")))
}

static RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[derive(Clone)]
struct Runner {
    providers: ProviderFactory,
    tools: ToolFactory,
    ctx: Arc<Ctx>,
    config: AgentConfig,
    gate: Option<Arc<dyn ToolGate>>,
    monitor: Option<TaskMonitor>,
}

impl Runner {
    fn report(&self, id: &str, agent: &str, state: TaskState) {
        if let Some(monitor) = &self.monitor {
            monitor(TaskUpdate {
                id: id.to_string(),
                agent: agent.to_string(),
                state,
            });
        }
    }

    async fn run(&self, def: AgentDef, prompt: String, id: String, background: bool, cancel: CancellationToken) -> Result<String, String> {
        let name = def.name.clone();
        self.report(&id, &name, TaskState::Started { prompt: prompt.clone(), background });
        let provider = (self.providers)(&def);
        let registry = Arc::new((self.tools)(&def.tools));
        let config = AgentConfig {
            model: def.model.clone().unwrap_or_else(|| self.config.model.clone()),
            ..self.config.clone()
        };
        let mut agent = Agent::new(provider, registry, self.ctx.clone(), config);
        if let Some(gate) = &self.gate {
            agent = agent.with_gate(gate.clone());
        }
        let mut messages = Vec::new();
        if !def.instructions.is_empty() {
            messages.push(Message::System(def.instructions.clone()));
        }
        messages.push(Message::user(prompt));

        let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);
        let watch = async {
            let mut tokens: u64 = 0;
            let mut chars: u64 = 0;
            while let Some(event) = rx.recv().await {
                match event {
                    AgentEvent::ToolStarted { name: tool, .. } => self.report(&id, &name, TaskState::Tool(tool)),
                    AgentEvent::Provider(Event::Usage { output, .. }) => {
                        tokens += u64::from(output);
                        chars = 0;
                        self.report(&id, &name, TaskState::Tokens(tokens));
                    }
                    AgentEvent::Provider(Event::TextDelta(text)) => {
                        chars += text.len() as u64;
                        self.report(&id, &name, TaskState::Tokens(tokens + chars.div_ceil(4)));
                    }
                    _ => {}
                }
            }
        };
        let work = async {
            let result = agent.run(&mut messages, &tx, &cancel).await;
            drop(tx);
            result
        };
        let (result, ()) = tokio::join!(work, watch);

        let outcome = match result {
            Err(err) => Err(format!("{name} failed: {err}")),
            Ok(crate::types::StopReason::Cancelled) => Err(format!("{name} was cancelled")),
            Ok(_) => {
                let text = messages
                    .iter()
                    .rev()
                    .find_map(|m| match m {
                        Message::Assistant { .. } => Some(m.text()),
                        _ => None,
                    })
                    .unwrap_or_default();
                if text.trim().is_empty() {
                    Err(format!("{name} returned nothing"))
                } else {
                    Ok(text)
                }
            }
        };
        self.report(&id, &name, TaskState::Finished { ok: outcome.is_ok() });
        outcome
    }
}

pub struct TaskTool {
    defs: Vec<AgentDef>,
    runner: Runner,
    background: Option<Arc<Background>>,
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
            runner: Runner {
                providers,
                tools,
                ctx,
                config,
                gate: None,
                monitor: None,
            },
            background: None,
        }
    }

    pub fn with_gate(mut self, gate: Arc<dyn ToolGate>) -> Self {
        self.runner.gate = Some(gate);
        self
    }

    pub fn with_monitor(mut self, monitor: TaskMonitor) -> Self {
        self.runner.monitor = Some(monitor);
        self
    }

    pub fn with_background(mut self, background: Arc<Background>) -> Self {
        self.background = Some(background);
        self
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
            "Delegate a self-contained task to a nested agent, which reports back only its final message. Several task calls in one reply run at the same time. Set background to true to start an agent and carry on; its result arrives with the next user turn. Available agents:\n{listed}"
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
                    },
                    "background": {
                        "type": "boolean",
                        "description": "Return at once with an id and deliver the result on the next turn"
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
        let background = args.get("background").and_then(Value::as_bool).unwrap_or(false);

        let Some(def) = self.defs.iter().find(|d| d.name == name).cloned() else {
            return Ok(ToolOutput::error(format!("no agent named {name}")));
        };

        if background && let Some(registry) = &self.background {
            let (id, cancel) = registry.start(name, prompt);
            let runner = self.runner.clone();
            let registry = registry.clone();
            let job = id.clone();
            let prompt = prompt.to_string();
            tokio::spawn(async move {
                let outcome = runner.run(def, prompt, job.clone(), true, cancel.clone()).await;
                let (status, text) = match outcome {
                    Ok(text) => (JobStatus::Done, text),
                    Err(_) if cancel.is_cancelled() => (JobStatus::Cancelled, "cancelled by the user".to_string()),
                    Err(err) => (JobStatus::Failed, err),
                };
                registry.finish(&job, status, text);
            });
            return Ok(ToolOutput::ok(format!(
                "started background agent {id} ({name}); its result arrives with the next user turn, and /tasks shows progress"
            )));
        }

        let n = RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let note = if background { "\n(background agents are not available here, so this ran in the foreground)" } else { "" };
        Ok(match self.runner.run(def, prompt.to_string(), format!("task-{n}"), false, CancellationToken::new()).await {
            Ok(text) => ToolOutput::ok(format!("{text}{note}")),
            Err(err) => ToolOutput::error(err),
        })
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
