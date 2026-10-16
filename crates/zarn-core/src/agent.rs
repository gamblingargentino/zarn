use crate::provider::{Event, Provider, ProviderError};
use crate::tools::{Ctx, Risk, ToolOutput, ToolRegistry};
use crate::types::{Content, Message, Request, StopReason, ToolCall};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub enum AgentEvent {
    Provider(Event),
    ToolStarted {
        id: String,
        name: String,
        args: Value,
    },
    ToolFinished {
        id: String,
        name: String,
        output: ToolOutput,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateDecision {
    Allow,
    Deny(String),
}

#[async_trait]
pub trait ToolGate: Send + Sync {
    async fn check(&self, name: &str, risk: Risk, args: &Value) -> GateDecision;
}

#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub max_iterations: usize,
    pub parallel: usize,
}

pub const DEFAULT_PARALLEL: usize = 4;
pub const PARALLEL_TOOLS: [&str; 1] = ["task"];

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            model: String::new(),
            max_tokens: 4096,
            temperature: 1.0,
            max_iterations: 32,
            parallel: DEFAULT_PARALLEL,
        }
    }
}

pub type ToolFilter = Arc<dyn Fn(&crate::types::ToolSpec) -> bool + Send + Sync>;

fn batches(calls: Vec<ToolCall>) -> Vec<Vec<ToolCall>> {
    let mut out: Vec<Vec<ToolCall>> = Vec::new();
    for call in calls {
        let parallel = PARALLEL_TOOLS.contains(&call.name.as_str());
        match out.last_mut() {
            Some(batch)
                if parallel && batch.first().is_some_and(|c| PARALLEL_TOOLS.contains(&c.name.as_str())) =>
            {
                batch.push(call)
            }
            _ => out.push(vec![call]),
        }
    }
    out
}

pub struct Agent {
    provider: Arc<dyn Provider>,
    tools: Arc<ToolRegistry>,
    ctx: Arc<Ctx>,
    config: AgentConfig,
    gate: Option<Arc<dyn ToolGate>>,
    filter: Option<ToolFilter>,
}

struct Collected {
    text: String,
    calls: Vec<ToolCall>,
    stop: StopReason,
}

impl Agent {
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: Arc<ToolRegistry>,
        ctx: Arc<Ctx>,
        config: AgentConfig,
    ) -> Self {
        Agent {
            provider,
            tools,
            ctx,
            config,
            gate: None,
            filter: None,
        }
    }

    pub fn with_gate(mut self, gate: Arc<dyn ToolGate>) -> Self {
        self.gate = Some(gate);
        self
    }

    pub fn with_filter(mut self, filter: ToolFilter) -> Self {
        self.filter = Some(filter);
        self
    }

    pub fn tools(&self) -> &Arc<ToolRegistry> {
        &self.tools
    }

    pub fn ctx(&self) -> &Arc<Ctx> {
        &self.ctx
    }

    pub fn gate(&self) -> Option<&Arc<dyn ToolGate>> {
        self.gate.as_ref()
    }

    fn specs(&self) -> Vec<crate::types::ToolSpec> {
        let specs = self.tools.specs();
        match &self.filter {
            Some(keep) => specs.into_iter().filter(|s| keep(s)).collect(),
            None => specs,
        }
    }

    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    pub async fn run(
        &self,
        messages: &mut Vec<Message>,
        sink: &Sender<AgentEvent>,
        cancel: &CancellationToken,
    ) -> Result<StopReason, ProviderError> {
        for _ in 0..self.config.max_iterations {
            let req = Request {
                messages: messages.clone(),
                tools: self.specs(),
                model: self.config.model.clone(),
                max_tokens: self.config.max_tokens,
                temperature: self.config.temperature,
            };

            let collected = self.consume(req, sink, cancel).await?;

            messages.push(Message::Assistant {
                content: if collected.text.is_empty() {
                    Vec::new()
                } else {
                    vec![Content::Text(collected.text)]
                },
                tool_calls: collected.calls.clone(),
            });

            if collected.stop == StopReason::Cancelled {
                return Ok(StopReason::Cancelled);
            }

            if collected.calls.is_empty() {
                return Ok(collected.stop);
            }

            for batch in batches(collected.calls) {
                let outputs = if batch.len() == 1 {
                    vec![self.invoke(&batch[0], sink, cancel).await]
                } else {
                    self.invoke_parallel(&batch, sink, cancel).await
                };
                for (call, output) in batch.into_iter().zip(outputs) {
                    messages.push(Message::ToolResult {
                        id: call.id,
                        content: output.content,
                        is_error: output.is_error,
                    });
                }
            }

            if cancel.is_cancelled() {
                return Ok(StopReason::Cancelled);
            }
        }

        tracing::warn!(
            max_iterations = self.config.max_iterations,
            "agent loop hit its iteration guard"
        );
        Ok(StopReason::MaxTokens)
    }

    async fn consume(
        &self,
        req: Request,
        sink: &Sender<AgentEvent>,
        cancel: &CancellationToken,
    ) -> Result<Collected, ProviderError> {
        let mut stream = self.provider.stream(req).await?;
        let mut text = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut pending: Option<(String, String, String)> = None;
        let mut stop = StopReason::EndTurn;

        loop {
            let event = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let _ = sink.send(AgentEvent::Provider(Event::Done(StopReason::Cancelled))).await;
                    stop = StopReason::Cancelled;
                    break;
                }
                next = stream.next() => match next {
                    Some(e) => e,
                    None => break,
                },
            };

            let _ = sink.send(AgentEvent::Provider(event.clone())).await;

            match event {
                Event::TextDelta(s) => text.push_str(&s),
                Event::ToolCallStart { id, name } => {
                    pending = Some((id, name, String::new()));
                }
                Event::ToolCallDelta(s) => {
                    if let Some((_, _, buf)) = pending.as_mut() {
                        buf.push_str(&s);
                    }
                }
                Event::ToolCallEnd => {
                    if let Some((id, name, buf)) = pending.take() {
                        calls.push(ToolCall {
                            id,
                            name,
                            args: parse_args(&buf),
                        });
                    }
                }
                Event::Usage { .. } => {}
                Event::Done(reason) => stop = reason,
                Event::Error(err) => return Err(err),
            }
        }

        if let Some((id, name, buf)) = pending.take() {
            calls.push(ToolCall {
                id,
                name,
                args: parse_args(&buf),
            });
        }

        if !calls.is_empty() && stop != StopReason::Cancelled {
            stop = StopReason::ToolUse;
        }

        Ok(Collected { text, calls, stop })
    }

    async fn started(&self, call: &ToolCall, sink: &Sender<AgentEvent>) {
        let _ = sink
            .send(AgentEvent::ToolStarted {
                id: call.id.clone(),
                name: call.name.clone(),
                args: call.args.clone(),
            })
            .await;
    }

    async fn finished(&self, call: &ToolCall, output: &ToolOutput, sink: &Sender<AgentEvent>) {
        let _ = sink
            .send(AgentEvent::ToolFinished {
                id: call.id.clone(),
                name: call.name.clone(),
                output: output.clone(),
            })
            .await;
    }

    async fn admit(&self, call: &ToolCall) -> Result<(), ToolOutput> {
        let Some(tool) = self.tools.get(&call.name) else {
            return Err(ToolOutput::error(format!("unknown tool: {}", call.name)));
        };
        match self.permit(&call.name, tool.risk(), &call.args).await {
            GateDecision::Deny(reason) => Err(ToolOutput::error(reason)),
            GateDecision::Allow => Ok(()),
        }
    }

    async fn execute(&self, call: &ToolCall, cancel: &CancellationToken) -> ToolOutput {
        let Some(tool) = self.tools.get(&call.name) else {
            return ToolOutput::error(format!("unknown tool: {}", call.name));
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => ToolOutput::error("cancelled before the tool finished"),
            result = tool.run(call.args.clone(), &self.ctx) => match result {
                Ok(out) => out,
                Err(err) => ToolOutput::error(format!("{err:#}")),
            },
        }
    }

    async fn invoke(&self, call: &ToolCall, sink: &Sender<AgentEvent>, cancel: &CancellationToken) -> ToolOutput {
        self.started(call, sink).await;
        let output = match self.admit(call).await {
            Err(denied) => denied,
            Ok(()) if cancel.is_cancelled() => ToolOutput::error("cancelled before the tool ran"),
            Ok(()) => self.execute(call, cancel).await,
        };
        self.finished(call, &output, sink).await;
        output
    }

    async fn invoke_parallel(
        &self,
        calls: &[ToolCall],
        sink: &Sender<AgentEvent>,
        cancel: &CancellationToken,
    ) -> Vec<ToolOutput> {
        let mut admitted = Vec::with_capacity(calls.len());
        for call in calls {
            self.started(call, sink).await;
            admitted.push(self.admit(call).await);
        }
        let slots = tokio::sync::Semaphore::new(self.config.parallel.max(1));
        let slots = &slots;
        let runs: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutput> + Send + '_>>> = calls
            .iter()
            .zip(admitted)
            .map(|(call, admitted)| {
                Box::pin(async move {
                    let output = match admitted {
                        Err(denied) => denied,
                        Ok(()) => {
                            let _slot = slots.acquire().await;
                            self.execute(call, cancel).await
                        }
                    };
                    self.finished(call, &output, sink).await;
                    output
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutput> + Send + '_>>
            })
            .collect();
        futures::future::join_all(runs).await
    }
}

impl Agent {
    async fn permit(&self, name: &str, risk: Risk, args: &Value) -> GateDecision {
        match &self.gate {
            None => GateDecision::Allow,
            Some(gate) => gate.check(name, risk, args).await,
        }
    }
}

fn parse_args(buf: &str) -> Value {
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return Value::Object(serde_json::Map::new());
    }
    serde_json::from_str(trimmed).unwrap_or_else(|_| Value::String(trimmed.to_string()))
}
