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

#[test]
fn a_profile_without_a_chain_gets_automatic_fallbacks_from_the_same_provider() {
    let store = zarn_config::auth::KeyStore::new(
        std::sync::Arc::new(zarn_config::auth::MemoryBackend::new()),
        tempfile::tempdir().unwrap().path().join("auth.json"),
    );
    store.save("google", "test-key-not-real-auto-0001").unwrap();
    let parse = |extra: &str| -> zarn_config::Config {
        toml::from_str(&format!(
            "[general]\n{extra}\n[providers.google]\nkind = \"google\"\n\n[providers.deepseek]\nkind = \"deepseek\"\n\n[profiles.default]\nprovider = \"google\"\nmodel = \"gemini-2.5-flash\"\n"
        ))
        .unwrap()
    };
    let plan = zarn_config::runtime::plan(&parse(""), &store, "default", None).unwrap();
    let labels = plan.chain_labels();
    assert_eq!(labels[0], "google:gemini-2.5-flash");
    assert_eq!(labels.len(), 3, "{labels:?}");
    assert!(labels[1..].iter().all(|l| l.starts_with("google:") && l != "google:gemini-2.5-flash"), "{labels:?}");
    assert_eq!(plan.auto_from, Some(1));

    let off = zarn_config::runtime::plan(&parse("auto_switch = \"off\""), &store, "default", None).unwrap();
    assert_eq!(off.chain_labels(), ["google:gemini-2.5-flash"]);
    assert_eq!(off.auto_from, None);

    let all = zarn_config::runtime::plan(&parse("auto_switch = \"all\""), &store, "default", None).unwrap();
    assert!(!all.chain_labels().iter().any(|l| l.starts_with("deepseek:")), "a provider without a key is never used");
    store.save("deepseek", "test-key-not-real-auto-0002").unwrap();
    let all = zarn_config::runtime::plan(&parse("auto_switch = \"all\""), &store, "default", None).unwrap();
    assert!(all.chain_labels().iter().any(|l| l.starts_with("deepseek:")), "{:?}", all.chain_labels());

    let mut chained = parse("");
    chained.profiles.get_mut("default").unwrap().chain = Some(vec!["google:gemini-flash-lite-latest".into()]);
    let explicit = zarn_config::runtime::plan(&chained, &store, "default", None).unwrap();
    assert_eq!(explicit.chain_labels(), ["google:gemini-2.5-flash", "google:gemini-flash-lite-latest"]);
    assert_eq!(explicit.auto_from, None, "a chain you set replaces the automatic one");
}
