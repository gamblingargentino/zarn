use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthServer {
    pub device_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub interval: u64,
    pub expires_in: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Token {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    pub client_id: String,
    pub token_endpoint: String,
}

impl Token {
    pub fn expired(&self, now: u64) -> bool {
        self.expires_at.is_some_and(|at| now + 30 >= at)
    }
}

pub fn form_body(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", crate::web::encode_query(k), crate::web::encode_query(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn post_form(client: &reqwest::Client, url: &str, pairs: &[(&str, &str)]) -> reqwest::RequestBuilder {
    client
        .post(url)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .body(form_body(pairs))
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn origin(url: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| anyhow!("{url} is not a URL: {e}"))?;
    let host = parsed.host_str().ok_or_else(|| anyhow!("{url} has no host"))?;
    Ok(match parsed.port() {
        Some(port) => format!("{}://{host}:{port}", parsed.scheme()),
        None => format!("{}://{host}", parsed.scheme()),
    })
}

async fn get_json(client: &reqwest::Client, url: &str) -> Option<Value> {
    let response = client.get(url).header("accept", "application/json").send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json().await.ok()
}

pub async fn discover(client: &reqwest::Client, mcp_url: &str) -> Result<AuthServer> {
    let base = origin(mcp_url)?;
    let issuer = match get_json(client, &format!("{base}/.well-known/oauth-protected-resource")).await {
        Some(meta) => meta
            .get("authorization_servers")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .map(|s| s.trim_end_matches('/').to_string())
            .unwrap_or_else(|| base.clone()),
        None => base.clone(),
    };
    let mut meta = None;
    for path in ["/.well-known/oauth-authorization-server", "/.well-known/openid-configuration"] {
        if let Some(found) = get_json(client, &format!("{issuer}{path}")).await {
            meta = Some(found);
            break;
        }
    }
    let meta = meta.ok_or_else(|| anyhow!("{issuer} publishes no OAuth metadata"))?;
    let field = |k: &str| meta.get(k).and_then(Value::as_str).map(str::to_string);
    let device_endpoint = field("device_authorization_endpoint")
        .ok_or_else(|| anyhow!("{issuer} does not support the device flow (no device_authorization_endpoint)"))?;
    let token_endpoint = field("token_endpoint").ok_or_else(|| anyhow!("{issuer} has no token_endpoint"))?;
    Ok(AuthServer {
        device_endpoint,
        token_endpoint,
        registration_endpoint: field("registration_endpoint"),
    })
}

pub async fn register(client: &reqwest::Client, endpoint: &str) -> Result<String> {
    let response = client
        .post(endpoint)
        .json(&json!({
            "client_name": "zarn",
            "grant_types": [DEVICE_GRANT, "refresh_token"],
            "token_endpoint_auth_method": "none"
        }))
        .send()
        .await?;
    if !response.status().is_success() {
        bail!("client registration failed with HTTP {}", response.status());
    }
    let body: Value = response.json().await?;
    body.get("client_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("client registration returned no client_id"))
}

pub async fn start_device(client: &reqwest::Client, server: &AuthServer, client_id: &str, scope: Option<&str>) -> Result<DeviceCode> {
    let mut form = vec![("client_id", client_id)];
    if let Some(scope) = scope {
        form.push(("scope", scope));
    }
    let response = post_form(client, &server.device_endpoint, &form).send().await?;
    if !response.status().is_success() {
        bail!("the device authorization request failed with HTTP {}", response.status());
    }
    let body: Value = response.json().await?;
    let field = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_string);
    Ok(DeviceCode {
        device_code: field("device_code").ok_or_else(|| anyhow!("no device_code in the response"))?,
        user_code: field("user_code").ok_or_else(|| anyhow!("no user_code in the response"))?,
        verification_uri: field("verification_uri")
            .or_else(|| field("verification_url"))
            .ok_or_else(|| anyhow!("no verification_uri in the response"))?,
        verification_uri_complete: field("verification_uri_complete"),
        interval: body.get("interval").and_then(Value::as_u64).unwrap_or(5),
        expires_in: body.get("expires_in").and_then(Value::as_u64).unwrap_or(600),
    })
}

fn token_from(body: &Value, client_id: &str, token_endpoint: &str, previous_refresh: Option<String>) -> Result<Token> {
    let access_token = body
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the token response has no access_token"))?
        .to_string();
    Ok(Token {
        access_token,
        refresh_token: body.get("refresh_token").and_then(Value::as_str).map(str::to_string).or(previous_refresh),
        expires_at: body.get("expires_in").and_then(Value::as_u64).map(|s| now() + s),
        client_id: client_id.to_string(),
        token_endpoint: token_endpoint.to_string(),
    })
}

pub async fn poll(client: &reqwest::Client, server: &AuthServer, client_id: &str, code: &DeviceCode) -> Result<Token> {
    let mut interval = Duration::from_secs(code.interval);
    let deadline = std::time::Instant::now() + Duration::from_secs(code.expires_in.max(1));
    loop {
        if std::time::Instant::now() > deadline {
            bail!("the device code expired before it was approved");
        }
        tokio::time::sleep(interval.max(Duration::from_millis(100))).await;
        let response = post_form(
            client,
            &server.token_endpoint,
            &[("grant_type", DEVICE_GRANT), ("device_code", code.device_code.as_str()), ("client_id", client_id)],
        )
        .send()
        .await?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return token_from(&body, client_id, &server.token_endpoint, None);
        }
        match body.get("error").and_then(Value::as_str) {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += Duration::from_secs(5),
            Some("access_denied") => bail!("the authorization was denied"),
            Some("expired_token") => bail!("the device code expired before it was approved"),
            Some(other) => bail!("the token endpoint answered {other}"),
            None => bail!("the token endpoint answered HTTP {status}"),
        }
    }
}

pub async fn refresh(client: &reqwest::Client, token: &Token) -> Result<Token> {
    let Some(refresh) = token.refresh_token.clone() else {
        bail!("the token expired and there is no refresh token; log in again");
    };
    let response = post_form(
        client,
        &token.token_endpoint,
        &[("grant_type", "refresh_token"), ("refresh_token", refresh.as_str()), ("client_id", token.client_id.as_str())],
    )
    .send()
    .await?;
    if !response.status().is_success() {
        bail!("refreshing the token failed with HTTP {}; log in again", response.status());
    }
    let body: Value = response.json().await?;
    token_from(&body, &token.client_id, &token.token_endpoint, Some(refresh))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_keep_scheme_host_and_port() {
        assert_eq!(origin("https://mcp.example.test/v1/mcp").unwrap(), "https://mcp.example.test");
        assert_eq!(origin("http://127.0.0.1:8080/mcp").unwrap(), "http://127.0.0.1:8080");
        assert!(origin("not a url").is_err());
    }

    #[test]
    fn form_bodies_are_url_encoded() {
        assert_eq!(form_body(&[("grant_type", DEVICE_GRANT), ("scope", "a b")]), "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&scope=a+b");
    }

    #[test]
    fn tokens_expire_a_little_early() {
        let t = Token { expires_at: Some(1000), ..Token::default() };
        assert!(!t.expired(900));
        assert!(t.expired(980));
        assert!(!Token::default().expired(u64::MAX - 100));
    }
}
