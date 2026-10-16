use crate::auth::redact;
use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::Value;
use std::io::Write;
use std::sync::Arc;
use zarn_core::provider::{Caps, Event, Provider, ProviderError};
use zarn_core::tools::{Ctx, Risk, Tool, ToolOutput, ToolRegistry};
use zarn_core::types::{Content, Message, Request, ToolSpec};

pub fn redact_error(err: ProviderError) -> ProviderError {
    match err {
        ProviderError::RateLimit(s) => ProviderError::RateLimit(redact(&s)),
        ProviderError::Auth(s) => ProviderError::Auth(redact(&s)),
        ProviderError::Network(s) => ProviderError::Network(redact(&s)),
        ProviderError::BadRequest(s) => ProviderError::BadRequest(redact(&s)),
        ProviderError::Other(s) => ProviderError::Other(redact(&s)),
    }
}

pub struct RedactingProvider {
    inner: Arc<dyn Provider>,
}

impl RedactingProvider {
    pub fn wrap(inner: Arc<dyn Provider>) -> Arc<dyn Provider> {
        Arc::new(RedactingProvider { inner })
    }
}

#[async_trait]
impl Provider for RedactingProvider {
    async fn stream(&self, req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let stream = self.inner.stream(req).await.map_err(redact_error)?;
        Ok(stream
            .map(|event| match event {
                Event::Error(err) => Event::Error(redact_error(err)),
                other => other,
            })
            .boxed())
    }

    fn caps(&self) -> Caps {
        self.inner.caps()
    }

    fn count_tokens(&self, msgs: &[Message]) -> Result<u32, ProviderError> {
        self.inner.count_tokens(msgs).map_err(redact_error)
    }
}

pub fn guarded_chain(specs: &[zarn_providers::ProviderSpec]) -> Result<Arc<dyn Provider>, ProviderError> {
    let providers = specs
        .iter()
        .map(|spec| zarn_providers::build(spec).map(RedactingProvider::wrap))
        .collect::<Result<Vec<_>, _>>()
        .map_err(redact_error)?;
    match providers.len() {
        0 => Err(ProviderError::Other("no providers configured".into())),
        1 => Ok(providers.into_iter().next().expect("one provider")),
        _ => Ok(Arc::new(zarn_providers::FallbackProvider::new(providers))),
    }
}

pub struct RedactingTool {
    inner: Box<dyn Tool>,
}

#[async_trait]
impl Tool for RedactingTool {
    fn spec(&self) -> ToolSpec {
        self.inner.spec()
    }

    fn risk(&self) -> Risk {
        self.inner.risk()
    }

    async fn run(&self, args: Value, ctx: &Ctx) -> anyhow::Result<ToolOutput> {
        match self.inner.run(args, ctx).await {
            Ok(out) => Ok(ToolOutput {
                content: redact(&out.content),
                is_error: out.is_error,
            }),
            Err(err) => Err(anyhow::anyhow!(redact(&format!("{err:#}")))),
        }
    }
}

pub fn redacting(tool: Box<dyn Tool>) -> Box<dyn Tool> {
    Box::new(RedactingTool { inner: tool })
}

pub fn guard_registry(tools: Vec<Box<dyn Tool>>) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for tool in tools {
        registry.register(redacting(tool));
    }
    registry
}

pub fn redact_message(message: &Message) -> Message {
    let content = |parts: &[Content]| {
        parts
            .iter()
            .map(|part| match part {
                Content::Text(t) => Content::Text(redact(t)),
                other => other.clone(),
            })
            .collect::<Vec<_>>()
    };
    match message {
        Message::System(s) => Message::System(redact(s)),
        Message::User(parts) => Message::User(content(parts)),
        Message::Assistant {
            content: parts,
            tool_calls,
        } => Message::Assistant {
            content: content(parts),
            tool_calls: tool_calls
                .iter()
                .map(|call| {
                    let mut call = call.clone();
                    let raw = call.args.to_string();
                    let clean = redact(&raw);
                    if clean != raw {
                        call.args = serde_json::from_str(&clean).unwrap_or(Value::String(clean));
                    }
                    call
                })
                .collect(),
        },
        Message::ToolResult {
            id,
            content,
            is_error,
        } => Message::ToolResult {
            id: id.clone(),
            content: redact(content),
            is_error: *is_error,
        },
    }
}

pub struct RedactingWriter<W: Write> {
    inner: W,
    pending: Vec<u8>,
}

impl<W: Write> RedactingWriter<W> {
    pub fn new(inner: W) -> Self {
        RedactingWriter {
            inner,
            pending: Vec::new(),
        }
    }

    fn emit(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let text = String::from_utf8_lossy(bytes);
        self.inner.write_all(redact(&text).as_bytes())
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(buf);
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            self.emit(&line)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.pending.is_empty() {
            let rest = std::mem::take(&mut self.pending);
            self.emit(&rest)?;
        }
        self.inner.flush()
    }
}

impl<W: Write> Drop for RedactingWriter<W> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{REDACTED, register_secret};

    struct Leaky;

    #[async_trait]
    impl Tool for Leaky {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "leaky".into(),
                description: String::new(),
                schema: Value::Null,
            }
        }

        fn risk(&self) -> Risk {
            Risk::Safe
        }

        async fn run(&self, _args: Value, _ctx: &Ctx) -> anyhow::Result<ToolOutput> {
            Ok(ToolOutput::ok("env: test-key-not-real-tool-output"))
        }
    }

    #[tokio::test]
    async fn tool_output_never_carries_a_known_key() {
        register_secret("test-key-not-real-tool-output");
        let tool = redacting(Box::new(Leaky));
        let out = tool.run(Value::Null, &Ctx::new(".")).await.unwrap();
        assert_eq!(out.content, format!("env: {REDACTED}"));
    }

    #[test]
    fn the_log_writer_redacts_whole_lines() {
        register_secret("test-key-not-real-log-line");
        let mut buf = Vec::new();
        {
            let mut writer = RedactingWriter::new(&mut buf);
            writer.write_all(b"auth failed for test-key-not").unwrap();
            writer.write_all(b"-real-log-line\nnext").unwrap();
        }
        let text = String::from_utf8(buf).unwrap();
        assert!(!text.contains("test-key-not-real-log-line"));
        assert!(text.ends_with("next"));
    }

    #[test]
    fn provider_errors_are_redacted() {
        register_secret("test-key-not-real-provider-err");
        let err = redact_error(ProviderError::Auth("bad key test-key-not-real-provider-err".into()));
        assert!(!err.to_string().contains("test-key-not-real-provider-err"));
    }
}
