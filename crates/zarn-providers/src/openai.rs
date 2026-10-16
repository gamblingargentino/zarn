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

pub struct OpenAiCompatProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    caps: Caps,
}

impl OpenAiCompatProvider {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        OpenAiCompatProvider {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            caps: Caps::default(),
        }
    }

    pub fn with_caps(mut self, caps: Caps) -> Self {
        self.caps = caps;
        self
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }
}

fn encode_content(items: &[Content]) -> Value {
    if items.iter().all(|c| matches!(c, Content::Text(_))) {
        return Value::String(
            items
                .iter()
                .filter_map(Content::as_text)
                .collect::<Vec<_>>()
                .join(""),
        );
    }
    let parts: Vec<Value> = items
        .iter()
        .map(|c| match c {
            Content::Text(t) => json!({ "type": "text", "text": t }),
            Content::Image { mime, data } => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(data);
                json!({
                    "type": "image_url",
                    "image_url": { "url": format!("data:{mime};base64,{encoded}") }
                })
            }
        })
        .collect();
    Value::Array(parts)
}

fn encode_messages(messages: &[Message]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| match m {
            Message::System(s) => json!({ "role": "system", "content": s }),
            Message::User(c) => json!({ "role": "user", "content": encode_content(c) }),
            Message::Assistant {
                content,
                tool_calls,
            } => {
                let mut obj = json!({ "role": "assistant", "content": encode_content(content) });
                if !tool_calls.is_empty() {
                    let calls: Vec<Value> = tool_calls
                        .iter()
                        .map(|t| {
                            json!({
                                "id": t.id,
                                "type": "function",
                                "function": { "name": t.name, "arguments": t.args.to_string() }
                            })
                        })
                        .collect();
                    obj["tool_calls"] = Value::Array(calls);
                }
                obj
            }
            Message::ToolResult { id, content, .. } => json!({
                "role": "tool",
                "tool_call_id": id,
                "content": content
            }),
        })
        .collect()
}

fn encode_body(req: &Request) -> Value {
    let mut body = json!({
        "model": req.model,
        "messages": encode_messages(&req.messages),
        "stream": true,
        "stream_options": { "include_usage": true },
        "max_tokens": req.max_tokens,
        "temperature": req.temperature,
    });
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.schema
                    }
                })
            })
            .collect();
        body["tools"] = Value::Array(tools);
    }
    body
}

#[derive(Debug, Default, Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, Default, Deserialize)]
struct Choice {
    #[serde(default)]
    delta: Delta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<DeltaToolCall>,
}

#[derive(Debug, Default, Deserialize)]
struct DeltaToolCall {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<DeltaFunction>,
}

#[derive(Debug, Default, Deserialize)]
struct DeltaFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    prompt_tokens_details: Option<PromptDetails>,
}

#[derive(Debug, Default, Deserialize)]
struct PromptDetails {
    #[serde(default)]
    cached_tokens: u32,
}

fn stop_reason(finish: &str) -> StopReason {
    match finish {
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "length" => StopReason::MaxTokens,
        "content_filter" => StopReason::Refusal,
        _ => StopReason::EndTurn,
    }
}

#[derive(Default)]
struct CallTracker {
    open: Option<usize>,
}

impl CallTracker {
    fn apply(&mut self, call: &DeltaToolCall, out: &mut Vec<Event>) {
        if self.open != Some(call.index) {
            if self.open.is_some() {
                out.push(Event::ToolCallEnd);
            }
            out.push(Event::ToolCallStart {
                id: call
                    .id
                    .clone()
                    .unwrap_or_else(|| format!("call_{}", call.index)),
                name: call
                    .function
                    .as_ref()
                    .and_then(|f| f.name.clone())
                    .unwrap_or_default(),
            });
            self.open = Some(call.index);
        }

        if let Some(args) = call.function.as_ref().and_then(|f| f.arguments.as_ref())
            && !args.is_empty()
        {
            out.push(Event::ToolCallDelta(args.clone()));
        }
    }

    fn finish(&mut self, out: &mut Vec<Event>) {
        if self.open.take().is_some() {
            out.push(Event::ToolCallEnd);
        }
    }
}

fn chunk_events(
    chunk: &Chunk,
    tracker: &mut CallTracker,
    stop: &mut Option<StopReason>,
) -> Vec<Event> {
    let mut out = Vec::new();

    for choice in &chunk.choices {
        if let Some(text) = &choice.delta.content
            && !text.is_empty()
        {
            out.push(Event::TextDelta(text.clone()));
        }
        for call in &choice.delta.tool_calls {
            tracker.apply(call, &mut out);
        }
        if let Some(finish) = &choice.finish_reason {
            tracker.finish(&mut out);
            *stop = Some(stop_reason(finish));
        }
    }

    if let Some(usage) = &chunk.usage {
        out.push(Event::Usage {
            input: usage.prompt_tokens,
            output: usage.completion_tokens,
            cached: usage
                .prompt_tokens_details
                .as_ref()
                .map(|d| d.cached_tokens)
                .unwrap_or(0),
        });
    }

    out
}

fn http_error(status: reqwest::StatusCode, body: String) -> ProviderError {
    let body = format!("{} (HTTP {})", crate::error_message(&body), status.as_u16());
    match status.as_u16() {
        401 | 403 => ProviderError::Auth(body),
        429 => ProviderError::RateLimit(body),
        400 | 404 | 422 => ProviderError::BadRequest(body),
        500..=599 => ProviderError::Network(body),
        _ => ProviderError::Other(body),
    }
}

#[async_trait]
impl Provider for OpenAiCompatProvider {
    async fn stream(&self, req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let mut body = encode_body(&req);
        let mut retried = false;
        let response = loop {
            let response = self
                .client
                .post(self.endpoint())
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await
                .map_err(|e| ProviderError::Network(e.to_string()))?;
            let status = response.status();
            if status.is_success() {
                break response;
            }
            let text = response.text().await.unwrap_or_default();
            if status.as_u16() == 402
                && !retried
                && let Some(limit) = crate::affordable_tokens(&crate::error_message(&text))
                && limit >= 64
                && limit < req.max_tokens
            {
                retried = true;
                body["max_tokens"] = json!(limit);
                continue;
            }
            return Err(http_error(status, text));
        };

        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            let mut events = response.bytes_stream().eventsource();
            let mut tracker = CallTracker::default();
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
                        if sse.data.trim() == "[DONE]" {
                            break;
                        }
                        let Ok(chunk) = serde_json::from_str::<Chunk>(&sse.data) else {
                            continue;
                        };
                        for event in chunk_events(&chunk, &mut tracker, &mut stop) {
                            if tx.send(event).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }

            let mut tail = Vec::new();
            tracker.finish(&mut tail);
            for event in tail {
                if tx.send(event).await.is_err() {
                    return;
                }
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

    fn feed(datas: &[&str]) -> Vec<Event> {
        let mut tracker = CallTracker::default();
        let mut stop = None;
        let mut events = Vec::new();
        for data in datas {
            let chunk: Chunk = serde_json::from_str(data).unwrap();
            events.extend(chunk_events(&chunk, &mut tracker, &mut stop));
        }
        tracker.finish(&mut events);
        events
    }

    #[test]
    fn tool_call_arguments_split_across_chunks_are_reassembled() {
        let events = feed(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read","arguments":"{\"pa"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a.rs\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
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
    }

    #[test]
    fn two_tool_calls_close_the_first_before_opening_the_second() {
        let events = feed(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"one","arguments":"{}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"b","function":{"name":"two","arguments":"{}"}}]}}]}"#,
        ]);

        let shape: Vec<&str> = events
            .iter()
            .map(|e| match e {
                Event::ToolCallStart { .. } => "start",
                Event::ToolCallDelta(_) => "delta",
                Event::ToolCallEnd => "end",
                _ => "other",
            })
            .collect();

        assert_eq!(
            shape,
            vec!["start", "delta", "end", "start", "delta", "end"]
        );
    }

    #[test]
    fn text_and_usage_map_through() {
        let events = feed(&[
            r#"{"choices":[{"delta":{"content":"hi"}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":4}}}"#,
        ]);

        assert_eq!(events[0], Event::TextDelta("hi".into()));
        assert_eq!(
            events[1],
            Event::Usage {
                input: 10,
                output: 2,
                cached: 4
            }
        );
    }
}
