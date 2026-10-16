use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};
use zarn_config::auth::{KeyStore, MemoryBackend, redact};
use zarn_config::mcp_auth;
use zarn_tools::mcp::{McpServer, ServerConfig};
use zarn_tools::mcp_auth::Token;

const ACCESS: &str = "test-key-not-real-mcp-access-0001";
const REFRESH: &str = "test-key-not-real-mcp-refresh-0002";

struct Mcp;

impl Respond for Mcp {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let result = match body["method"].as_str() {
            Some("initialize") => json!({ "protocolVersion": "2024-11-05", "capabilities": { "tools": {} } }),
            Some("tools/list") => json!({ "tools": [{ "name": "remote_tool", "description": "a remote tool", "inputSchema": { "type": "object" } }] }),
            _ => json!({}),
        };
        if id.is_null() {
            return ResponseTemplate::new(202);
        }
        ResponseTemplate::new(200).set_body_json(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }
}

async fn oauth_server() -> MockServer {
    let server = MockServer::start().await;
    let base = server.uri();
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-protected-resource"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "authorization_servers": [format!("{base}/auth")] })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_authorization_endpoint": format!("{base}/auth/device"),
            "token_endpoint": format!("{base}/auth/token"),
            "registration_endpoint": format!("{base}/auth/register")
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/register"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "client_id": "zarn-registered" })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/device"))
        .and(body_string_contains("client_id=zarn-registered"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dev-1",
            "user_code": "ABCD-EFGH",
            "verification_uri": format!("{base}/activate"),
            "interval": 0,
            "expires_in": 30
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/token"))
        .and(body_string_contains("device_code=dev-1"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "error": "authorization_pending" })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/token"))
        .and(body_string_contains("device_code=dev-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": ACCESS,
            "refresh_token": REFRESH,
            "expires_in": 3600
        })))
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "test-key-not-real-mcp-access-0003",
            "expires_in": 3600
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(header("authorization", format!("Bearer {ACCESS}").as_str()))
        .respond_with(Mcp)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(5)
        .mount(&server)
        .await;
    server
}

fn store() -> (tempfile::TempDir, KeyStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = KeyStore::new(Arc::new(MemoryBackend::new()), dir.path().join("auth.json"));
    (dir, store)
}

#[tokio::test]
async fn the_device_flow_stores_the_token_in_the_credential_store_and_authorizes_requests() {
    let server = oauth_server().await;
    let (dir, store) = store();
    let config = ServerConfig { url: Some(format!("{}/mcp", server.uri())), ..ServerConfig::default() };

    let refused = McpServer::start("remote", &config).await.err().unwrap().to_string();
    assert!(refused.contains("run /mcp login remote"), "{refused}");

    let shown = Arc::new(Mutex::new(None));
    let seen = shown.clone();
    mcp_auth::login(&store, "remote", &config, move |code| *seen.lock().unwrap() = Some(code.user_code.clone()))
        .await
        .unwrap();
    assert_eq!(shown.lock().unwrap().as_deref(), Some("ABCD-EFGH"));

    let (raw, _) = store.resolve("zarn-mcp:remote", None).expect("stored under zarn-mcp:<server>");
    let token: Token = serde_json::from_str(&raw).unwrap();
    assert_eq!(token.access_token, ACCESS);
    assert_eq!(token.client_id, "zarn-registered");
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|e| {
        std::fs::read_to_string(e.unwrap().path()).unwrap_or_default().contains(ACCESS)
    }), "nothing on disk when the credential store works");
    assert!(!redact(&format!("header Bearer {ACCESS} and {REFRESH}")).contains("test-key-not-real"));

    let authed = mcp_auth::with_auth(&store, "remote", &config).await;
    assert_eq!(authed.headers.get("Authorization").map(String::as_str), Some(format!("Bearer {ACCESS}").as_str()));
    assert!(config.headers.is_empty(), "the saved config never carries the token");
    let connected = McpServer::start("remote", &authed).await.unwrap();
    assert_eq!(connected.tool_names(), vec!["remote__remote_tool"]);
}

#[tokio::test]
async fn an_expired_token_is_refreshed_and_saved() {
    let server = oauth_server().await;
    let (_dir, store) = store();
    mcp_auth::save(
        &store,
        "remote",
        &Token {
            access_token: "test-key-not-real-mcp-old".into(),
            refresh_token: Some(REFRESH.into()),
            expires_at: Some(1),
            client_id: "zarn-registered".into(),
            token_endpoint: format!("{}/auth/token", server.uri()),
        },
    )
    .unwrap();
    assert_eq!(mcp_auth::bearer(&store, "remote").await.as_deref(), Some("test-key-not-real-mcp-access-0003"));
    let saved = mcp_auth::load(&store, "remote").unwrap();
    assert_eq!(saved.access_token, "test-key-not-real-mcp-access-0003");
    assert_eq!(saved.refresh_token.as_deref(), Some(REFRESH), "the refresh token is kept when not rotated");
    assert!(mcp_auth::forget(&store, "remote").unwrap());
    assert!(mcp_auth::bearer(&store, "remote").await.is_none());
}
