use crate::ProviderSpec;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Window {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset: Option<String>,
}

impl Window {
    fn is_empty(&self) -> bool {
        self.limit.is_none() && self.remaining.is_none() && self.reset.is_none()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Limits {
    Found { requests: Window, tokens: Window },
    NotExposed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Credits {
    Found {
        balance: Option<f64>,
        limit: Option<f64>,
        used: Option<f64>,
        currency: Option<String>,
    },
    NotExposed,
}

fn kind(spec: &ProviderSpec) -> &str {
    match spec.kind.as_str() {
        "claude" => "anthropic",
        "gemini" => "google",
        "openai-compat" => "openai",
        other => other,
    }
}

pub fn base_url(spec: &ProviderSpec) -> String {
    let default = match kind(spec) {
        "anthropic" => "https://api.anthropic.com/v1",
        "google" => "https://generativelanguage.googleapis.com/v1beta",
        "ollama" => "http://localhost:11434",
        _ => "https://api.openai.com/v1",
    };
    spec.base_url
        .clone()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn scrub(spec: &ProviderSpec, text: &str) -> String {
    match spec.api_key.as_deref().filter(|k| k.len() >= 4) {
        Some(key) => text.replace(key, "[redacted]"),
        None => text.to_string(),
    }
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())
}

fn authorize(spec: &ProviderSpec, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    let key = spec.api_key.clone().unwrap_or_default();
    match kind(spec) {
        "anthropic" => request
            .header("x-api-key", key)
            .header("anthropic-version", ANTHROPIC_VERSION),
        "google" => request.header("x-goog-api-key", key),
        "ollama" => request,
        _ if key.is_empty() => request,
        _ => request.bearer_auth(key),
    }
}

fn describe_status(status: u16) -> &'static str {
    match status {
        401 => "the key was rejected",
        403 => "the key is not allowed to do this",
        404 => "the endpoint does not exist on this server",
        429 => "rate-limited",
        500..=599 => "the server failed",
        _ => "the request failed",
    }
}

async fn send(
    name: &str,
    spec: &ProviderSpec,
    request: reqwest::RequestBuilder,
) -> Result<(HeaderMap, Value), String> {
    let response = authorize(spec, request).send().await.map_err(|err| {
        let why = if err.is_timeout() {
            "timed out".to_string()
        } else if err.is_connect() {
            "could not connect".to_string()
        } else {
            scrub(spec, &err.without_url().to_string())
        };
        format!("{name}: {why}")
    })?;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let text = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!("{name}: {} (HTTP {status})", describe_status(status)));
    }
    let body = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok((headers, body))
}

fn number(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .and_then(|n| u32::try_from(n).ok())
}

fn flag(map: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|k| map.get(*k).and_then(Value::as_bool))
}

fn parse_anthropic(body: &Value) -> Vec<ModelInfo> {
    body.get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let caps = m.get("capabilities").cloned().unwrap_or(Value::Null);
            Some(ModelInfo {
                id: m.get("id")?.as_str()?.to_string(),
                context_window: m.get("max_input_tokens").and_then(number),
                tools: flag(&caps, &["tools", "tool_use"]),
                vision: flag(&caps, &["vision", "image_input"]),
            })
        })
        .collect()
}

fn parse_openai(body: &Value) -> Vec<ModelInfo> {
    body.get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let modalities = m
                .pointer("/architecture/input_modalities")
                .and_then(Value::as_array);
            let parameters = m.get("supported_parameters").and_then(Value::as_array);
            Some(ModelInfo {
                id: m.get("id")?.as_str()?.to_string(),
                context_window: ["context_window", "context_length", "max_model_len"]
                    .iter()
                    .find_map(|k| m.get(*k).and_then(number)),
                tools: parameters.map(|p| p.iter().any(|v| v == "tools")),
                vision: modalities.map(|p| p.iter().any(|v| v == "image")),
            })
        })
        .collect()
}

fn parse_google(body: &Value) -> Vec<ModelInfo> {
    body.get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|m| {
            m.get("supportedGenerationMethods")
                .and_then(Value::as_array)
                .is_none_or(|methods| methods.iter().any(|v| v == "generateContent"))
        })
        .filter_map(|m| {
            let name = m.get("name")?.as_str()?;
            Some(ModelInfo {
                id: name.strip_prefix("models/").unwrap_or(name).to_string(),
                context_window: m.get("inputTokenLimit").and_then(number),
                tools: None,
                vision: None,
            })
        })
        .collect()
}

fn parse_ollama(body: &Value) -> Vec<ModelInfo> {
    body.get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| {
            Some(ModelInfo {
                id: m.get("name").or_else(|| m.get("model"))?.as_str()?.to_string(),
                ..ModelInfo::default()
            })
        })
        .collect()
}

pub async fn models(name: &str, spec: &ProviderSpec) -> Result<Vec<ModelInfo>, String> {
    let client = client()?;
    let base = base_url(spec);
    let (path, parse): (String, fn(&Value) -> Vec<ModelInfo>) = match kind(spec) {
        "anthropic" => (format!("{base}/models?limit=1000"), parse_anthropic),
        "google" => (format!("{base}/models?pageSize=1000"), parse_google),
        "ollama" => (format!("{base}/api/tags"), parse_ollama),
        "openai" => (format!("{base}/models"), parse_openai),
        other => return Err(format!("{name}: model listing is not supported for kind {other}")),
    };
    let (_, body) = send(name, spec, client.get(path)).await?;
    let mut found = parse(&body);
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found.dedup_by(|a, b| a.id == b.id);
    Ok(found)
}

pub fn parse_limits(headers: &HeaderMap) -> Limits {
    let mut requests = Window::default();
    let mut tokens = Window::default();
    for (name, value) in headers {
        let name = name.as_str().to_lowercase();
        if !name.contains("ratelimit") {
            continue;
        }
        let Ok(value) = value.to_str() else { continue };
        let is_input_or_output = name.contains("input-tokens") || name.contains("output-tokens");
        let window = if name.contains("request") {
            &mut requests
        } else if name.contains("token") && !is_input_or_output {
            &mut tokens
        } else {
            continue;
        };
        let slot = if name.contains("remaining") {
            &mut window.remaining
        } else if name.contains("reset") {
            &mut window.reset
        } else if name.contains("limit") {
            &mut window.limit
        } else {
            continue;
        };
        slot.get_or_insert_with(|| value.to_string());
    }
    if requests.is_empty() && tokens.is_empty() {
        Limits::NotExposed
    } else {
        Limits::Found { requests, tokens }
    }
}

pub async fn limits(name: &str, spec: &ProviderSpec, model: &str) -> Result<Limits, String> {
    let client = client()?;
    let base = base_url(spec);
    let request = match kind(spec) {
        "ollama" => return Ok(Limits::NotExposed),
        "anthropic" => client.post(format!("{base}/messages")).json(&json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{ "role": "user", "content": "hi" }]
        })),
        "google" => client
            .post(format!("{base}/models/{model}:generateContent"))
            .json(&json!({
                "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }],
                "generationConfig": { "maxOutputTokens": 1 }
            })),
        _ => client.post(format!("{base}/chat/completions")).json(&json!({
            "model": model,
            "max_tokens": 1,
            "messages": [{ "role": "user", "content": "hi" }]
        })),
    };
    let (headers, _) = send(name, spec, request).await?;
    Ok(parse_limits(&headers))
}

fn float(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

pub fn parse_openrouter_key(body: &Value) -> Credits {
    let data = body.get("data").unwrap_or(body);
    let limit = float(data.get("limit"));
    let used = float(data.get("usage"));
    let balance = float(data.get("limit_remaining")).or(match (limit, used) {
        (Some(l), Some(u)) => Some(l - u),
        _ => None,
    });
    Credits::Found {
        balance,
        limit,
        used,
        currency: Some("USD".into()),
    }
}

pub fn parse_deepseek_balance(body: &Value) -> Credits {
    let info = body
        .get("balance_infos")
        .and_then(Value::as_array)
        .and_then(|list| list.first())
        .cloned()
        .unwrap_or(Value::Null);
    Credits::Found {
        balance: float(info.get("total_balance")),
        limit: None,
        used: None,
        currency: info.get("currency").and_then(Value::as_str).map(str::to_string),
    }
}

pub type CreditParser = fn(&Value) -> Credits;

pub fn credits_endpoint(spec: &ProviderSpec) -> Option<(String, CreditParser)> {
    let base = base_url(spec);
    if base.contains("openrouter.ai") {
        return Some((format!("{base}/key"), parse_openrouter_key));
    }
    if base.contains("deepseek.com") {
        let root = base.trim_end_matches("/v1");
        return Some((format!("{root}/user/balance"), parse_deepseek_balance));
    }
    spec.extra.get("credits_url").map(|url| {
        let parse: CreditParser = match spec.extra.get("credits_format").map(String::as_str) {
            Some("deepseek") => parse_deepseek_balance,
            _ => parse_openrouter_key,
        };
        (url.clone(), parse)
    })
}

pub async fn credits(name: &str, spec: &ProviderSpec) -> Result<Credits, String> {
    let Some((url, parse)) = credits_endpoint(spec) else {
        return Ok(Credits::NotExposed);
    };
    let client = client()?;
    let (_, body) = send(name, spec, client.get(url)).await?;
    Ok(parse(&body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_headers_are_grouped_into_requests_and_tokens() {
        let mut headers = HeaderMap::new();
        for (k, v) in [
            ("x-ratelimit-limit-requests", "500"),
            ("x-ratelimit-remaining-requests", "499"),
            ("x-ratelimit-reset-requests", "120ms"),
            ("x-ratelimit-limit-tokens", "30000"),
            ("x-ratelimit-remaining-tokens", "29990"),
            ("content-type", "application/json"),
        ] {
            headers.insert(
                reqwest::header::HeaderName::from_static(k),
                v.parse().unwrap(),
            );
        }
        let Limits::Found { requests, tokens } = parse_limits(&headers) else {
            panic!("expected limits");
        };
        assert_eq!(requests.limit.as_deref(), Some("500"));
        assert_eq!(requests.remaining.as_deref(), Some("499"));
        assert_eq!(requests.reset.as_deref(), Some("120ms"));
        assert_eq!(tokens.limit.as_deref(), Some("30000"));
        assert!(tokens.reset.is_none());
        assert_eq!(parse_limits(&HeaderMap::new()), Limits::NotExposed);
    }

    #[test]
    fn the_base_url_defaults_per_kind() {
        let spec = |kind: &str| ProviderSpec {
            kind: kind.into(),
            ..ProviderSpec::default()
        };
        assert_eq!(base_url(&spec("claude")), "https://api.anthropic.com/v1");
        assert_eq!(base_url(&spec("ollama")), "http://localhost:11434");
        assert!(credits_endpoint(&spec("openai")).is_none());
    }
}
