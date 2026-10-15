use async_trait::async_trait;
use base64::Engine;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use zarn_core::provider::{Caps, Event, Provider, ProviderError};
use zarn_core::types::{Content, Message, Request, StopReason};

const API_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    caps: Caps,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        AnthropicProvider {
            client: reqwest::Client::new(),
            base_url: "https://api.anthropic.com/v1".to_string(),
            api_key: api_key.into(),
            caps: Caps {
                caching: true,
                vision: true,
                ..Caps::default()
            },
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    pub fn with_caps(mut self, caps: Caps) -> Self {
        self.caps = caps;
        self
    }

    fn endpoint(&self) -> String {
        format!("{}/messages", self.base_url)
    }
}

fn cache_control() -> Value {
    json!({ "type": "ephemeral" })
}

fn encode_content(items: &[Content]) -> Vec<Value> {
    items
        .iter()
        .map(|c| match c {
            Content::Text(t) => json!({ "type": "text", "text": t }),
            Content::Image { mime, data } => json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": mime,
                    "data": base64::engine::general_purpose::STANDARD.encode(data)
                }
            }),
        })
        .collect()
}

fn encode_body(req: &Request) -> Value {
    let mut system: Vec<Value> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();

    for message in &req.messages {
        match message {
            Message::System(text) => system.push(json!({ "type": "text", "text": text })),
            Message::User(items) => messages.push(json!({
                "role": "user",
                "content": encode_content(items)
            })),
            Message::Assistant {
                content,
                tool_calls,
            } => {
                let mut blocks = encode_content(content);
                for call in tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.args
                    }));
                }
                if blocks.is_empty() {
                    continue;
                }
                messages.push(json!({ "role": "assistant", "content": blocks }));
            }
            Message::ToolResult {
                id,
                content,
                is_error,
            } => messages.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": content,
                    "is_error": is_error
                }]
            })),
        }
    }

    if let Some(last) = system.last_mut() {
        last["cache_control"] = cache_control();
    }

    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "max_tokens": req.max_tokens,
        "temperature": req.temperature,
        "stream": true
    });

    if !system.is_empty() {
        body["system"] = Value::Array(system);
    }

    if !req.tools.is_empty() {
        let mut tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.schema
                })
            })
            .collect();
        if let Some(last) = tools.last_mut() {
            last["cache_control"] = cache_control();
        }
        body["tools"] = Value::Array(tools);
    }

    body
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Wire {
    MessageStart {
        #[serde(default)]
        message: MessageStart,
    },
    ContentBlockStart {
        #[serde(default)]
        content_block: Block,
    },
    ContentBlockDelta {
        #[serde(default)]
        delta: Delta,
    },
    ContentBlockStop,
    MessageDelta {
        #[serde(default)]
        delta: MessageDeltaBody,
        #[serde(default)]
        usage: Option<Usage>,
    },
    MessageStop,
    #[serde(other)]
    Other,
}

#[derive(Debug, Default, Deserialize)]
struct MessageStart {
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Default, Deserialize)]
struct Block {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Delta {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    partial_json: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct MessageDeltaBody {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
}

fn stop_reason(reason: &str) -> StopReason {
    match reason {
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "refusal" => StopReason::Refusal,
        _ => StopReason::EndTurn,
    }
}

#[derive(Default)]
struct Tracker {
    tool_open: bool,
}

fn wire_events(frame: Wire, tracker: &mut Tracker, stop: &mut Option<StopReason>) -> Vec<Event> {
    let mut out = Vec::new();

    match frame {
        Wire::MessageStart { message } => {
            if let Some(usage) = message.usage {
                out.push(Event::Usage {
                    input: usage.input_tokens,
                    output: usage.output_tokens,
                    cached: usage.cache_read_input_tokens,
                });
            }
        }
        Wire::ContentBlockStart { content_block } => {
            if content_block.kind == "tool_use" {
                tracker.tool_open = true;
                out.push(Event::ToolCallStart {
                    id: content_block.id.unwrap_or_default(),
                    name: content_block.name.unwrap_or_default(),
                });
            }
        }
        Wire::ContentBlockDelta { delta } => match delta.kind.as_str() {
            "text_delta" => {
                if let Some(text) = delta.text
                    && !text.is_empty()
                {
                    out.push(Event::TextDelta(text));
                }
            }
            "input_json_delta" => {
                if let Some(fragment) = delta.partial_json
                    && !fragment.is_empty()
                {
                    out.push(Event::ToolCallDelta(fragment));
                }
            }
            _ => {}
        },
        Wire::ContentBlockStop => {
            if tracker.tool_open {
                tracker.tool_open = false;
                out.push(Event::ToolCallEnd);
            }
        }
        Wire::MessageDelta { delta, usage } => {
            if let Some(reason) = delta.stop_reason {
                *stop = Some(stop_reason(&reason));
            }
            if let Some(usage) = usage {
                out.push(Event::Usage {
                    input: usage.input_tokens,
                    output: usage.output_tokens,
                    cached: usage.cache_read_input_tokens,
                });
            }
        }
        Wire::MessageStop | Wire::Other => {}
    }

    out
}

fn http_error(status: reqwest::StatusCode, body: String) -> ProviderError {
    match status.as_u16() {
        401 | 403 => ProviderError::Auth(body),
        429 => ProviderError::RateLimit(body),
        400 | 404 | 422 => ProviderError::BadRequest(body),
        500..=599 => ProviderError::Network(body),
        _ => ProviderError::Other(body),
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn stream(&self, req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let response = self
            .client
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&encode_body(&req))
            .send()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(http_error(status, body));
        }

        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            let mut events = response.bytes_stream().eventsource();
            let mut tracker = Tracker::default();
            let mut stop = None;

            while let Some(item) = events.next().await {
                match item {
                    Err(e) => {
                        let _ = tx
                            .send(Event::Error(ProviderError::Network(e.to_string())))
                            .await;
                        return;
                    }
                    Ok(sse) => {
                        let Ok(frame) = serde_json::from_str::<Wire>(&sse.data) else {
                            continue;
                        };
                        for event in wire_events(frame, &mut tracker, &mut stop) {
                            if tx.send(event).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }

            if tracker.tool_open {
                let _ = tx.send(Event::ToolCallEnd).await;
            }
            let _ = tx
                .send(Event::Done(stop.unwrap_or(StopReason::EndTurn)))
                .await;
        });

        Ok(ReceiverStream::new(rx).boxed())
    }

    fn caps(&self) -> Caps {
        self.caps
    }

    fn count_tokens(&self, msgs: &[Message]) -> Result<u32, ProviderError> {
        let bpe = tiktoken_rs::cl100k_base()
            .map_err(|e| ProviderError::Other(format!("tokenizer unavailable: {e}")))?;
        let text = msgs.iter().map(Message::text).collect::<Vec<_>>().join("\n");
        Ok(bpe.encode_with_special_tokens(&text).len() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zarn_core::types::ToolSpec;

    fn feed(frames: &[&str]) -> (Vec<Event>, Option<StopReason>) {
        let mut tracker = Tracker::default();
        let mut stop = None;
        let mut events = Vec::new();
        for frame in frames {
            let parsed: Wire = serde_json::from_str(frame).unwrap();
            events.extend(wire_events(parsed, &mut tracker, &mut stop));
        }
        (events, stop)
    }

    fn tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: String::new(),
            schema: json!({ "type": "object" }),
        }
    }

    #[test]
    fn tool_arguments_split_across_frames_are_reassembled() {
        let (events, stop) = feed(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"read"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"pa"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"th\":\"a.rs\"}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}"#,
        ]);

        let args: String = events
            .iter()
            .filter_map(|e| match e {
                Event::ToolCallDelta(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(args, r#"{"path":"a.rs"}"#);
        assert_eq!(
            events.iter().filter(|e| **e == Event::ToolCallEnd).count(),
            1
        );
        assert_eq!(stop, Some(StopReason::ToolUse));
    }

    #[test]
    fn output_tokens_from_message_delta_are_not_lost() {
        let (events, _) = feed(&[
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":42}}"#,
        ]);
        assert_eq!(
            events[0],
            Event::Usage {
                input: 0,
                output: 42,
                cached: 0
            }
        );
    }

    #[test]
    fn a_text_block_does_not_emit_tool_events() {
        let (events, _) = feed(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
        ]);
        assert_eq!(events, vec![Event::TextDelta("hi".into())]);
    }

    #[test]
    fn a_refusal_is_not_reported_as_a_normal_finish() {
        let (_, stop) = feed(&[r#"{"type":"message_delta","delta":{"stop_reason":"refusal"}}"#]);
        assert_eq!(stop, Some(StopReason::Refusal));
    }

    #[test]
    fn only_the_last_tool_carries_a_cache_breakpoint() {
        let req = Request {
            messages: vec![Message::System("sys".into()), Message::user("hi")],
            tools: vec![tool("a"), tool("b"), tool("c")],
            model: "claude".into(),
            max_tokens: 10,
            temperature: 0.0,
        };
        let body = encode_body(&req);
        let tools = body["tools"].as_array().unwrap();

        assert!(tools[0].get("cache_control").is_none());
        assert!(tools[1].get("cache_control").is_none());
        assert!(tools[2].get("cache_control").is_some());

        let breakpoints = body["system"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|b| b.get("cache_control").is_some())
            .count()
            + 1;
        assert!(breakpoints <= 4);
    }

    #[test]
    fn an_unknown_frame_type_is_ignored_rather_than_fatal() {
        let parsed: Wire = serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
        let mut tracker = Tracker::default();
        let mut stop = None;
        assert!(wire_events(parsed, &mut tracker, &mut stop).is_empty());
    }
}
