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

const CALL_PREFIX: &str = "call_";

pub struct GoogleProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    caps: Caps,
}

impl GoogleProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        GoogleProvider {
            client: reqwest::Client::new(),
            base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
            api_key: api_key.into(),
            caps: Caps {
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

    fn endpoint(&self, model: &str) -> String {
        format!(
            "{}/models/{model}:streamGenerateContent?alt=sse",
            self.base_url
        )
    }
}

fn call_id(name: &str) -> String {
    format!("{CALL_PREFIX}{name}")
}

fn name_from_id(id: &str) -> &str {
    id.strip_prefix(CALL_PREFIX).unwrap_or(id)
}

fn encode_parts(items: &[Content]) -> Vec<Value> {
    items
        .iter()
        .map(|c| match c {
            Content::Text(t) => json!({ "text": t }),
            Content::Image { mime, data } => json!({
                "inline_data": {
                    "mime_type": mime,
                    "data": base64::engine::general_purpose::STANDARD.encode(data)
                }
            }),
        })
        .collect()
}

fn encode_body(req: &Request) -> Value {
    let mut contents = Vec::new();
    let mut system = Vec::new();

    for message in &req.messages {
        match message {
            Message::System(text) => system.push(json!({ "text": text })),
            Message::User(items) => contents.push(json!({
                "role": "user",
                "parts": encode_parts(items)
            })),
            Message::Assistant {
                content,
                tool_calls,
            } => {
                let mut parts = encode_parts(content);
                for call in tool_calls {
                    parts.push(json!({
                        "functionCall": { "name": call.name, "args": call.args }
                    }));
                }
                if parts.is_empty() {
                    parts.push(json!({ "text": "" }));
                }
                contents.push(json!({ "role": "model", "parts": parts }));
            }
            Message::ToolResult { id, content, .. } => contents.push(json!({
                "role": "user",
                "parts": [{
                    "functionResponse": {
                        "name": name_from_id(id),
                        "response": { "result": content }
                    }
                }]
            })),
        }
    }

    let mut body = json!({
        "contents": contents,
        "generationConfig": {
            "temperature": req.temperature,
            "maxOutputTokens": req.max_tokens
        }
    });

    if !system.is_empty() {
        body["systemInstruction"] = json!({ "parts": system });
    }

    if !req.tools.is_empty() {
        let declarations: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.schema
                })
            })
            .collect();
        body["tools"] = json!([{ "functionDeclarations": declarations }]);
    }

    body
}

#[derive(Debug, Default, Deserialize)]
struct StreamResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(default, rename = "usageMetadata")]
    usage: Option<UsageMetadata>,
}

#[derive(Debug, Default, Deserialize)]
struct Candidate {
    #[serde(default)]
    content: CandidateContent,
    #[serde(default, rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct CandidateContent {
    #[serde(default)]
    parts: Vec<Part>,
}

#[derive(Debug, Default, Deserialize)]
struct Part {
    #[serde(default)]
    text: Option<String>,
    #[serde(default, rename = "functionCall")]
    function_call: Option<FunctionCall>,
}

#[derive(Debug, Default, Deserialize)]
struct FunctionCall {
    #[serde(default)]
    name: String,
    #[serde(default)]
    args: Value,
}

#[derive(Debug, Default, Deserialize)]
struct UsageMetadata {
    #[serde(default, rename = "promptTokenCount")]
    prompt: u32,
    #[serde(default, rename = "candidatesTokenCount")]
    candidates: u32,
    #[serde(default, rename = "cachedContentTokenCount")]
    cached: u32,
}

fn stop_reason(finish: &str) -> StopReason {
    match finish {
        "MAX_TOKENS" => StopReason::MaxTokens,
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" => {
            StopReason::Refusal
        }
        _ => StopReason::EndTurn,
    }
}

fn response_events(response: &StreamResponse, stop: &mut Option<StopReason>) -> Vec<Event> {
    let mut out = Vec::new();

    for candidate in &response.candidates {
        for part in &candidate.content.parts {
            if let Some(text) = &part.text
                && !text.is_empty()
            {
                out.push(Event::TextDelta(text.clone()));
            }
            if let Some(call) = &part.function_call {
                out.push(Event::ToolCallStart {
                    id: call_id(&call.name),
                    name: call.name.clone(),
                });
                out.push(Event::ToolCallDelta(call.args.to_string()));
                out.push(Event::ToolCallEnd);
                *stop = Some(StopReason::ToolUse);
            }
        }
        if let Some(finish) = &candidate.finish_reason
            && stop.is_none()
        {
            *stop = Some(stop_reason(finish));
        }
    }

    if let Some(usage) = &response.usage {
        out.push(Event::Usage {
            input: usage.prompt,
            output: usage.candidates,
            cached: usage.cached,
        });
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

fn scrub(text: String, api_key: &str) -> String {
    if api_key.is_empty() {
        return text;
    }
    text.replace(api_key, "<redacted>")
}

#[async_trait]
impl Provider for GoogleProvider {
    async fn stream(&self, req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let response = self
            .client
            .post(self.endpoint(&req.model))
            .header("x-goog-api-key", &self.api_key)
            .json(&encode_body(&req))
            .send()
            .await
            .map_err(|e| ProviderError::Network(scrub(e.to_string(), &self.api_key)))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(http_error(status, scrub(body, &self.api_key)));
        }

        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            let mut events = response.bytes_stream().eventsource();
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
                        let Ok(parsed) = serde_json::from_str::<StreamResponse>(&sse.data) else {
                            continue;
                        };
                        for event in response_events(&parsed, &mut stop) {
                            if tx.send(event).await.is_err() {
                                return;
                            }
                        }
                    }
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
    use zarn_core::types::ToolCall;

    fn feed(chunks: &[&str]) -> (Vec<Event>, Option<StopReason>) {
        let mut stop = None;
        let mut events = Vec::new();
        for chunk in chunks {
            let parsed: StreamResponse = serde_json::from_str(chunk).unwrap();
            events.extend(response_events(&parsed, &mut stop));
        }
        (events, stop)
    }

    #[test]
    fn text_parts_become_deltas() {
        let (events, stop) = feed(&[
            r#"{"candidates":[{"content":{"parts":[{"text":"hi"}]}}]}"#,
            r#"{"candidates":[{"content":{"parts":[]},"finishReason":"STOP"}]}"#,
        ]);
        assert_eq!(events[0], Event::TextDelta("hi".into()));
        assert_eq!(stop, Some(StopReason::EndTurn));
    }

    #[test]
    fn two_function_calls_in_one_chunk_stay_separate() {
        let (events, stop) = feed(&[
            r#"{"candidates":[{"content":{"parts":[
                {"functionCall":{"name":"read","args":{"path":"a"}}},
                {"functionCall":{"name":"glob","args":{"pattern":"*"}}}
            ]}}]}"#,
        ]);

        let names: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                Event::ToolCallStart { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["read", "glob"]);
        assert_eq!(
            events.iter().filter(|e| **e == Event::ToolCallEnd).count(),
            2
        );
        assert_eq!(stop, Some(StopReason::ToolUse));
    }

    #[test]
    fn usage_metadata_is_reported() {
        let (events, _) = feed(&[
            r#"{"candidates":[],"usageMetadata":{"promptTokenCount":12,"candidatesTokenCount":3,"cachedContentTokenCount":4}}"#,
        ]);
        assert_eq!(
            events[0],
            Event::Usage {
                input: 12,
                output: 3,
                cached: 4
            }
        );
    }

    #[test]
    fn a_safety_stop_is_a_refusal() {
        let (_, stop) = feed(&[
            r#"{"candidates":[{"content":{"parts":[]},"finishReason":"SAFETY"}]}"#,
        ]);
        assert_eq!(stop, Some(StopReason::Refusal));
    }

    #[test]
    fn a_tool_result_is_sent_back_under_the_function_name() {
        let req = Request {
            messages: vec![
                Message::Assistant {
                    content: Vec::new(),
                    tool_calls: vec![ToolCall {
                        id: call_id("read"),
                        name: "read".into(),
                        args: json!({}),
                    }],
                },
                Message::ToolResult {
                    id: call_id("read"),
                    content: "file contents".into(),
                    is_error: false,
                },
            ],
            tools: Vec::new(),
            model: "gemini".into(),
            max_tokens: 10,
            temperature: 0.0,
        };

        let body = encode_body(&req);
        let name = body["contents"][1]["parts"][0]["functionResponse"]["name"]
            .as_str()
            .unwrap();
        assert_eq!(name, "read");
    }

    #[test]
    fn the_api_key_never_survives_into_an_error_string() {
        let leaked = "https://example/v1?key=SECRET123 failed".to_string();
        assert!(!scrub(leaked, "SECRET123").contains("SECRET123"));
    }
}
