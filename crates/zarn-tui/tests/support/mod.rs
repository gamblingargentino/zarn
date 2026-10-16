#![allow(dead_code)]

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, MutexGuard};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};
use zarn_config::auth::{KeyStore, MemoryBackend};
use zarn_tui::state::Block;
use zarn_tui::{Controller, Options};

static LOCK: Mutex<()> = Mutex::const_new(());

pub fn sse(text: &str) -> String {
    let content = serde_json::to_string(text).unwrap();
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{content}}}}}]}}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":10,\"completion_tokens\":2}}}}\n\ndata: [DONE]\n\n"
    )
}

pub fn tool_sse(id: &str, name: &str, args: &Value) -> String {
    let args = serde_json::to_string(&args.to_string()).unwrap();
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"{id}\",\"function\":{{\"name\":\"{name}\",\"arguments\":{args}}}}}]}}}}]}}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}],\"usage\":{{\"prompt_tokens\":5,\"completion_tokens\":8}}}}\n\ndata: [DONE]\n\n"
    )
}

pub struct Script<F>(pub F);

impl<F> Respond for Script<F>
where
    F: Fn(&Value) -> (String, u64) + Send + Sync,
{
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let (text, delay) = (self.0)(&body);
        ResponseTemplate::new(200)
            .set_body_string(text)
            .insert_header("content-type", "text/event-stream")
            .set_delay(Duration::from_millis(delay))
    }
}

pub async fn server<F>(script: F) -> MockServer
where
    F: Fn(&Value) -> (String, u64) + Send + Sync + 'static,
{
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(Script(script))
        .mount(&server)
        .await;
    server
}

pub async fn echo_server(text: &'static str) -> MockServer {
    server(move |_| (sse(text), 0)).await
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

pub fn system_text(body: &Value) -> String {
    body["messages"]
        .as_array()
        .and_then(|m| m.first())
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default()
        .to_string()
}

pub struct Env {
    pub dir: tempfile::TempDir,
    pub store: Arc<KeyStore>,
    _guard: MutexGuard<'static, ()>,
}

impl Env {
    pub async fn new(user_config: &str) -> Self {
        let guard = LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("project/.zarn")).unwrap();
        std::fs::create_dir_all(root.join("cfg")).unwrap();
        std::fs::write(root.join("cfg/config.toml"), user_config).unwrap();
        std::env::set_current_dir(root.join("project")).unwrap();
        unsafe {
            std::env::set_var("ZARN_CONFIG_DIR", root.join("cfg"));
            std::env::set_var("ZARN_DATA_DIR", root.join("data"));
            std::env::remove_var("ZARN_PROFILE");
            std::env::remove_var("ZARN_MODEL");
        }
        let store = Arc::new(KeyStore::new(
            Arc::new(MemoryBackend::new()),
            root.join("cfg/auth.json"),
        ));
        Env {
            dir,
            store,
            _guard: guard,
        }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn project(&self) -> PathBuf {
        self.root().join("project")
    }

    pub fn user_dir(&self) -> PathBuf {
        self.root().join("cfg")
    }

    pub fn controller(&self) -> Controller {
        self.controller_with(Options::default())
    }

    pub fn controller_with(&self, options: Options) -> Controller {
        Controller::new(Options {
            store: Some(self.store.clone()),
            ..options
        })
        .unwrap()
    }
}

pub fn provider_config(base: &str, extra: &str) -> String {
    format!(
        "[providers.mock]\nkind = \"openai-compat\"\nbase_url = \"{base}\"\n{extra}\n[profiles.default]\nprovider = \"mock\"\nmodel = \"m\"\n"
    )
}

pub fn notices(c: &Controller) -> Vec<String> {
    c.app()
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Notice(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

pub fn assistant(c: &Controller) -> Vec<String> {
    c.app()
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Assistant(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

pub async fn turn(c: &mut Controller, text: &str) {
    c.type_line(text);
    c.settle(Duration::from_secs(10)).await;
}
