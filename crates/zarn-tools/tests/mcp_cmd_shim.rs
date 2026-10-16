#![cfg(windows)]

use serde_json::json;
use std::collections::HashMap;
use std::fs;
use tempfile::TempDir;
use zarn_core::tools::Ctx;
use zarn_tools::mcp::{McpClient, ServerConfig};

const SERVER: &str = r#"#!/bin/bash
while read line; do
    id=$(echo "$line" | grep -o '"id":[0-9]*' | cut -d: -f2)
    if echo "$line" | grep -q '"initialize"'; then
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{}}}"
    elif echo "$line" | grep -q '"tools/list"'; then
        echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"read_text_file\",\"description\":\"Read a file\",\"inputSchema\":{\"type\":\"object\"}}]}}"
    fi
done
"#;

fn bash_available() -> bool {
    std::process::Command::new("bash")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn install_shim(dir: &TempDir, stem: &str) -> String {
    let script = dir.path().join("server.sh");
    fs::write(&script, SERVER).unwrap();
    let shim = dir.path().join(format!("{stem}.cmd"));
    fs::write(
        &shim,
        format!(
            "@bash \"{}\" %*\r\n",
            script.display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();
    dir.path().join(stem).display().to_string()
}

#[tokio::test]
async fn a_command_that_exists_only_as_a_cmd_shim_still_starts() {
    if !bash_available() {
        return;
    }

    let dir = TempDir::new().unwrap();
    let extensionless = install_shim(&dir, "zarn_fake_mcp");

    assert!(
        std::process::Command::new(&extensionless)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn()
            .is_err(),
        "a direct spawn without the .cmd extension must fail, or this test proves nothing"
    );

    let config = ServerConfig {
        command: Some(extensionless),
        ..ServerConfig::default()
    };

    let client = McpClient::connect("fake", &config)
        .await
        .expect("connect must fall back to cmd.exe for a .cmd shim");

    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "read_text_file");

    client.shutdown().await;
}

#[tokio::test]
async fn a_command_that_exists_nowhere_is_still_an_error() {
    let config = ServerConfig {
        command: Some("zarn_no_such_shim_anywhere".to_string()),
        ..ServerConfig::default()
    };
    assert!(McpClient::connect("missing", &config).await.is_err());
}

#[tokio::test]
async fn the_real_filesystem_server_reaches_the_registry() {
    if std::env::var("ZARN_MCP_LIVE").is_err() {
        return;
    }

    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("hello.txt"), "mcp smoke test file").unwrap();

    let servers = HashMap::from([(
        "fs".to_string(),
        ServerConfig {
            command: Some("npx".to_string()),
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
                dir.path().display().to_string(),
            ],
            ..ServerConfig::default()
        },
    )]);

    let tools = zarn_tools::mcp::connect_all(&servers).await;
    assert!(
        tools.iter().any(|t| t.spec().name == "fs__read_text_file"),
        "expected fs__read_text_file, got {:?}",
        tools.iter().map(|t| t.spec().name).collect::<Vec<_>>()
    );

    let read = tools
        .iter()
        .find(|t| t.spec().name == "fs__read_text_file")
        .unwrap();
    let ctx = Ctx::new(dir.path().to_path_buf());
    let out = read
        .run(
            json!({ "path": dir.path().join("hello.txt").display().to_string() }),
            &ctx,
        )
        .await
        .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("mcp smoke test file"), "{}", out.content);
}

#[tokio::test]
async fn a_configured_allow_lets_a_headless_run_call_an_mcp_tool() {
    use zarn_core::agent::{GateDecision, ToolGate};
    use zarn_core::tools::Risk;
    use zarn_tools::permissions::{Permissions, Policy};

    let bare = Permissions::headless();
    assert!(matches!(
        bare.check("fs__read_text_file", Risk::Execute, &json!({})).await,
        GateDecision::Deny(_)
    ));

    let allowed = Permissions::headless().with_tools(HashMap::from([(
        "fs__read_text_file".to_string(),
        Policy::Allow,
    )]));
    assert!(matches!(
        allowed
            .check("fs__read_text_file", Risk::Execute, &json!({}))
            .await,
        GateDecision::Allow
    ));
}

#[tokio::test]
async fn a_server_config_round_trips_through_toml() {
    let toml = r#"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "C:\\zarn\\scratch"]
"#;
    let config: ServerConfig = toml::from_str(toml).unwrap();
    assert_eq!(config.command.as_deref(), Some("npx"));
    assert_eq!(config.args.len(), 3);
    assert_eq!(config.args[2], "C:\\zarn\\scratch");
    assert_eq!(
        serde_json::to_value(&config).unwrap()["args"][1],
        json!("@modelcontextprotocol/server-filesystem")
    );
}
