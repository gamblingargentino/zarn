mod support;

use support::*;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn missing_model_server() -> MockServer {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(404).set_body_string(r#"{"error":{"message":"No endpoints found for m."}}"#))
        .mount(&mock)
        .await;
    mock
}

fn two_providers(main: &str, spare: &str, chain: &str) -> String {
    format!(
        "[providers.main]\nkind = \"openai-compat\"\nbase_url = \"{main}\"\n\n[providers.spare]\nkind = \"openai-compat\"\nbase_url = \"{spare}\"\n\n[profiles.default]\nprovider = \"main\"\nmodel = \"m\"\n{chain}\n"
    )
}

#[tokio::test]
async fn a_failing_model_hands_the_turn_to_the_next_in_the_chain_with_its_own_model() {
    let main = missing_model_server().await;
    let spare = echo_server("from the spare").await;
    let env = Env::new(&two_providers(&main.uri(), &spare.uri(), "chain = [\"spare:backup-model\"]")).await;
    let mut c = env.controller();

    turn(&mut c, "hello").await;
    assert_eq!(assistant(&c).last().map(String::as_str), Some("from the spare"));
    let sent = chat_bodies(&spare).await;
    assert_eq!(sent[0]["model"], "backup-model", "a chain entry uses its own model, not the main one");
    let switch = notices(&c).into_iter().find(|n| n.contains("answered by")).unwrap();
    assert!(switch.contains("main:m does not offer this model, answered by spare:backup-model"), "{switch}");
    assert!(switch.contains("sits out for 30 min"), "{switch}");

    assert_eq!(c.app().status.model, "spare:backup-model (instead of m)", "the status bar names the model answering");
    let asked_main = main.received_requests().await.unwrap().len();
    c.type_line("/style concise");
    turn(&mut c, "again").await;
    assert_eq!(main.received_requests().await.unwrap().len(), asked_main, "a resting model is not asked, even after a rebuild");
    assert_eq!(chat_bodies(&spare).await.len(), 2);

    c.type_line("/chain");
    let shown = notices(&c).last().unwrap().clone();
    assert!(shown.contains("1. main:m") && shown.contains("resting"), "{shown}");
    assert!(shown.contains("2. spare:backup-model"), "{shown}");
}

#[tokio::test]
async fn provider_fallbacks_use_their_own_model() {
    let main = missing_model_server().await;
    let spare = echo_server("ok").await;
    let mut config = two_providers(&main.uri(), &spare.uri(), "fallback = [\"spare\"]");
    config.push_str("\n[profiles.other]\nprovider = \"spare\"\nmodel = \"spare-default\"\n");
    let env = Env::new(&config).await;
    let mut c = env.controller();
    turn(&mut c, "hello").await;
    assert_eq!(chat_bodies(&spare).await[0]["model"], "spare-default", "not the main model's name");
}

#[tokio::test]
async fn chain_commands_edit_and_save_the_chain() {
    let main = echo_server("ok").await;
    let spare = echo_server("ok").await;
    let env = Env::new(&two_providers(&main.uri(), &spare.uri(), "")).await;
    let mut c = env.controller();
    let saved = || -> Vec<String> {
        let text = std::fs::read_to_string(env.user_dir().join("config.toml")).unwrap();
        let value: toml::Value = toml::from_str(&text).unwrap();
        value["profiles"]["default"]
            .get("chain")
            .and_then(|c| c.as_array())
            .map(|c| c.iter().map(|e| e.as_str().unwrap().to_string()).collect())
            .unwrap_or_default()
    };

    c.type_line("/chain");
    assert!(notices(&c).last().unwrap().contains("only the main model"));

    c.type_line("/chain add spare:a spare:b");
    assert_eq!(saved(), ["spare:a", "spare:b"]);
    assert!(notices(&c).last().unwrap().contains("3. spare:b"));

    c.type_line("/chain add spare:a");
    assert_eq!(saved(), ["spare:a", "spare:b"], "duplicates are dropped");

    c.type_line("/chain remove 2");
    assert_eq!(saved(), ["spare:b"]);

    c.type_line("/chain remove spare:zzz");
    assert!(notices(&c).last().unwrap().contains("is not in the chain"));

    c.type_line("/chain add nowhere:x");
    assert!(notices(&c).last().unwrap().contains("no provider named 'nowhere'"));

    c.type_line("/chain set spare:c main:other");
    assert_eq!(saved(), ["spare:c", "main:other"]);

    c.type_line("/chain clear");
    assert!(saved().is_empty());
}

#[tokio::test]
async fn switching_to_a_smaller_model_compacts_before_the_next_message_goes_out() {
    let mock = echo_server("short reply").await;
    let env = Env::new(&provider_config(&mock.uri(), "context_window = 200000\n")).await;
    let mut c = env.controller();
    for i in 0..6 {
        turn(&mut c, &format!("turn {i}: {}", "words ".repeat(450))).await;
    }
    std::fs::write(
        env.user_dir().join("config.toml"),
        provider_config(&mock.uri(), "context_window = 3000\n"),
    )
    .unwrap();
    c.type_line("/model m");
    assert_eq!(c.app().status.context.map(|(_, w)| w), Some(3000), "the meter follows the smaller window");
    let before = chat_bodies(&mock).await.len();
    turn(&mut c, "the newest question").await;
    let sent = chat_bodies(&mock).await;
    let new: Vec<String> = sent[before..].iter().map(system_text).collect();
    assert!(new[0].starts_with("Summarize the conversation"), "the summary comes first: {new:?}");
    let question = sent[before..]
        .iter()
        .map(|b| serde_json::to_string(b).unwrap())
        .find(|b| b.contains("the newest question") && !b.contains("Summarize the conversation"))
        .expect("the question is sent after the summary");
    assert!(question.contains("Summary of the earlier conversation"), "{question}");
    assert!(question.len() < serde_json::to_string(&sent[before - 1]).unwrap().len(), "the request shrank");
    assert!(notices(&c).iter().any(|n| n.contains("compacting before the next turn")));
}

#[tokio::test]
async fn a_setting_changed_in_zarn_keeps_edits_made_to_the_config_file_meanwhile() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    let path = env.user_dir().join("config.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("\n[general]\nbanner = false\n");
    std::fs::write(&path, text).unwrap();
    c.type_line("/theme light");
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("banner = false"), "the edit made outside zarn survives: {saved}");
    assert!(saved.contains("theme = \"light\""), "{saved}");
}

#[tokio::test]
async fn rank_checks_models_orders_them_and_saves_the_best_as_the_chain() {
    let main = echo_server("I would add them: 5").await;
    let tools = server(|body| {
        let model = body["model"].as_str().unwrap_or_default().to_string();
        let args = if model == "wrong" { serde_json::json!({"a": 1, "b": 1}) } else { serde_json::json!({"a": 2, "b": 3}) };
        (tool_sse("c1", "add", &args), 0)
    })
    .await;
    let broken = missing_model_server().await;
    let config = format!(
        "{}\n[providers.broke]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n",
        two_providers(&main.uri(), &tools.uri(), ""),
        broken.uri()
    );
    let env = Env::new(&config).await;
    let mut c = env.controller();

    c.type_line("/rank save");
    assert!(notices(&c).last().unwrap().contains("run /rank first"));
    c.type_line("/rank nowhere:x");
    assert!(notices(&c).last().unwrap().contains("no provider named 'nowhere'"));

    c.type_line("/rank main:m spare:good spare:wrong broke:gone");
    assert!(notices(&c).last().unwrap().contains("checking 4 models"));
    wait_for(&mut c, std::time::Duration::from_secs(20), |c| notices(c).iter().any(|n| n.contains("answered. /rank save"))).await;
    let table = notices(&c).into_iter().find(|n| n.contains("answered. /rank save")).unwrap();
    let rows: Vec<&str> = table.lines().skip(1).take(4).collect();
    assert!(rows[0].contains("spare:good") && rows[0].contains("works, calls tools"), "{table}");
    assert!(rows[3].contains("broke:gone"), "{table}");
    assert!(table.contains("did not call the tool"), "{table}");
    assert!(table.contains("3 of 4 answered"), "{table}");

    let probe = &chat_bodies(&tools).await[0];
    assert_eq!(probe["max_tokens"], 256, "probes are small");
    assert_eq!(probe["tools"][0]["function"]["name"], "add");

    c.type_line("/rank save 2");
    let shown = notices(&c).last().unwrap().clone();
    assert!(shown.contains("2. spare:good"), "{shown}");
    assert!(!shown.contains("broke:gone") && !shown.contains("4. "), "{shown}");
}

#[tokio::test]
async fn model_accepts_provider_colon_model_and_switches_provider_when_needed() {
    let main = echo_server("from main").await;
    let spare = echo_server("from spare").await;
    let env = Env::new(&two_providers(&main.uri(), &spare.uri(), "")).await;
    let mut c = env.controller();
    let profile = || {
        let text = std::fs::read_to_string(env.user_dir().join("config.toml")).unwrap();
        let value: toml::Value = toml::from_str(&text).unwrap();
        let p = &value["profiles"]["default"];
        (p["provider"].as_str().unwrap().to_string(), p["model"].as_str().unwrap().to_string())
    };

    c.type_line("/model main:m2");
    assert_eq!(profile(), ("main".to_string(), "m2".to_string()), "the prefix of the current provider is dropped");
    c.type_line("/model spare:s1");
    assert_eq!(profile(), ("spare".to_string(), "s1".to_string()));
    turn(&mut c, "hi").await;
    assert_eq!(chat_bodies(&spare).await[0]["model"], "s1");
    assert_eq!(assistant(&c).last().map(String::as_str), Some("from spare"));
}

#[tokio::test]
async fn a_long_rate_limit_hands_over_at_once_when_the_chain_has_another_model() {
    let limited = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).set_body_string(r#"{"error":{"message":"Rate limit reached. Please retry in 30s."}}"#))
        .mount(&limited)
        .await;
    let spare = echo_server("from the spare").await;
    let env = Env::new(&two_providers(&limited.uri(), &spare.uri(), "chain = [\"spare:backup\"]")).await;
    let mut c = env.controller();
    let started = std::time::Instant::now();
    turn(&mut c, "hello").await;
    assert_eq!(assistant(&c).last().map(String::as_str), Some("from the spare"));
    assert!(started.elapsed() < std::time::Duration::from_secs(8), "took {:?}", started.elapsed());
    assert_eq!(limited.received_requests().await.unwrap().len(), 1, "no waiting on the limited model");
}
