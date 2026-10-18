use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zarn_providers::ProviderSpec;
use zarn_providers::discovery::{self, Credits, Limits};

const KEY: &str = "test-key-not-real-discovery";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/discovery")
            .join(name),
    )
    .unwrap()
}

fn spec(kind: &str, base: &str) -> ProviderSpec {
    ProviderSpec {
        kind: kind.into(),
        base_url: Some(base.into()),
        api_key: Some(KEY.into()),
        ..ProviderSpec::default()
    }
}

async fn serve(route: &str, body: String) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn anthropic_models_carry_the_context_window_when_given() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("x-api-key", KEY))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("anthropic_models.json")))
        .mount(&server)
        .await;
    let found = discovery::models("claude", &spec("anthropic", &format!("{}/v1", server.uri())))
        .await
        .unwrap();
    assert_eq!(found.len(), 2);
    let sonnet = found.iter().find(|m| m.id == "claude-sonnet-5-5").unwrap();
    assert_eq!(sonnet.context_window, Some(1_000_000));
    let haiku = found.iter().find(|m| m.id == "claude-haiku-5-5").unwrap();
    assert_eq!(haiku.context_window, None);
    assert_eq!(haiku.vision, None);
}

#[tokio::test]
async fn openai_models_leave_unknown_fields_unknown() {
    let server = serve("/v1/models", fixture("openai_models.json")).await;
    let found = discovery::models("openai", &spec("openai", &format!("{}/v1", server.uri())))
        .await
        .unwrap();
    assert_eq!(found.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["gpt-5", "gpt-5-mini"]);
    assert!(found.iter().all(|m| m.context_window.is_none() && m.tools.is_none() && m.vision.is_none()));
}

#[tokio::test]
async fn groq_models_report_their_context_window() {
    let server = serve("/openai/v1/models", fixture("groq_models.json")).await;
    let found = discovery::models("groq", &spec("openai-compat", &format!("{}/openai/v1", server.uri())))
        .await
        .unwrap();
    assert_eq!(found[0].id, "llama-3.3-70b-versatile");
    assert_eq!(found[0].context_window, Some(131_072));
}

#[tokio::test]
async fn openrouter_models_report_tools_and_vision() {
    let server = serve("/api/v1/models", fixture("openrouter_models.json")).await;
    let found = discovery::models("router", &spec("openai-compat", &format!("{}/api/v1", server.uri())))
        .await
        .unwrap();
    let sonnet = found.iter().find(|m| m.id == "anthropic/claude-sonnet-5.5").unwrap();
    assert_eq!(sonnet.tools, Some(true));
    assert_eq!(sonnet.vision, Some(true));
    let guard = found.iter().find(|m| m.id == "meta-llama/llama-guard").unwrap();
    assert_eq!(guard.tools, Some(false));
    assert_eq!(guard.context_window, Some(8192));
}

#[tokio::test]
async fn google_models_strip_the_prefix_and_skip_non_generating_models() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1beta/models"))
        .and(header("x-goog-api-key", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("google_models.json")))
        .mount(&server)
        .await;
    let found = discovery::models("gemini", &spec("google", &format!("{}/v1beta", server.uri())))
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "gemini-2.5-pro");
    assert_eq!(found[0].context_window, Some(1_048_576));
}

#[tokio::test]
async fn ollama_models_come_from_api_tags() {
    let server = serve("/api/tags", fixture("ollama_tags.json")).await;
    let found = discovery::models("local", &spec("ollama", &server.uri())).await.unwrap();
    assert_eq!(found[0].id, "qwen3:8b");
}

#[tokio::test]
async fn a_401_names_the_provider_and_never_the_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_string(format!(
            "{{\"error\":{{\"message\":\"Incorrect API key provided: {KEY}\"}}}}"
        )))
        .mount(&server)
        .await;
    let err = discovery::models("work-openai", &spec("openai", &server.uri()))
        .await
        .unwrap_err();
    assert!(err.contains("work-openai"), "{err}");
    assert!(err.contains("401"), "{err}");
    assert!(!err.contains(KEY));
    assert!(!err.contains("not-real"));
}

#[tokio::test]
async fn limits_come_from_the_probe_response_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("{\"content\":[]}")
                .insert_header("anthropic-ratelimit-requests-limit", "50")
                .insert_header("anthropic-ratelimit-requests-remaining", "49")
                .insert_header("anthropic-ratelimit-requests-reset", "2026-10-16T12:00:30Z")
                .insert_header("anthropic-ratelimit-tokens-limit", "40000")
                .insert_header("anthropic-ratelimit-tokens-remaining", "39990")
                .insert_header("anthropic-ratelimit-input-tokens-limit", "30000"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let found = discovery::limits("claude", &spec("anthropic", &format!("{}/v1", server.uri())), "claude-haiku-5-5")
        .await
        .unwrap();
    let Limits::Found { requests, tokens } = found else {
        panic!("expected limits");
    };
    assert_eq!(requests.limit.as_deref(), Some("50"));
    assert_eq!(requests.remaining.as_deref(), Some("49"));
    assert_eq!(requests.reset.as_deref(), Some("2026-10-16T12:00:30Z"));
    assert_eq!(tokens.limit.as_deref(), Some("40000"));
    assert_eq!(tokens.remaining.as_deref(), Some("39990"));
    let body: serde_json::Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(body["max_tokens"], 1);
}

#[tokio::test]
async fn a_probe_without_headers_reports_not_exposed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    let found = discovery::limits("local", &spec("openai-compat", &server.uri()), "m").await.unwrap();
    assert_eq!(found, Limits::NotExposed);
}

#[tokio::test]
async fn openrouter_exposes_credits_through_its_key_endpoint() {
    let server = serve("/api/v1/key", fixture("openrouter_key.json")).await;
    let mut s = spec("openai-compat", &format!("{}/api/v1", server.uri()));
    s.extra.insert("credits_url".into(), format!("{}/api/v1/key", server.uri()));
    let found = discovery::credits("router", &s).await.unwrap();
    assert_eq!(
        found,
        Credits::Found {
            balance: Some(37.5),
            limit: Some(50.0),
            used: Some(12.5),
            currency: Some("USD".into())
        }
    );
}

#[tokio::test]
async fn deepseek_balance_parses() {
    let server = serve("/user/balance", fixture("deepseek_balance.json")).await;
    let mut s = spec("openai-compat", &server.uri());
    s.extra.insert("credits_url".into(), format!("{}/user/balance", server.uri()));
    s.extra.insert("credits_format".into(), "deepseek".into());
    let Credits::Found { balance, currency, .. } = discovery::credits("ds", &s).await.unwrap() else {
        panic!("expected a balance");
    };
    assert_eq!(balance, Some(110.0));
    assert_eq!(currency.as_deref(), Some("USD"));
}

#[tokio::test]
async fn providers_without_a_balance_endpoint_say_not_exposed_without_a_request() {
    let found = discovery::credits("openai", &spec("openai", "http://127.0.0.1:9")).await.unwrap();
    assert_eq!(found, Credits::NotExposed);
}

#[test]
fn hosted_openrouter_and_deepseek_are_recognised_by_url() {
    let or = spec("openai-compat", "https://openrouter.ai/api/v1");
    assert_eq!(discovery::credits_endpoint(&or).unwrap().0, "https://openrouter.ai/api/v1/key");
    let ds = spec("openai-compat", "https://api.deepseek.com/v1");
    assert_eq!(discovery::credits_endpoint(&ds).unwrap().0, "https://api.deepseek.com/user/balance");
}
