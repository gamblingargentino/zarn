use async_trait::async_trait;
use base64::Engine;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use zarn_core::provider::{Caps, Event, Provider, ProviderError};
use zarn_core::types::{Content, Message, Request, StopReason};

const CALL_PREFIX: &str = "call_";
const CALL_MARK: &str = "-z";
const SKIP_SIGNATURE: &str = "skip_thought_signature_validator";

type Signatures = Arc<Mutex<HashMap<String, String>>>;

pub struct GoogleProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    caps: Caps,
    signatures: Signatures,
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
            signatures: Signatures::default(),
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
        let model = model.strip_prefix("models/").unwrap_or(model);
        format!(
            "{}/models/{model}:streamGenerateContent?alt=sse",
            self.base_url
        )
    }
}

fn call_id(name: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    static SEED: OnceLock<u64> = OnceLock::new();
    let seed = *SEED.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64 % 1_000_000_000)
            .unwrap_or(0)
    });
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("{CALL_PREFIX}{name}{CALL_MARK}{seed}x{n}")
}

fn name_from_id(id: &str) -> &str {
    let rest = id.strip_prefix(CALL_PREFIX).unwrap_or(id);
    match rest.rsplit_once(CALL_MARK) {
        Some((name, tail))
            if !name.is_empty()
                && !tail.is_empty()
                && tail.chars().all(|c| c.is_ascii_digit() || c == 'x') =>
        {
            name
        }
        _ => rest,
    }
}

fn needs_signature(model: &str) -> bool {
    let model = model.rsplit('/').next().unwrap_or(model);
    !["gemini-1", "gemini-2", "gemma"]
        .iter()
        .any(|old| model.starts_with(old))
}

const SCHEMA_KEYS: &[&str] = &[
    "type",
    "format",
    "title",
    "description",
    "nullable",
    "enum",
    "maxItems",
    "minItems",
    "properties",
    "required",
    "minProperties",
    "maxProperties",
    "minLength",
    "maxLength",
    "pattern",
    "anyOf",
    "items",
    "minimum",
    "maximum",
];

fn gemini_schema(schema: &Value) -> Value {
    let Some(object) = schema.as_object() else {
        return schema.clone();
    };
    let mut out = serde_json::Map::new();
    for (key, value) in object {
        if !SCHEMA_KEYS.contains(&key.as_str()) {
            continue;
        }
        let value = match key.as_str() {
            "properties" => match value.as_object() {
                Some(props) => Value::Object(
                    props
                        .iter()
                        .map(|(k, v)| (k.clone(), gemini_schema(v)))
                        .collect(),
                ),
                None => continue,
            },
            "items" => gemini_schema(value),
            "anyOf" => match value.as_array() {
                Some(list) => Value::Array(list.iter().map(gemini_schema).collect()),
                None => continue,
            },
            "type" => match value {
                Value::Array(types) => {
                    let named: Vec<&str> = types.iter().filter_map(Value::as_str).collect();
                    if named.contains(&"null") {
                        out.insert("nullable".into(), Value::Bool(true));
                    }
                    match named.into_iter().find(|t| *t != "null") {
                        Some(t) => Value::String(t.to_string()),
                        None => continue,
                    }
                }
                other => other.clone(),
            },
            "enum" => match value.as_array() {
                Some(list) => Value::Array(
                    list.iter()
                        .map(|v| match v {
                            Value::String(_) => v.clone(),
                            other => Value::String(other.to_string()),
                        })
                        .collect(),
                ),
                None => continue,
            },
            _ => value.clone(),
        };
        out.insert(key.clone(), value);
    }
    if let Some(Value::String(t)) = out.get("type")
        && t != "object"
    {
        out.remove("properties");
        out.remove("required");
    }
    if let (Some(Value::Array(required)), Some(Value::Object(props))) =
        (out.get("required"), out.get("properties"))
    {
        let kept: Vec<Value> = required
            .iter()
            .filter(|r| r.as_str().is_some_and(|r| props.contains_key(r)))
            .cloned()
            .collect();
        out.insert("required".into(), Value::Array(kept));
    }
    Value::Object(out)
}

fn encode_parts(items: &[Content]) -> Vec<Value> {
    items
        .iter()
        .filter_map(|c| match c {
            Content::Text(t) if t.is_empty() => None,
            Content::Text(t) => Some(json!({ "text": t })),
            Content::Image { mime, data } => Some(json!({
                "inline_data": {
                    "mime_type": mime,
                    "data": base64::engine::general_purpose::STANDARD.encode(data)
                }
            })),
        })
        .collect()
}

fn push_content(contents: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = contents.last_mut()
        && last["role"] == role
        && let Some(existing) = last["parts"].as_array_mut()
    {
        existing.extend(parts);
        return;
    }
    contents.push(json!({ "role": role, "parts": parts }));
}

fn encode_body(req: &Request, signatures: &HashMap<String, String>) -> Value {
    let mut contents = Vec::new();
    let mut system = Vec::new();
    let signed = needs_signature(&req.model);

    for message in &req.messages {
        match message {
            Message::System(text) => system.push(json!({ "text": text })),
            Message::User(items) => push_content(&mut contents, "user", encode_parts(items)),
            Message::Assistant {
                content,
                tool_calls,
            } => {
                let mut parts = encode_parts(content);
                for (index, call) in tool_calls.iter().enumerate() {
                    let mut part = json!({
                        "functionCall": { "name": call.name, "args": call.args }
                    });
                    match signatures.get(&call.id) {
                        Some(signature) => part["thoughtSignature"] = json!(signature),
                        None if signed && index == 0 => {
                            part["thoughtSignature"] = json!(SKIP_SIGNATURE)
                        }
                        None => {}
                    }
                    parts.push(part);
                }
                push_content(&mut contents, "model", parts);
            }
            Message::ToolResult {
                id,
                content,
                is_error,
            } => {
                let key = if *is_error { "error" } else { "result" };
                push_content(
                    &mut contents,
                    "user",
                    vec![json!({
                        "functionResponse": {
                            "name": name_from_id(id),
                            "response": { key: content }
                        }
                    })],
                );
            }
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
                    "parameters": gemini_schema(&t.schema)
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
    #[serde(default)]
    thought: bool,
    #[serde(default, rename = "thoughtSignature")]
    thought_signature: Option<String>,
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

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct UsageMetadata {
    #[serde(default, rename = "promptTokenCount")]
    prompt: u32,
    #[serde(default, rename = "candidatesTokenCount")]
    candidates: u32,
    #[serde(default, rename = "thoughtsTokenCount")]
    thoughts: u32,
    #[serde(default, rename = "cachedContentTokenCount")]
    cached: u32,
}

impl UsageMetadata {
    fn event(&self) -> Event {
        Event::Usage {
            input: self.prompt,
            output: self.candidates + self.thoughts,
            cached: self.cached,
        }
    }
}

pub const MALFORMED_CALL: &str =
    "Gemini produced a malformed tool call and stopped; ask again, or try another Gemini model";
pub const UNOFFERED_CALL: &str = "Gemini called a tool that was not offered and stopped; ask again";

enum Finish {
    Stop(StopReason),
    Failed(&'static str),
}

fn finish(reason: &str) -> Finish {
    match reason {
        "MAX_TOKENS" => Finish::Stop(StopReason::MaxTokens),
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" | "IMAGE_SAFETY" => {
            Finish::Stop(StopReason::Refusal)
        }
        "MALFORMED_FUNCTION_CALL" => Finish::Failed(MALFORMED_CALL),
        "UNEXPECTED_TOOL_CALL" => Finish::Failed(UNOFFERED_CALL),
        _ => Finish::Stop(StopReason::EndTurn),
    }
}

#[derive(Default)]
struct Decoder {
    stop: Option<StopReason>,
    usage: Option<UsageMetadata>,
    failed: Option<&'static str>,
    signatures: Vec<(String, String)>,
}

impl Decoder {
    fn feed(&mut self, response: &StreamResponse) -> Vec<Event> {
        let mut out = Vec::new();

        for candidate in &response.candidates {
            for part in &candidate.content.parts {
                if let Some(call) = &part.function_call {
                    let id = call_id(&call.name);
                    if let Some(signature) = &part.thought_signature {
                        self.signatures.push((id.clone(), signature.clone()));
                    }
                    out.push(Event::ToolCallStart {
                        id,
                        name: call.name.clone(),
                    });
                    let args = if call.args.is_null() { json!({}) } else { call.args.clone() };
                    out.push(Event::ToolCallDelta(args.to_string()));
                    out.push(Event::ToolCallEnd);
                    self.stop = Some(StopReason::ToolUse);
                    continue;
                }
                if part.thought {
                    continue;
                }
                if let Some(text) = &part.text
                    && !text.is_empty()
                {
                    out.push(Event::TextDelta(text.clone()));
                }
            }
            if let Some(reason) = &candidate.finish_reason
                && self.stop.is_none()
            {
                match finish(reason) {
                    Finish::Stop(stop) => self.stop = Some(stop),
                    Finish::Failed(message) => self.failed = Some(message),
                }
            }
        }

        if let Some(usage) = response.usage {
            self.usage = Some(usage);
        }

        out
    }

    fn finish(self) -> (Vec<Event>, Vec<(String, String)>) {
        let mut out = Vec::new();
        if let Some(usage) = self.usage {
            out.push(usage.event());
        }
        out.push(match self.failed {
            Some(message) => Event::Error(ProviderError::Other(message.to_string())),
            None => Event::Done(self.stop.unwrap_or(StopReason::EndTurn)),
        });
        (out, self.signatures)
    }
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

fn scrub(text: String, api_key: &str) -> String {
    if api_key.is_empty() {
        return text;
    }
    text.replace(api_key, "<redacted>")
}

#[async_trait]
impl Provider for GoogleProvider {
    async fn stream(&self, req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let body = {
            let signatures = self.signatures.lock().unwrap_or_else(|e| e.into_inner());
            encode_body(&req, &signatures)
        };
        let response = self
            .client
            .post(self.endpoint(&req.model))
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Network(scrub(e.to_string(), &self.api_key)))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(http_error(status, scrub(body, &self.api_key)));
        }

        let (tx, rx) = mpsc::channel(64);
        let signatures = self.signatures.clone();
        tokio::spawn(async move {
            let mut events = response.bytes_stream().eventsource();
            let mut decoder = Decoder::default();

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
                        for event in decoder.feed(&parsed) {
                            if tx.send(event).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }

            let (tail, signed) = decoder.finish();
            if !signed.is_empty() {
                signatures
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend(signed);
            }
            for event in tail {
                let _ = tx.send(event).await;
            }
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
    use zarn_core::types::{ToolCall, ToolSpec};

    fn feed(chunks: &[&str]) -> (Vec<Event>, Vec<(String, String)>) {
        let mut decoder = Decoder::default();
        let mut events = Vec::new();
        for chunk in chunks {
            let parsed: StreamResponse = serde_json::from_str(chunk).unwrap();
            events.extend(decoder.feed(&parsed));
        }
        let (tail, signatures) = decoder.finish();
        events.extend(tail);
        (events, signatures)
    }

    fn request(messages: Vec<Message>, model: &str) -> Request {
        Request {
            messages,
            tools: Vec::new(),
            model: model.into(),
            max_tokens: 10,
            temperature: 1.0,
        }
    }

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            args: json!({}),
        }
    }

    fn result(id: &str, content: &str) -> Message {
        Message::ToolResult {
            id: id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    #[test]
    fn text_parts_become_deltas() {
        let (events, _) = feed(&[
            r#"{"candidates":[{"content":{"parts":[{"text":"hi"}]}}]}"#,
            r#"{"candidates":[{"content":{"parts":[]},"finishReason":"STOP"}]}"#,
        ]);
        assert_eq!(events[0], Event::TextDelta("hi".into()));
        assert_eq!(events.last(), Some(&Event::Done(StopReason::EndTurn)));
    }

    #[test]
    fn thought_parts_are_not_shown_as_the_answer() {
        let (events, _) = feed(&[
            r#"{"candidates":[{"content":{"parts":[{"text":"let me think","thought":true},{"text":"answer"}]}}]}"#,
        ]);
        assert_eq!(events[0], Event::TextDelta("answer".into()));
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn two_calls_to_the_same_tool_get_distinct_ids_that_map_back_to_the_name() {
        let (events, _) = feed(&[
            r#"{"candidates":[{"content":{"parts":[
                {"functionCall":{"name":"read_file","args":{"path":"a"}}},
                {"functionCall":{"name":"read_file","args":{"path":"b"}}},
                {"functionCall":{"name":"glob"}}
            ]}}]}"#,
        ]);
        let ids: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                Event::ToolCallStart { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ids.len(), 3);
        assert_ne!(ids[0], ids[1]);
        assert_eq!(name_from_id(&ids[0]), "read_file");
        assert_eq!(name_from_id(&ids[2]), "glob");
        assert_eq!(name_from_id("call_read"), "read");
        assert_eq!(name_from_id("call_mcp__fs__read-z"), "mcp__fs__read-z");
        assert!(ids.iter().all(|id| id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')));
        assert!(events.contains(&Event::ToolCallDelta("{}".into())));
        assert_eq!(events.last(), Some(&Event::Done(StopReason::ToolUse)));
    }

    #[test]
    fn usage_is_reported_once_from_the_last_chunk_not_summed_per_chunk() {
        let (events, _) = feed(&[
            r#"{"candidates":[{"content":{"parts":[{"text":"a"}]}}],"usageMetadata":{"promptTokenCount":1000}}"#,
            r#"{"candidates":[{"content":{"parts":[{"text":"b"}]}}],"usageMetadata":{"promptTokenCount":1000,"candidatesTokenCount":5}}"#,
            r#"{"candidates":[{"content":{"parts":[]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":1000,"candidatesTokenCount":9,"thoughtsTokenCount":20,"cachedContentTokenCount":400}}"#,
        ]);
        let usage: Vec<&Event> = events.iter().filter(|e| matches!(e, Event::Usage { .. })).collect();
        assert_eq!(
            usage,
            vec![&Event::Usage {
                input: 1000,
                output: 29,
                cached: 400
            }]
        );
    }

    #[test]
    fn a_safety_stop_is_a_refusal() {
        let (events, _) = feed(&[
            r#"{"candidates":[{"content":{"parts":[]},"finishReason":"SAFETY"}]}"#,
        ]);
        assert_eq!(events.last(), Some(&Event::Done(StopReason::Refusal)));
    }

    #[test]
    fn a_malformed_call_is_an_error_not_a_silent_empty_reply() {
        let (events, _) = feed(&[
            r#"{"candidates":[{"content":{"parts":[]},"finishReason":"MALFORMED_FUNCTION_CALL"}]}"#,
        ]);
        assert!(matches!(events.last(), Some(Event::Error(ProviderError::Other(m))) if m.contains("malformed")));
    }

    #[test]
    fn parallel_tool_results_go_back_in_one_turn_under_their_names() {
        let req = request(
            vec![
                Message::user("go"),
                Message::Assistant {
                    content: Vec::new(),
                    tool_calls: vec![call("call_read-z1x1", "read"), call("call_read-z1x2", "read")],
                },
                result("call_read-z1x1", "one"),
                result("call_read-z1x2", "two"),
            ],
            "gemini-2.5-pro",
        );
        let body = encode_body(&req, &HashMap::new());
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[1]["parts"].as_array().unwrap().len(), 2);
        let responses = contents[2]["parts"].as_array().unwrap();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["functionResponse"]["name"], "read");
        assert_eq!(responses[1]["functionResponse"]["response"]["result"], "two");
        assert!(contents[1]["parts"][0].get("thoughtSignature").is_none());
    }

    #[test]
    fn thought_signatures_are_sent_back_and_gemini_3_gets_a_placeholder_when_none_is_known() {
        let (events, signatures) = feed(&[
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{}},"thoughtSignature":"c2ln"}]}}]}"#,
        ]);
        let id = match &events[0] {
            Event::ToolCallStart { id, .. } => id.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(signatures, vec![(id.clone(), "c2ln".to_string())]);
        let known: HashMap<String, String> = signatures.into_iter().collect();
        let messages = vec![
            Message::user("go"),
            Message::Assistant {
                content: Vec::new(),
                tool_calls: vec![call(&id, "read")],
            },
            result(&id, "x"),
            Message::Assistant {
                content: Vec::new(),
                tool_calls: vec![call("call_glob", "glob")],
            },
        ];
        let body = encode_body(&request(messages.clone(), "gemini-3-pro-preview"), &known);
        assert_eq!(body["contents"][1]["parts"][0]["thoughtSignature"], "c2ln");
        assert_eq!(body["contents"][3]["parts"][0]["thoughtSignature"], SKIP_SIGNATURE);
        let old = encode_body(&request(messages, "gemini-2.5-flash"), &HashMap::new());
        assert!(old["contents"][3]["parts"][0].get("thoughtSignature").is_none());
    }

    #[test]
    fn an_error_result_is_reported_as_an_error_and_empty_text_is_dropped() {
        let req = request(
            vec![
                Message::user("go"),
                Message::Assistant {
                    content: vec![Content::Text(String::new())],
                    tool_calls: vec![call("call_bash", "bash")],
                },
                Message::ToolResult {
                    id: "call_bash".into(),
                    content: "denied".into(),
                    is_error: true,
                },
            ],
            "gemini-2.5-pro",
        );
        let body = encode_body(&req, &HashMap::new());
        assert_eq!(body["contents"][1]["parts"].as_array().unwrap().len(), 1);
        assert_eq!(body["contents"][2]["parts"][0]["functionResponse"]["response"]["error"], "denied");
    }

    #[test]
    fn tool_schemas_drop_keywords_gemini_rejects() {
        let mut req = request(vec![Message::user("go")], "gemini-2.5-pro");
        req.tools = vec![ToolSpec {
            name: "t".into(),
            description: "d".into(),
            schema: json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "path": { "type": ["string", "null"], "default": "x" },
                    "n": { "type": "integer", "exclusiveMinimum": 0, "enum": [1, 2] },
                    "list": { "type": "array", "items": { "type": "object", "additionalProperties": {}, "properties": {} } }
                },
                "required": ["path", "gone"]
            }),
        }];
        let body = encode_body(&req, &HashMap::new());
        let schema = &body["tools"][0]["functionDeclarations"][0]["parameters"];
        assert_eq!(
            schema,
            &json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "nullable": true },
                    "n": { "type": "integer", "enum": ["1", "2"] },
                    "list": { "type": "array", "items": { "type": "object", "properties": {} } }
                },
                "required": ["path"]
            })
        );
    }

    #[test]
    fn a_model_name_with_the_models_prefix_is_not_doubled() {
        let provider = GoogleProvider::new("test-key-not-real").with_base_url("http://h/v1beta");
        assert_eq!(
            provider.endpoint("models/gemini-2.5-pro"),
            "http://h/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn the_api_key_never_survives_into_an_error_string() {
        let leaked = "https://example/v1?key=SECRET123 failed".to_string();
        assert!(!scrub(leaked, "SECRET123").contains("SECRET123"));
    }
}
