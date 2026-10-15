use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Content {
    Text(String),
    Image { mime: String, data: Vec<u8> },
}

impl Content {
    pub fn text(s: impl Into<String>) -> Self {
        Content::Text(s.into())
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Content::Text(t) => Some(t),
            Content::Image { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Message {
    System(String),
    User(Vec<Content>),
    Assistant {
        content: Vec<Content>,
        tool_calls: Vec<ToolCall>,
    },
    ToolResult {
        id: String,
        content: String,
        is_error: bool,
    },
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Message::User(vec![Content::Text(text.into())])
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Message::Assistant {
            content: vec![Content::Text(text.into())],
            tool_calls: Vec::new(),
        }
    }

    pub fn text(&self) -> String {
        match self {
            Message::System(s) => s.clone(),
            Message::User(c) | Message::Assistant { content: c, .. } => c
                .iter()
                .filter_map(Content::as_text)
                .collect::<Vec<_>>()
                .join(""),
            Message::ToolResult { content, .. } => content.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Request {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
}

impl Request {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Request {
            messages,
            tools: Vec::new(),
            model: model.into(),
            max_tokens: 4096,
            temperature: 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Cancelled,
    Refusal,
}
