use std::sync::Arc;
use zarn_config::auth::{KeyStore, MemoryBackend};
use zarn_config::layers::Layers;
use zarn_config::mcp_auth;
use zarn_tools::mcp::ServerConfig;

fn shaped() -> String {
    format!("sk-{}", "r4".repeat(14))
}

#[test]
fn a_literal_secret_in_mcp_env_or_headers_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let user = dir.path().join("user.toml");
    for body in [
        format!("[mcp.gh]\ncommand = \"gh-mcp\"\n\n[mcp.gh.env]\nGITHUB_TOKEN = \"{}\"\n", shaped()),
        format!("[mcp.gh]\nurl = \"https://mcp.example.test\"\n\n[mcp.gh.headers]\nAuthorization = \"Bearer {}\"\n", shaped()),
    ] {
        std::fs::write(&user, body).unwrap();
        let err = Layers::load_from(dir.path().join("project.toml"), Some(user.clone())).unwrap_err().to_string();
        assert!(err.contains("looks like a openai secret"), "{err}");
        assert!(err.contains("key:gh-") && err.contains("env:VARIABLE"), "{err}");
    }
    std::fs::write(
        &user,
        "[mcp.gh]\ncommand = \"gh-mcp\"\n\n[mcp.gh.env]\nGITHUB_TOKEN = \"key:gh-token\"\nLOG = \"debug\"\n\n[mcp.gh.headers]\nAuthorization = \"Bearer env:GH_TOKEN\"\n",
    )
    .unwrap();
    assert!(Layers::load_from(dir.path().join("project.toml"), Some(user)).is_ok());
}

#[tokio::test]
async fn references_are_resolved_when_the_server_starts() {
    let dir = tempfile::tempdir().unwrap();
    let store = KeyStore::new(Arc::new(MemoryBackend::new()), dir.path().join("auth.json"));
    store.save("gh-token", "test-key-not-real-mcp-env-0001").unwrap();
    unsafe {
        std::env::set_var("ZARN_MCP_REFS_HEADER", "test-key-not-real-mcp-header-0002");
    }
    let mut config = ServerConfig { command: Some("gh-mcp".into()), ..ServerConfig::default() };
    config.env.insert("GITHUB_TOKEN".into(), "key:gh-token".into());
    config.env.insert("LOG".into(), "debug".into());
    config.headers.insert("Authorization".into(), "Bearer env:ZARN_MCP_REFS_HEADER".into());
    let ready = mcp_auth::prepare(&store, "gh", &config).await.unwrap();
    assert_eq!(ready.env["GITHUB_TOKEN"], "test-key-not-real-mcp-env-0001");
    assert_eq!(ready.env["LOG"], "debug");
    assert_eq!(ready.headers["Authorization"], "Bearer test-key-not-real-mcp-header-0002");
    assert_eq!(config.env["GITHUB_TOKEN"], "key:gh-token", "the stored config keeps the reference");
    assert!(!zarn_config::auth::redact("x test-key-not-real-mcp-env-0001").contains("mcp-env"));

    config.env.insert("MISSING".into(), "env:ZARN_MCP_REFS_UNSET".into());
    let err = mcp_auth::prepare(&store, "gh", &config).await.unwrap_err();
    assert!(err.contains("env.MISSING") && err.contains("not set"), "{err}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_resolved_env_value_reaches_the_server_process() {
    let dir = tempfile::tempdir().unwrap();
    let store = KeyStore::new(Arc::new(MemoryBackend::new()), dir.path().join("auth.json"));
    store.save("probe-token", "test-key-not-real-mcp-probe-0003").unwrap();
    let script = dir.path().join("server.sh");
    std::fs::write(
        &script,
        "while IFS= read -r line; do id=$(printf '%s' \"$line\" | grep -o '\"id\":[0-9]*' | cut -d: -f2); case \"$line\" in *initialize*) printf '{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{\"capabilities\":{}}}\\n' \"$id\";; *tools/list*) printf '{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{\"tools\":[{\"name\":\"t_%s\",\"inputSchema\":{\"type\":\"object\"}}]}}\\n' \"$id\" \"${#PROBE}\";; esac; done\n",
    )
    .unwrap();
    let mut config = ServerConfig { command: Some("sh".into()), args: vec![script.display().to_string()], ..ServerConfig::default() };
    config.env.insert("PROBE".into(), "key:probe-token".into());
    let ready = mcp_auth::prepare(&store, "probe", &config).await.unwrap();
    let server = zarn_tools::mcp::McpServer::start("probe", &ready).await.unwrap();
    let expected = format!("probe__t_{}", "test-key-not-real-mcp-probe-0003".len());
    assert_eq!(server.tool_names(), vec![expected]);
    server.shutdown().await;
}
