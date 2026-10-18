#![cfg(unix)]

use serde_json::json;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zarn_core::tools::Ctx;
use zarn_tools::mcp::{McpServer, ServerConfig};

pub fn rich_config() -> ServerConfig {
    ServerConfig {
        command: Some("sh".into()),
        args: vec![format!("{}/tests/fixtures/mcp_rich.sh", env!("CARGO_MANIFEST_DIR"))],
        ..ServerConfig::default()
    }
}

#[tokio::test]
async fn resources_and_prompts_are_listed_read_and_rendered() {
    let server = tokio::time::timeout(Duration::from_secs(5), McpServer::start("docs", &rich_config()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(server.resources().len(), 1);
    assert_eq!(server.resources()[0].uri, "docs://readme");
    assert_eq!(server.prompts()[0].name, "summarize");
    assert!(server.prompts()[0].arguments[0].required);
    assert_eq!(server.client().read_resource("docs://readme").await.unwrap(), "# Readme from MCP");
    assert!(server.client().read_resource("docs://missing").await.is_err());
    let args = BTreeMap::from([("topic".to_string(), "rust".to_string()), ("style".to_string(), "terse".to_string())]);
    assert_eq!(server.client().get_prompt("summarize", &args).await.unwrap(), "Summarize rust in a terse style.");
    server.shutdown().await;
}

#[tokio::test]
async fn list_changed_notifications_reach_the_listener_and_refresh_the_tools() {
    let server = McpServer::start("docs", &rich_config()).await.unwrap();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = seen.clone();
    server.on_notification(Arc::new(move |method| log.lock().unwrap().push(method.to_string())));
    assert_eq!(server.tool_names(), vec!["docs__grow"]);
    let tool = server.tools().remove(0);
    let out = tool.run(json!({}), &Ctx::new(".")).await.unwrap();
    assert_eq!(out.content, "grown");
    for _ in 0..50 {
        if !seen.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(seen.lock().unwrap().as_slice(), ["notifications/tools/list_changed"]);
    assert_eq!(server.refresh_tools().await.unwrap(), 2);
    assert_eq!(server.tool_names(), vec!["docs__grow", "docs__fresh"]);
    server.shutdown().await;
}

#[tokio::test]
async fn servers_without_resources_or_prompts_start_quickly() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("plain.sh");
    std::fs::write(
        &script,
        "while IFS= read -r line; do id=$(printf '%s' \"$line\" | grep -o '\"id\":[0-9]*' | cut -d: -f2); case \"$line\" in *initialize*) printf '{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{\"capabilities\":{}}}\\n' \"$id\";; *tools/list*) printf '{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{\"tools\":[]}}\\n' \"$id\";; esac; done\n",
    )
    .unwrap();
    let config = ServerConfig { command: Some("sh".into()), args: vec![script.display().to_string()], ..ServerConfig::default() };
    let started = std::time::Instant::now();
    let server = McpServer::start("plain", &config).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(server.resources().is_empty() && server.prompts().is_empty());
    server.shutdown().await;
}
