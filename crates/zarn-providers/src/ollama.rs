use async_trait::async_trait;
use base64::Engine;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use zarn_core::provider::{Caps, Event, Provider, ProviderError, ToolSupport};
use zarn_core::types::{Content, Message, Request, StopReason};

pub struct OllamaProvider {
    client: reqwest::Client,
    base_url: String,
    caps: Caps,
}

impl OllamaProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        OllamaProvider {
            client: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            caps: Caps {
                tools: ToolSupport::Prompted,
                context_window: 8192,
                ..Caps::default()
            },
        }
    }

    pub fn local() -> Self {
        Self::new("http://localhost:11434")
    }

    pub fn with_caps(mut self, caps: Caps) -> Self {
        self.caps = caps;
        self
    }

    pub async fn models(&self) -> Result<Vec<String>, ProviderError> {
        let response = self
            .client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let body: Value = response
            .json()
            .await
            .map_err(|e| ProviderError::Other(e.to_string()))?;

        Ok(body
            .get("models")
            .and_then(Value::as_array)
            .map(|models| {
                models
                    .iter()
                    .filter_map(|m| m.get("name").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn context_window(&self, model: &str) -> Result<Option<u32>, ProviderError> {
        let response = self
            .client
            .post(format!("{}/api/show", self.base_url))
            .json(&json!({ "model": model }))
            .send()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let body: Value = response
            .json()
            .await
            .map_err(|e| ProviderError::Other(e.to_string()))?;

        Ok(body
            .get("model_info")
            .and_then(Value::as_object)
            .and_then(|info| {
                info.iter()
                    .find(|(k, _)| k.ends_with(".context_length"))
                    .and_then(|(_, v)| v.as_u64())
            })
            .map(|v| v as u32))
    }

    pub async fn probed(mut self, model: &str) -> Self {
        if let Ok(Some(window)) = self.context_window(model).await {
            self.caps.context_window = window;
        }
        self
    }
}

fn encode_messages(messages: &[Message]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| match m {
            Message::System(text) => json!({ "role": "system", "content": text }),
            Message::User(items) => {
                let mut obj = json!({ "role": "user", "content": text_of(items) });
                let images = images_of(items);
                if !images.is_empty() {
                    obj["images"] = Value::Array(images);
                }
                obj
            }
            Message::Assistant { content, .. } => {
                json!({ "role": "assistant", "content": text_of(content) })
            }
            Message::ToolResult { content, .. } => {
                json!({ "role": "tool", "content": content })
            }
        })
        .collect()
}

fn text_of(items: &[Content]) -> String {
    items
        .iter()
        .filter_map(Content::as_text)
        .collect::<Vec<_>>()
        .join("")
}

fn images_of(items: &[Content]) -> Vec<Value> {
    items
        .iter()
        .filter_map(|c| match c {
            Content::Image { data, .. } => Some(Value::String(
                base64::engine::general_purpose::STANDARD.encode(data),
            )),
            Content::Text(_) => None,
        })
        .collect()
}

fn encode_body(req: &Request) -> Value {
    json!({
        "model": req.model,
        "messages": encode_messages(&req.messages),
        "stream": true,
        "options": {
            "temperature": req.temperature,
            "num_predict": req.max_tokens
        }
    })
}

#[derive(Debug, Default, Deserialize)]
struct Chunk {
    #[serde(default)]
    message: ChunkMessage,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    done_reason: Option<String>,
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    #[serde(default)]
    eval_count: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
struct ChunkMessage {
    #[serde(default)]
    content: String,
}

fn stop_reason(reason: Option<&str>) -> StopReason {
    match reason {
        Some("length") => StopReason::MaxTokens,
        _ => StopReason::EndTurn,
    }
}

fn chunk_events(chunk: &Chunk, stop: &mut Option<StopReason>) -> Vec<Event> {
    let mut out = Vec::new();

    if !chunk.message.content.is_empty() {
        out.push(Event::TextDelta(chunk.message.content.clone()));
    }

    if chunk.done {
        if chunk.prompt_eval_count.is_some() || chunk.eval_count.is_some() {
            out.push(Event::Usage {
                input: chunk.prompt_eval_count.unwrap_or(0),
                output: chunk.eval_count.unwrap_or(0),
                cached: 0,
            });
        }
        *stop = Some(stop_reason(chunk.done_reason.as_deref()));
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
impl Provider for OllamaProvider {
    async fn stream(&self, req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let response = self
            .client
            .post(format!("{}/api/chat", self.base_url))
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
            let mut bytes = response.bytes_stream();
            let mut buffer: Vec<u8> = Vec::new();
            let mut stop = None;

            while let Some(item) = bytes.next().await {
                let Ok(block) = item else {
                    let _ = tx
                        .send(Event::Error(ProviderError::Network(
                            "stream interrupted".into(),
                        )))
                        .await;
                    return;
                };
                buffer.extend_from_slice(&block);

                while let Some(at) = buffer.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = buffer.drain(..=at).collect();
                    let Ok(text) = std::str::from_utf8(&line[..line.len() - 1]) else {
                        continue;
                    };
                    if text.trim().is_empty() {
                        continue;
                    }
                    let Ok(chunk) = serde_json::from_str::<Chunk>(text) else {
                        continue;
                    };
                    for event in chunk_events(&chunk, &mut stop) {
                        if tx.send(event).await.is_err() {
                            return;
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

    fn feed(lines: &[&str]) -> (Vec<Event>, Option<StopReason>) {
        let mut stop = None;
        let mut events = Vec::new();
        for line in lines {
            let chunk: Chunk = serde_json::from_str(line).unwrap();
            events.extend(chunk_events(&chunk, &mut stop));
        }
        (events, stop)
    }

    #[test]
    fn content_becomes_text_deltas() {
        let (events, _) = feed(&[
            r#"{"message":{"content":"he"},"done":false}"#,
            r#"{"message":{"content":"llo"},"done":false}"#,
        ]);
        assert_eq!(
            events,
            vec![
                Event::TextDelta("he".into()),
                Event::TextDelta("llo".into())
            ]
        );
    }

    #[test]
    fn the_final_chunk_reports_usage() {
        let (events, stop) = feed(&[
            r#"{"message":{"content":""},"done":true,"done_reason":"stop","prompt_eval_count":11,"eval_count":5}"#,
        ]);
        assert_eq!(
            events[0],
            Event::Usage {
                input: 11,
                output: 5,
                cached: 0
            }
        );
        assert_eq!(stop, Some(StopReason::EndTurn));
    }

    #[test]
    fn a_length_stop_is_not_reported_as_a_clean_finish() {
        let (_, stop) = feed(&[r#"{"message":{"content":""},"done":true,"done_reason":"length"}"#]);
        assert_eq!(stop, Some(StopReason::MaxTokens));
    }

    #[test]
    fn generation_options_are_nested_where_ollama_reads_them() {
        let req = Request {
            messages: vec![Message::user("hi")],
            tools: Vec::new(),
            model: "llama3".into(),
            max_tokens: 256,
            temperature: 0.4,
        };
        let body = encode_body(&req);
        assert_eq!(body["options"]["num_predict"], json!(256));
        assert!((body["options"]["temperature"].as_f64().unwrap() - 0.4).abs() < 1e-6);
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn images_are_carried_on_the_message_rather_than_dropped() {
        let req = Request {
            messages: vec![Message::User(vec![
                Content::Text("look".into()),
                Content::Image {
                    mime: "image/png".into(),
                    data: vec![1, 2, 3],
                },
            ])],
            tools: Vec::new(),
            model: "llava".into(),
            max_tokens: 10,
            temperature: 0.0,
        };
        let body = encode_body(&req);
        assert_eq!(body["messages"][0]["content"], json!("look"));
        assert_eq!(body["messages"][0]["images"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn tools_are_prompted_so_the_wrapper_must_inject_them() {
        assert_eq!(OllamaProvider::local().caps().tools, ToolSupport::Prompted);
    }
}
