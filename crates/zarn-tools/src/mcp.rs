use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex, oneshot};
use tokio::time::{Duration, timeout};
use zarn_core::tools::{Ctx, Risk, Tool, ToolOutput};
use zarn_core::types::ToolSpec;

const CALL_TIMEOUT: Duration = Duration::from_secs(120);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ServerConfig {
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

enum Transport {
    Stdio {
        stdin: Mutex<ChildStdin>,
        pending: Pending,
        child: Box<Mutex<Child>>,
    },
    Http {
        client: reqwest::Client,
        url: String,
        headers: HashMap<String, String>,
    },
}

pub struct McpClient {
    name: String,
    transport: Transport,
    next_id: AtomicU64,
}

impl McpClient {
    pub async fn connect(name: &str, config: &ServerConfig) -> Result<Arc<Self>> {
        let transport = match (&config.command, &config.url) {
            (Some(command), _) => Self::spawn_stdio(command, config).await?,
            (None, Some(url)) => Transport::Http {
                client: reqwest::Client::new(),
                url: url.clone(),
                headers: config.headers.clone(),
            },
            (None, None) => bail!("server {name} has neither a command nor a url"),
        };

        let client = Arc::new(McpClient {
            name: name.to_string(),
            transport,
            next_id: AtomicU64::new(1),
        });

        timeout(STARTUP_TIMEOUT, client.initialize())
            .await
            .map_err(|_| anyhow!("server {name} did not finish initializing in time"))??;

        Ok(client)
    }

    async fn spawn_stdio(command: &str, config: &ServerConfig) -> Result<Transport> {
        let mut child = tokio::process::Command::new(command)
            .args(&config.args)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));

        let reader_pending = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let Some(id) = value.get("id").and_then(Value::as_u64) else {
                    continue;
                };
                if let Some(sender) = reader_pending.lock().await.remove(&id) {
                    let _ = sender.send(split_response(value));
                }
            }
            for (_, sender) in reader_pending.lock().await.drain() {
                let _ = sender.send(Err("server closed the connection".into()));
            }
        });

        Ok(Transport::Stdio {
            stdin: Mutex::new(stdin),
            pending,
            child: Box::new(Mutex::new(child)),
        })
    }

    async fn initialize(&self) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "zarn", "version": env!("CARGO_PKG_VERSION") }
            }),
        )
        .await?;
        self.notify("notifications/initialized", json!({})).await
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let payload = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        match &self.transport {
            Transport::Stdio { stdin, .. } => {
                let mut stdin = stdin.lock().await;
                stdin
                    .write_all(format!("{payload}\n").as_bytes())
                    .await
                    .map_err(Into::into)
            }
            Transport::Http {
                client,
                url,
                headers,
            } => {
                let mut request = client.post(url).json(&payload);
                for (key, value) in headers {
                    request = request.header(key, value);
                }
                request.send().await?;
                Ok(())
            }
        }
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let payload = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });

        let result = match &self.transport {
            Transport::Stdio { stdin, pending, .. } => {
                let (tx, rx) = oneshot::channel();
                pending.lock().await.insert(id, tx);
                {
                    let mut stdin = stdin.lock().await;
                    stdin.write_all(format!("{payload}\n").as_bytes()).await?;
                    stdin.flush().await?;
                }
                match timeout(CALL_TIMEOUT, rx).await {
                    Err(_) => {
                        pending.lock().await.remove(&id);
                        bail!("server {} timed out on {method}", self.name);
                    }
                    Ok(Err(_)) => bail!("server {} dropped the request", self.name),
                    Ok(Ok(result)) => result,
                }
            }
            Transport::Http {
                client,
                url,
                headers,
            } => {
                let mut request = client
                    .post(url)
                    .header("accept", "application/json, text/event-stream")
                    .json(&payload);
                for (key, value) in headers {
                    request = request.header(key, value);
                }
                let response = timeout(CALL_TIMEOUT, request.send())
                    .await
                    .map_err(|_| anyhow!("server {} timed out on {method}", self.name))??;

                let is_sse = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.contains("text/event-stream"));

                if is_sse {
                    read_sse(response, id).await?
                } else {
                    split_response(response.json().await?)
                }
            }
        };

        result.map_err(|e| anyhow!("{}: {e}", self.name))
    }

    pub async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        let result = self.request("tools/list", json!({})).await?;
        let listed = result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        Ok(listed
            .into_iter()
            .filter_map(|tool| {
                Some(ToolSpec {
                    name: tool.get("name")?.as_str()?.to_string(),
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    schema: tool
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object" })),
                })
            })
            .collect())
    }

    pub async fn shutdown(&self) {
        if let Transport::Stdio { child, .. } = &self.transport {
            let _ = child.lock().await.start_kill();
        }
    }
}

async fn read_sse(response: reqwest::Response, id: u64) -> Result<Result<Value, String>> {
    let mut stream = response.bytes_stream().eventsource();
    while let Some(Ok(event)) = stream.next().await {
        let Ok(value) = serde_json::from_str::<Value>(&event.data) else {
            continue;
        };
        if value.get("id").and_then(Value::as_u64) == Some(id) {
            return Ok(split_response(value));
        }
    }
    Ok(Err("the stream ended before a response arrived".into()))
}

fn split_response(value: Value) -> Result<Value, String> {
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(message.to_string());
    }
    Ok(value.get("result").cloned().unwrap_or(Value::Null))
}

pub fn namespaced(server: &str, tool: &str) -> String {
    format!("{server}__{tool}")
}

pub struct McpTool {
    client: Arc<McpClient>,
    remote_name: String,
    spec: ToolSpec,
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn risk(&self) -> Risk {
        Risk::Execute
    }

    async fn run(&self, args: Value, _ctx: &Ctx) -> Result<ToolOutput> {
        let result = self
            .client
            .request(
                "tools/call",
                json!({ "name": self.remote_name, "arguments": args }),
            )
            .await?;
        Ok(render_result(&result))
    }
}

fn render_result(result: &Value) -> ToolOutput {
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let content = match result.get("content").and_then(Value::as_array) {
        None => result.to_string(),
        Some(parts) => parts
            .iter()
            .map(|part| match part.get("type").and_then(Value::as_str) {
                Some("text") => part
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                Some(kind) => format!("[{kind} content]"),
                None => part.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    };

    ToolOutput {
        content,
        is_error,
    }
}

pub async fn connect_all(servers: &HashMap<String, ServerConfig>) -> Vec<Box<dyn Tool>> {
    let mut tools: Vec<Box<dyn Tool>> = Vec::new();

    for (name, config) in servers {
        let client = match McpClient::connect(name, config).await {
            Ok(client) => client,
            Err(err) => {
                tracing::warn!(server = %name, %err, "mcp server unavailable, skipping");
                continue;
            }
        };

        let listed = match client.list_tools().await {
            Ok(listed) => listed,
            Err(err) => {
                tracing::warn!(server = %name, %err, "mcp server listed no tools, skipping");
                client.shutdown().await;
                continue;
            }
        };

        for spec in listed {
            let remote_name = spec.name.clone();
            tools.push(Box::new(McpTool {
                client: client.clone(),
                remote_name: remote_name.clone(),
                spec: ToolSpec {
                    name: namespaced(name, &remote_name),
                    ..spec
                },
            }));
        }
    }

    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_namespaced_by_server() {
        assert_eq!(namespaced("github", "search"), "github__search");
    }

    #[test]
    fn an_error_response_does_not_look_like_a_result() {
        let value = json!({ "id": 1, "error": { "code": -1, "message": "nope" } });
        assert_eq!(split_response(value), Err("nope".to_string()));
    }

    #[test]
    fn a_missing_result_is_null_not_a_failure() {
        assert_eq!(split_response(json!({ "id": 1 })), Ok(Value::Null));
    }

    #[test]
    fn text_content_blocks_are_joined() {
        let out = render_result(&json!({
            "content": [
                { "type": "text", "text": "one" },
                { "type": "text", "text": "two" }
            ]
        }));
        assert_eq!(out.content, "one\ntwo");
        assert!(!out.is_error);
    }

    #[test]
    fn non_text_content_is_labelled_rather_than_dropped() {
        let out = render_result(&json!({
            "content": [{ "type": "image", "data": "..." }]
        }));
        assert_eq!(out.content, "[image content]");
    }

    #[test]
    fn the_is_error_flag_is_carried_through() {
        let out = render_result(&json!({
            "isError": true,
            "content": [{ "type": "text", "text": "boom" }]
        }));
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn a_server_that_cannot_start_is_skipped_rather_than_fatal() {
        let servers = HashMap::from([(
            "broken".to_string(),
            ServerConfig {
                command: Some("zarn-no-such-binary".into()),
                ..ServerConfig::default()
            },
        )]);
        assert!(connect_all(&servers).await.is_empty());
    }

    #[tokio::test]
    async fn a_server_with_no_command_and_no_url_is_rejected() {
        let result = McpClient::connect("empty", &ServerConfig::default()).await;
        assert!(result.is_err());
    }
}
