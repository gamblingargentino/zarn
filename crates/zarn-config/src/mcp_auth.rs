use crate::auth::{KeyStore, register_secret};
use anyhow::{Result, anyhow};
use zarn_tools::mcp::ServerConfig;
use zarn_tools::mcp_auth::{self, DeviceCode, Token};

pub const PREFIX: &str = "zarn-mcp:";

pub fn key_name(server: &str) -> String {
    format!("{PREFIX}{server}")
}

pub fn load(store: &KeyStore, server: &str) -> Option<Token> {
    let (raw, _) = store.resolve(&key_name(server), None)?;
    let token: Token = serde_json::from_str(&raw).ok()?;
    register_secret(&token.access_token);
    if let Some(refresh) = &token.refresh_token {
        register_secret(refresh);
    }
    Some(token)
}

pub fn save(store: &KeyStore, server: &str, token: &Token) -> Result<()> {
    register_secret(&token.access_token);
    if let Some(refresh) = &token.refresh_token {
        register_secret(refresh);
    }
    let raw = serde_json::to_string(token)?;
    store.save(&key_name(server), &raw).map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

pub fn forget(store: &KeyStore, server: &str) -> Result<bool> {
    store.remove(&key_name(server)).map_err(|e| anyhow!("{e}"))
}

pub async fn bearer(store: &KeyStore, server: &str) -> Option<String> {
    let token = load(store, server)?;
    if !token.expired(mcp_auth::now()) {
        return Some(token.access_token);
    }
    let client = reqwest::Client::new();
    match mcp_auth::refresh(&client, &token).await {
        Ok(fresh) => {
            let _ = save(store, server, &fresh);
            Some(fresh.access_token)
        }
        Err(err) => {
            tracing::warn!(server, error = %err, "mcp token refresh failed");
            None
        }
    }
}

pub async fn with_auth(store: &KeyStore, server: &str, config: &ServerConfig) -> ServerConfig {
    let mut config = config.clone();
    if config.url.is_some()
        && !config.headers.keys().any(|k| k.eq_ignore_ascii_case("authorization"))
        && let Some(token) = bearer(store, server).await
    {
        config.headers.insert("Authorization".into(), format!("Bearer {token}"));
    }
    config
}

pub async fn login(
    store: &KeyStore,
    server: &str,
    config: &ServerConfig,
    show: impl Fn(&DeviceCode),
) -> Result<()> {
    let url = config.url.as_deref().ok_or_else(|| anyhow!("{server} is not an HTTP server; only remote servers log in"))?;
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build()?;
    let auth = mcp_auth::discover(&client, url).await?;
    let client_id = match (&config.client_id, &auth.registration_endpoint) {
        (Some(id), _) => id.clone(),
        (None, Some(endpoint)) => mcp_auth::register(&client, endpoint).await?,
        (None, None) => return Err(anyhow!("{server} needs a client_id in its [mcp.{server}] config; its authorization server does not allow registration")),
    };
    let code = mcp_auth::start_device(&client, &auth, &client_id, config.scope.as_deref()).await?;
    show(&code);
    let token = mcp_auth::poll(&client, &auth, &client_id, &code).await?;
    save(store, server, &token)
}
