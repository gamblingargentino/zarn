mod support;

use support::*;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zarn_config::layers::Layers;
use zarn_config::ops;

async fn server(gone: &str) -> MockServer {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_partial_json(serde_json::json!({ "model": gone })))
        .respond_with(ResponseTemplate::new(404).set_body_string(format!(
            "{{\"error\":{{\"message\":\"This model models/{gone} is no longer available to new users.\"}}}}"
        )))
        .with_priority(1)
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(reply_body("OK"))
                .insert_header("content-type", "text/event-stream"),
        )
        .with_priority(2)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(format!(
            "{{\"data\":[{{\"id\":\"{gone}\"}},{{\"id\":\"vendor-tts\"}},{{\"id\":\"works\"}}]}}"
        )))
        .mount(&mock)
        .await;
    mock
}

#[tokio::test]
async fn a_model_the_key_cannot_use_is_swapped_for_one_that_answers() {
    let mock = server("first").await;
    let dir = tempfile::tempdir().unwrap();
    let user = dir.path().join("user.toml");
    std::fs::write(
        &user,
        format!(
            "[providers.mine]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[profiles.default]\nprovider = \"mine\"\n",
            mock.uri()
        ),
    )
    .unwrap();
    let mut layers = Layers::load_from(dir.path().join("project.toml"), Some(user.clone())).unwrap();
    let (_keys, store) = store();
    let cache = dir.path().join("cache");

    let done = ops::connect(&mut layers, &store, &cache, "mine", "test-key-not-real", true)
        .await
        .unwrap();

    assert_eq!(done.model.as_deref(), Some("works"));
    assert_eq!(
        layers.merged().profiles["default"].model.as_deref(),
        Some("works")
    );
    assert!(done.summary().contains("2 models found · using works"), "{}", done.summary());
    let tried: Vec<String> = mock
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/chat/completions")
        .map(|r| serde_json::from_slice::<serde_json::Value>(&r.body).unwrap()["model"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(tried, ["first", "works"], "the text-to-speech model is never tried");
}

#[tokio::test]
async fn a_model_that_answers_is_kept_with_a_single_test_request() {
    let mock = server("other").await;
    let dir = tempfile::tempdir().unwrap();
    let user = dir.path().join("user.toml");
    std::fs::write(
        &user,
        format!(
            "[providers.mine]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[profiles.default]\nprovider = \"mine\"\nmodel = \"works\"\n",
            mock.uri()
        ),
    )
    .unwrap();
    let mut layers = Layers::load_from(dir.path().join("project.toml"), Some(user)).unwrap();
    let (_keys, store) = store();

    let done = ops::connect(&mut layers, &store, &dir.path().join("cache"), "mine", "test-key-not-real", true)
        .await
        .unwrap();

    assert_eq!(done.model.as_deref(), Some("works"));
    let chats = mock
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/chat/completions")
        .count();
    assert_eq!(chats, 1);
}
