use crate::types::{Message, Request, StopReason};
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Event {
    TextDelta(String),
    ToolCallStart { id: String, name: String },
    ToolCallDelta(String),
    ToolCallEnd,
    Usage { input: u32, output: u32, cached: u32 },
    Done(StopReason),
    Error(ProviderError),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ToolSupport {
    Native,
    Prompted,
    None,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Caps {
    pub tools: ToolSupport,
    pub vision: bool,
    pub streaming: bool,
    pub caching: bool,
    pub context_window: u32,
    pub max_output: u32,
}

impl Default for Caps {
    fn default() -> Self {
        Caps {
            tools: ToolSupport::Native,
            vision: false,
            streaming: true,
            caching: false,
            context_window: 128_000,
            max_output: 4096,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    #[error("rate limited: {0}")]
    RateLimit(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("{0}")]
    Other(String),
}

impl ProviderError {
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ProviderError::RateLimit(_) | ProviderError::Auth(_) | ProviderError::Network(_)
        )
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn stream(&self, req: Request) -> Result<BoxStream<'static, Event>, ProviderError>;
    fn caps(&self) -> Caps;
    fn count_tokens(&self, msgs: &[Message]) -> Result<u32, ProviderError>;
}
