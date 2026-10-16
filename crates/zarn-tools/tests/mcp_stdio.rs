use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::io::Write;
use tokio::time::{timeout, Duration};
use zarn_core::tools::Ctx;
use zarn_tools::mcp::{McpClient, ServerConfig, connect_all};
use serde_json::json;
use tempfile::TempDir;

fn find_sh() -> Option<String> {
    if cfg!(windows)
        && let Ok(output) = std::process::Command::new("bash")
            .arg("--version")
            .output()
        && output.status.success()
    {
        return Some("bash".to_string());
    }
    if fs::metadata("/bin/sh").is_ok() {
        return Some("/bin/sh".to_string());
    }
    None
}

fn create_basic_server(tmpdir: &TempDir) -> PathBuf {
    let script = r#"#!/bin/bash
while read line; do
    if echo "$line" | grep -q '"initialize"'; then
        id=$(echo "$line" | grep -o '"id":[0-9]*' | cut -d: -f2)
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{}}}"
    elif echo "$line" | grep -q '"tools/list"'; then
        id=$(echo "$line" | grep -o '"id":[0-9]*' | cut -d: -f2)
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"echo\",\"description\":\"Echo tool\",\"inputSchema\":{\"type\":\"object\",\"properties\":{\"text\":{\"type\":\"string\"}}}},{\"name\":\"upper\",\"description\":\"Uppercase tool\",\"inputSchema\":{\"type\":\"object\"}}]}}"
    elif echo "$line" | grep -q '"tools/call"'; then
        id=$(echo "$line" | grep -o '"id":[0-9]*' | cut -d: -f2)
        name=$(echo "$line" | grep -o '"name":"[^"]*"' | head -1 | cut -d'"' -f4)
        if [ "$name" = "echo" ]; then
            echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"response one\"},{\"type\":\"text\",\"text\":\"response two\"}]}}"
        else
            echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"upper result\"}]}}"
        fi
    fi
done
"#;
    let path = tmpdir.path().join("server.sh");
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(script.as_bytes()).unwrap();
    drop(f);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    path
}

fn create_error_response_server(tmpdir: &TempDir) -> PathBuf {
    let script = r#"#!/bin/bash
while read line; do
    if echo "$line" | grep -q '"initialize"'; then
        id=$(echo "$line" | grep -o '"id":[0-9]*' | cut -d: -f2)
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{}}}"
    elif echo "$line" | grep -q '"tools/list"'; then
        id=$(echo "$line" | grep -o '"id":[0-9]*' | cut -d: -f2)
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"error\":{\"code\":-32600,\"message\":\"Invalid request\"}}"
    fi
done
"#;
    let path = tmpdir.path().join("error_server.sh");
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(script.as_bytes()).unwrap();
    drop(f);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    path
}

fn create_is_error_server(tmpdir: &TempDir) -> PathBuf {
    let script = r#"#!/bin/bash
while read line; do
    if echo "$line" | grep -q '"initialize"'; then
        id=$(echo "$line" | grep -o '"id":[0-9]*' | cut -d: -f2)
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{}}}"
    elif echo "$line" | grep -q '"tools/list"'; then
        id=$(echo "$line" | grep -o '"id":[0-9]*' | cut -d: -f2)
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"fail\",\"description\":\"Fails\",\"inputSchema\":{\"type\":\"object\"}}]}}"
    elif echo "$line" | grep -q '"tools/call"'; then
        id=$(echo "$line" | grep -o '"id":[0-9]*' | cut -d: -f2)
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"isError\":true,\"content\":[{\"type\":\"text\",\"text\":\"error message\"}]}}"
    fi
done
"#;
    let path = tmpdir.path().join("is_error_server.sh");
    let mut f = fs::File::create(&path).unwrap();
    f.write_all(script.as_bytes()).unwrap();
    drop(f);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    path
}

#[tokio::test]
async fn test_connect_completes_handshake() {
    let Some(sh) = find_sh() else {
        eprintln!("No shell found, skipping");
        return;
    };

    let tmpdir = TempDir::new().unwrap();
    let script = create_basic_server(&tmpdir);

    let config = ServerConfig {
        command: Some(sh),
        args: vec![script.to_string_lossy().to_string()],
        ..ServerConfig::default()
    };

    let result = timeout(
        Duration::from_secs(5),
        McpClient::connect("test", &config),
    )
    .await;

    assert!(result.is_ok(), "should not timeout");
    let client = result.unwrap();
    assert!(client.is_ok(), "connect should succeed");
    let client = client.unwrap();
    client.shutdown().await;
}

#[tokio::test]
async fn test_list_tools_returns_correct_mapping() {
    let Some(sh) = find_sh() else {
        eprintln!("No shell found, skipping");
        return;
    };

    let tmpdir = TempDir::new().unwrap();
    let script = create_basic_server(&tmpdir);

    let config = ServerConfig {
        command: Some(sh),
        args: vec![script.to_string_lossy().to_string()],
        ..ServerConfig::default()
    };

    let client = timeout(
        Duration::from_secs(5),
        McpClient::connect("test", &config),
    )
    .await
    .unwrap()
    .unwrap();

    let tools = client.list_tools().await.unwrap();

    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[0].description, "Echo tool");
    assert!(tools[0]
        .schema
        .get("properties")
        .and_then(|p| p.get("text"))
        .is_some());
    assert_eq!(tools[1].name, "upper");
    assert_eq!(tools[1].description, "Uppercase tool");

    client.shutdown().await;
}

#[tokio::test]
async fn test_connect_all_namespaces_tools() {
    let Some(sh) = find_sh() else {
        eprintln!("No shell found, skipping");
        return;
    };

    let tmpdir = TempDir::new().unwrap();
    let script = create_basic_server(&tmpdir);

    let config = ServerConfig {
        command: Some(sh),
        args: vec![script.to_string_lossy().to_string()],
        ..ServerConfig::default()
    };

    let servers = HashMap::from([("myserver".to_string(), config)]);

    let tools = timeout(Duration::from_secs(5), connect_all(&servers))
        .await
        .unwrap();

    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].spec().name, "myserver__echo");
    assert_eq!(tools[1].spec().name, "myserver__upper");
}

#[tokio::test]
async fn test_tool_call_joins_text_content() {
    let Some(sh) = find_sh() else {
        eprintln!("No shell found, skipping");
        return;
    };

    let tmpdir = TempDir::new().unwrap();
    let script = create_basic_server(&tmpdir);

    let config = ServerConfig {
        command: Some(sh),
        args: vec![script.to_string_lossy().to_string()],
        ..ServerConfig::default()
    };

    let servers = HashMap::from([("srv".to_string(), config)]);

    let tools = timeout(Duration::from_secs(5), connect_all(&servers))
        .await
        .unwrap();

    assert!(!tools.is_empty());

    let ctx = Ctx::new(".");
    let output = tools[0].run(json!({"text": "hello"}), &ctx).await.unwrap();

    assert_eq!(output.content, "response one\nresponse two");
    assert!(!output.is_error);
}

#[tokio::test]
async fn test_is_error_flag() {
    let Some(sh) = find_sh() else {
        eprintln!("No shell found, skipping");
        return;
    };

    let tmpdir = TempDir::new().unwrap();
    let script = create_is_error_server(&tmpdir);

    let config = ServerConfig {
        command: Some(sh),
        args: vec![script.to_string_lossy().to_string()],
        ..ServerConfig::default()
    };

    let servers = HashMap::from([("srv".to_string(), config)]);

    let tools = timeout(Duration::from_secs(5), connect_all(&servers))
        .await
        .unwrap();

    assert!(!tools.is_empty());

    let ctx = Ctx::new(".");
    let output = tools[0].run(json!({}), &ctx).await.unwrap();

    assert_eq!(output.content, "error message");
    assert!(output.is_error);
}

#[tokio::test]
async fn test_json_rpc_error_response_surfaces_as_error() {
    let Some(sh) = find_sh() else {
        eprintln!("No shell found, skipping");
        return;
    };

    let tmpdir = TempDir::new().unwrap();
    let script = create_error_response_server(&tmpdir);

    let config = ServerConfig {
        command: Some(sh),
        args: vec![script.to_string_lossy().to_string()],
        ..ServerConfig::default()
    };

    let client = timeout(
        Duration::from_secs(5),
        McpClient::connect("test", &config),
    )
    .await
    .unwrap()
    .unwrap();

    let result = client.list_tools().await;

    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Invalid request"));
}

#[tokio::test]
async fn test_server_that_exits_immediately_is_skipped() {
    let Some(sh) = find_sh() else {
        eprintln!("No shell found, skipping");
        return;
    };

    let config = ServerConfig {
        command: Some(sh),
        args: vec!["-c".to_string(), "exit 0".to_string()],
        ..ServerConfig::default()
    };

    let result = timeout(
        Duration::from_secs(5),
        McpClient::connect("test", &config),
    )
    .await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_err());
}

#[tokio::test]
async fn test_server_with_no_command_and_no_url_is_skipped() {
    let config = ServerConfig::default();

    let result = timeout(
        Duration::from_secs(5),
        McpClient::connect("empty", &config),
    )
    .await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_err());
}

#[tokio::test]
async fn test_connect_all_skips_unavailable_server_without_panic() {
    let servers = HashMap::from([(
        "unavailable".to_string(),
        ServerConfig {
            command: Some("nonexistent-binary-xyz-123".to_string()),
            ..ServerConfig::default()
        },
    )]);

    let result = timeout(Duration::from_secs(5), connect_all(&servers)).await;

    assert!(result.is_ok());
    let tools = result.unwrap();
    assert!(tools.is_empty());
}

#[tokio::test]
async fn test_connect_all_skips_servers_that_fail_list_tools() {
    let Some(sh) = find_sh() else {
        eprintln!("No shell found, skipping");
        return;
    };

    let tmpdir = TempDir::new().unwrap();
    let script = create_error_response_server(&tmpdir);

    let config = ServerConfig {
        command: Some(sh),
        args: vec![script.to_string_lossy().to_string()],
        ..ServerConfig::default()
    };

    let servers = HashMap::from([("broken".to_string(), config)]);

    let result = timeout(Duration::from_secs(5), connect_all(&servers)).await;

    assert!(result.is_ok());
    let tools = result.unwrap();
    assert!(tools.is_empty());
}

#[tokio::test]
async fn test_multiple_servers_mixed_success_and_failure() {
    let Some(sh) = find_sh() else {
        eprintln!("No shell found, skipping");
        return;
    };

    let tmpdir = TempDir::new().unwrap();
    let good_script = create_basic_server(&tmpdir);
    let bad_script = create_error_response_server(&tmpdir);

    let servers = HashMap::from([
        (
            "good".to_string(),
            ServerConfig {
                command: Some(sh.clone()),
                args: vec![good_script.to_string_lossy().to_string()],
                ..ServerConfig::default()
            },
        ),
        (
            "bad".to_string(),
            ServerConfig {
                command: Some(sh),
                args: vec![bad_script.to_string_lossy().to_string()],
                ..ServerConfig::default()
            },
        ),
    ]);

    let tools = timeout(Duration::from_secs(5), connect_all(&servers))
        .await
        .unwrap();

    assert_eq!(tools.len(), 2);
    for tool in tools {
        assert!(tool.spec().name.starts_with("good__"));
    }
}
