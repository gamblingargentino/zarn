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
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            model: String::new(),
            max_tokens: 4096,
            temperature: 1.0,
            max_iterations: 32,
        }
    }
}

pub struct Agent {
    provider: Arc<dyn Provider>,
    tools: Arc<ToolRegistry>,
    ctx: Arc<Ctx>,
    config: AgentConfig,
    gate: Option<Arc<dyn ToolGate>>,
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
        }
    }

    pub fn with_gate(mut self, gate: Arc<dyn ToolGate>) -> Self {
        self.gate = Some(gate);
        self
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
                tools: self.tools.specs(),
                model: self.config.model.clone(),
                max_tokens: self.config.max_tokens,
                temperature: self.config.temperature,
            };

            let collected = match self.consume(req, sink, cancel).await? {
                Some(c) => c,
                None => {
                    return Ok(StopReason::Cancelled);
                }
            };

            messages.push(Message::Assistant {
                content: if collected.text.is_empty() {
                    Vec::new()
                } else {
                    vec![Content::Text(collected.text)]
                },
                tool_calls: collected.calls.clone(),
            });

            if collected.calls.is_empty() {
                return Ok(collected.stop);
            }

            for call in collected.calls {
                let output = self.invoke(&call, sink).await;
                messages.push(Message::ToolResult {
                    id: call.id,
                    content: output.content,
                    is_error: output.is_error,
                });
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
    ) -> Result<Option<Collected>, ProviderError> {
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
                    return Ok(None);
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

        if !calls.is_empty() {
            stop = StopReason::ToolUse;
        }

        Ok(Some(Collected { text, calls, stop }))
    }

    async fn invoke(&self, call: &ToolCall, sink: &Sender<AgentEvent>) -> ToolOutput {
        let _ = sink
            .send(AgentEvent::ToolStarted {
                id: call.id.clone(),
                name: call.name.clone(),
                args: call.args.clone(),
            })
            .await;

        let output = match self.tools.get(&call.name) {
            None => ToolOutput::error(format!("unknown tool: {}", call.name)),
            Some(tool) => match self.permit(&call.name, tool.risk(), &call.args).await {
                GateDecision::Deny(reason) => ToolOutput::error(reason),
                GateDecision::Allow => match tool.run(call.args.clone(), &self.ctx).await {
                    Ok(out) => out,
                    Err(err) => ToolOutput::error(format!("{err:#}")),
                },
            },
        };

        let _ = sink
            .send(AgentEvent::ToolFinished {
                id: call.id.clone(),
                name: call.name.clone(),
                output: output.clone(),
            })
            .await;

        output
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
