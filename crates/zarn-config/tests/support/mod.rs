#![allow(dead_code)]

use serde_json::Value;
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zarn_config::Config;
use zarn_config::auth::{KeyStore, MemoryBackend};

pub fn reply_body(text: &str) -> String {
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":10,\"completion_tokens\":2}}}}\n\ndata: [DONE]\n\n"
    )
}

pub async fn openai_server(text: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(reply_body(text))
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;
    server
}

pub async fn failing_server(status: u16) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(status).set_body_string("{\"error\":{\"message\":\"nope\"}}"))
        .mount(&server)
        .await;
    server
}

pub async fn bodies(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|r| serde_json::from_slice(&r.body).ok())
        .collect()
}

pub async fn models(server: &MockServer) -> Vec<String> {
    bodies(server)
        .await
        .iter()
        .filter_map(|b| b.get("model").and_then(Value::as_str).map(str::to_string))
        .collect()
}

pub fn store() -> (tempfile::TempDir, Arc<KeyStore>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(KeyStore::new(
        Arc::new(MemoryBackend::new()),
        dir.path().join("auth.json"),
    ));
    (dir, store)
}

pub fn config(body: &str) -> Config {
    let cfg: Config = toml::from_str(body).unwrap();
    cfg.validate().unwrap();
    cfg
}
