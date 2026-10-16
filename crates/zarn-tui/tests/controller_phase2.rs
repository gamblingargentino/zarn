mod support;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;
use support::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zarn_tui::state::{Block, Overlay};

async fn lists(s: &MockServer) -> usize {
    s.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "GET")
        .count()
}

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

async fn chat_and_models(models_body: &str) -> MockServer {
    let server = server(|body| {
        if system_text(body).starts_with("Write a title") {
            (sse("Title"), 0)
        } else {
            (sse("ok"), 0)
        }
    })
    .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(models_body.to_string()))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn slash_models_lists_each_provider_and_keeps_a_failure_on_its_own_row() {
    let good = chat_and_models("{\"data\":[{\"id\":\"m\",\"context_window\":64000},{\"id\":\"m-large\"}]}").await;
    let broken = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&broken)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&broken)
        .await;
    let env = Env::new(&format!(
        "{}\n[providers.broken]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n",
        provider_config(&good.uri(), ""),
        broken.uri()
    ))
    .await;
    let mut c = env.controller();
    c.type_line("/models");
    let count = notices(&c).len();
    wait_for(&mut c, Duration::from_secs(5), |c| notices(c).len() > count).await;
    let report = notices(&c).last().unwrap().clone();
    assert!(report.contains("mock (openai-compat) · fetched"), "{report}");
    assert!(report.contains("m (64k ctx), m-large"), "{report}");
    assert!(report.contains("broken (openai-compat)  FAILED"), "{report}");
    let before = lists(&good).await;
    c.type_line("/models");
    let count = notices(&c).len();
    wait_for(&mut c, Duration::from_secs(5), |c| notices(c).len() > count).await;
    assert_eq!(lists(&good).await, before, "the cache is used");
    c.type_line("/models refresh");
    let count = notices(&c).len();
    wait_for(&mut c, Duration::from_secs(5), |c| notices(c).len() > count).await;
    assert!(lists(&good).await > before, "refresh asks again");
}

#[tokio::test]
async fn the_model_picker_shows_discovered_models_and_accepts_any_name() {
    let mock = chat_and_models("{\"data\":[{\"id\":\"m\",\"context_window\":64000},{\"id\":\"m-large\",\"context_window\":256000}]}").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    c.type_line("/model");
    wait_for(&mut c, Duration::from_secs(5), |c| match &c.app().overlay {
        Some(Overlay::Pick(p, _)) => p.choices.iter().any(|ch| ch.value == "m-large"),
        _ => false,
    })
    .await;
    match &c.app().overlay {
        Some(Overlay::Pick(picker, _)) => {
            let large = picker.choices.iter().find(|ch| ch.value == "m-large").unwrap();
            assert_eq!(large.detail, "256k ctx");
            assert!(picker.allow_custom);
        }
        _ => panic!("expected the model picker"),
    }
    for ch in "unlisted-model".chars() {
        c.key(key(ch));
    }
    c.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(c.app().status.model, "unlisted-model");
}

fn priced(mock: &str, limits: &str) -> String {
    format!(
        "[pricing.m]\ninput = 1000.0\noutput = 1000.0\n\n[limits]\n{limits}\n\n{}",
        provider_config(mock, "")
    )
}

#[tokio::test]
async fn a_session_cap_warns_at_eighty_percent_and_then_stops_naming_the_cap() {
    let mock = echo_server("ok").await;
    let env = Env::new(&priced(&mock.uri(), "session_usd = 0.03")).await;
    let mut c = env.controller();
    turn(&mut c, "one").await;
    turn(&mut c, "two").await;
    assert!(
        notices(&c).iter().any(|n| n.contains("80% of the session spend cap")),
        "{:?}",
        notices(&c)
    );
    turn(&mut c, "three").await;
    let stop = notices(&c)
        .into_iter()
        .find(|n| n.starts_with("stopped: the session spend cap of $0.03"))
        .expect("a stop notice");
    assert!(stop.contains("session_usd"));
    let sent = chat_bodies(&mock).await.len();
    turn(&mut c, "four").await;
    assert_eq!(chat_bodies(&mock).await.len(), sent, "no request after the cap");
}

#[tokio::test]
async fn the_daily_total_is_kept_locally_without_keys_and_enforced() {
    let mock = echo_server("ok").await;
    let env = Env::new(&priced(&mock.uri(), "day_usd = 0.02")).await;
    env.store.save("mock", "test-key-not-real-ledger").unwrap();
    let mut c = env.controller();
    turn(&mut c, "one").await;
    let ledger = env.root().join("spend").join(format!("{}.json", zarn_config::spend::today()));
    let body = std::fs::read_to_string(&ledger).unwrap();
    assert!(body.contains("\"usd\":0.012"), "{body}");
    assert!(!body.contains("not-real"));
    turn(&mut c, "two").await;
    assert!(notices(&c).iter().any(|n| n.contains("daily spend cap") && n.contains("day_usd")));

    let mut fresh = env.controller();
    turn(&mut fresh, "three").await;
    assert!(notices(&fresh).iter().any(|n| n.starts_with("stopped: the daily spend cap")));
}

#[tokio::test]
async fn an_unpriced_model_is_never_treated_as_free_under_a_cap() {
    let mock = echo_server("ok").await;
    let env = Env::new(&format!(
        "[limits]\nsession_usd = 0.01\n\n{}",
        provider_config(&mock.uri(), "")
    ))
    .await;
    let mut c = env.controller();
    turn(&mut c, "one").await;
    turn(&mut c, "two").await;
    let notes: Vec<String> = notices(&c)
        .into_iter()
        .filter(|n| n.contains("spend caps cannot be enforced for m"))
        .collect();
    assert_eq!(notes.len(), 1);
}

#[tokio::test]
async fn limits_can_be_set_from_the_chat() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    c.type_line("/limits session 2.5");
    let saved = std::fs::read_to_string(env.user_dir().join("config.toml")).unwrap();
    assert!(saved.contains("[limits]\nsession_usd = 2.5"), "{saved}");
    c.type_line("/limits session off");
    let saved = std::fs::read_to_string(env.user_dir().join("config.toml")).unwrap();
    assert!(!saved.contains("session_usd"));
    c.type_line("/limits day -3");
    assert!(notices(&c).last().unwrap().contains("not a positive dollar amount"));
}

#[tokio::test]
async fn ctrl_p_switches_the_profile_for_the_session() {
    let mock = echo_server("ok").await;
    let env = Env::new(&format!(
        "{}\n[profiles.fast]\nprovider = \"mock\"\nmodel = \"fast-model\"\n",
        provider_config(&mock.uri(), "")
    ))
    .await;
    let mut c = env.controller();
    c.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    match &c.app().overlay {
        Some(Overlay::Pick(picker, _)) => {
            assert!(picker.choices.iter().any(|ch| ch.value == "fast"));
        }
        _ => panic!("expected the profile picker"),
    }
    for ch in "fast".chars() {
        c.key(key(ch));
    }
    c.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    turn(&mut c, "hi").await;
    assert_eq!(chat_bodies(&mock).await[0]["model"], "fast-model");
}

#[tokio::test]
async fn compare_shows_both_answers_with_latency_and_tokens_and_isolates_errors() {
    let a = echo_server("answer from a").await;
    let b = server(|_| (sse("answer from b"), 150)).await;
    let broken = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&broken)
        .await;
    let env = Env::new(&format!(
        "{}\n[providers.b]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[providers.broken]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n",
        provider_config(&a.uri(), ""),
        b.uri(),
        broken.uri()
    ))
    .await;
    let mut c = env.controller();
    let compares = |c: &zarn_tui::Controller| {
        c.app().blocks.iter().filter(|b| matches!(b, Block::Compare(_))).count()
    };
    c.type_line("/compare mock b say hello");
    wait_for(&mut c, Duration::from_secs(5), |c| compares(c) == 1).await;
    let sides = c
        .app()
        .blocks
        .iter()
        .find_map(|bl| match bl {
            Block::Compare(sides) => Some(sides.clone()),
            _ => None,
        })
        .expect("a compare block");
    assert_eq!(sides[0].text, "answer from a");
    assert_eq!(sides[1].text, "answer from b");
    assert!(sides[1].meta.contains(" s · 10 in / 2 out"), "{}", sides[1].meta);
    assert!(bodies(&a).await[0]["tools"].is_null());

    c.type_line("/compare mock broken hi");
    wait_for(&mut c, Duration::from_secs(5), |c| compares(c) == 2).await;
    let last = c
        .app()
        .blocks
        .iter()
        .rev()
        .find_map(|bl| match bl {
            Block::Compare(sides) => Some(sides.clone()),
            _ => None,
        })
        .unwrap();
    assert!(!last[0].failed);
    assert!(last[1].failed);

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
    terminal.draw(|f| zarn_tui::render::draw(f, c.app())).unwrap();
    let screen: String = (0..30)
        .map(|y| {
            (0..80)
                .map(|x| terminal.backend().buffer().cell((x, y)).unwrap().symbol().to_string())
                .collect::<String>()
                + "\n"
        })
        .collect();
    assert!(screen.contains("answer from a") && screen.contains(" │ "));
}

fn broken_tool_sse() -> String {
    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"x\",\"function\":{\"name\":\"read\",\"arguments\":\"path=foo (not json\"}}]}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n".to_string()
}

#[tokio::test]
async fn three_unparseable_tool_calls_offer_prompted_mode_and_write_it_through_ops() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = calls.clone();
    let mock = server(move |body| {
        if system_text(body).starts_with("Write a title") {
            return (sse("Title"), 0);
        }
        if counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 3 {
            (broken_tool_sse(), 0)
        } else {
            (sse("gave up"), 0)
        }
    })
    .await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    turn(&mut c, "read foo").await;
    match &c.app().overlay {
        Some(Overlay::Confirm(text, _)) => assert!(text.contains("prompted")),
        _ => panic!("expected the prompted-mode offer"),
    }
    c.key(key('y'));
    let saved = std::fs::read_to_string(env.user_dir().join("config.toml")).unwrap();
    assert!(saved.contains("tools = \"prompted\""), "{saved}");
}

#[tokio::test]
async fn ollama_reports_its_real_context_window_for_the_meter() {
    let ollama = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "{\"model_info\":{\"general.architecture\":\"qwen3\",\"qwen3.context_length\":40960}}",
        ))
        .mount(&ollama)
        .await;
    let env = Env::new(&format!(
        "[providers.local]\nkind = \"ollama\"\nbase_url = \"{}\"\n\n[profiles.default]\nprovider = \"local\"\nmodel = \"qwen3:8b\"\n",
        ollama.uri()
    ))
    .await;
    let mut c = env.controller();
    wait_for(&mut c, Duration::from_secs(3), |c| {
        c.app().status.context.map(|(_, w)| w) == Some(40_960)
    })
    .await;
    assert_eq!(c.app().status.context.map(|(_, w)| w), Some(40_960));
}
