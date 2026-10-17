mod support;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;
use support::*;
use zarn_tui::state::Block;

#[tokio::test]
async fn messages_sent_while_streaming_are_queued_and_sent_in_order() {
    let mock = server(|body| {
        if system_text(body).starts_with("Write a title") {
            return (sse("Title"), 0);
        }
        let last = body["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        (sse(&format!("re: {last}")), 300)
    })
    .await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();

    c.type_line("one");
    c.type_line("two");
    c.type_line("three");
    assert!(c.app().streaming);
    assert_eq!(c.app().queue, vec!["two", "three"]);
    assert!(c.app().blocks.iter().any(|b| matches!(b, Block::Queued(t) if t == "two")));

    c.settle(Duration::from_secs(10)).await;
    assert_eq!(assistant(&c), vec!["re: one", "re: two", "re: three"]);
    assert!(c.app().queue.is_empty());
    let users: Vec<String> = chat_bodies(&mock)
        .await
        .iter()
        .map(|b| b["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(users, vec!["one", "two", "three"]);
}

#[tokio::test]
async fn esc_cancels_the_turn_but_keeps_the_queue_until_a_second_esc() {
    let mock = server(|_| (sse("slow"), 2_000)).await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();

    c.type_line("one");
    c.type_line("two");
    c.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    c.settle(Duration::from_secs(5)).await;
    assert!(!c.app().streaming);
    assert_eq!(c.app().queue, vec!["two"]);
    assert!(notices(&c).iter().any(|n| n.contains("1 queued message kept")));
    assert_eq!(chat_bodies(&mock).await.len(), 1);

    c.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(c.app().queue.is_empty());
    c.settle(Duration::from_millis(300)).await;
    assert_eq!(chat_bodies(&mock).await.len(), 1);
}

#[tokio::test]
async fn a_full_context_is_compacted_before_the_next_turn_and_said_so() {
    let mock = server(|body| {
        let system = system_text(body);
        if system.starts_with("Summarize the conversation") {
            (sse("SUMMARY OF EARLIER TURNS"), 0)
        } else {
            (sse("ok"), 0)
        }
    })
    .await;
    let env = Env::new(&provider_config(&mock.uri(), "context_window = 200\n")).await;
    let mut c = env.controller();

    for i in 0..4 {
        turn(&mut c, &format!("question number {i} {}", "padding ".repeat(20))).await;
    }
    let summaries = |bodies: &[serde_json::Value]| {
        bodies
            .iter()
            .filter(|b| system_text(b).starts_with("Summarize the conversation"))
            .count()
    };
    assert_eq!(summaries(&chat_bodies(&mock).await), 0, "too short to split, so left alone");
    assert!(!notices(&c).iter().any(|n| n.contains("compacted")));

    turn(&mut c, &format!("question number 4 {}", "padding ".repeat(20))).await;
    assert_eq!(summaries(&chat_bodies(&mock).await), 1);
    let said = notices(&c);
    assert!(said.iter().any(|n| n.contains("full; compacting before the next turn")), "{said:?}");
    assert!(said.iter().any(|n| n.starts_with("compacted")));
    assert!(c.messages().iter().any(|m| m.text().contains("SUMMARY OF EARLIER TURNS")));
}

#[tokio::test]
async fn auto_compaction_can_be_turned_off() {
    let mock = echo_server("ok").await;
    let env = Env::new(&format!(
        "[general]\nauto_compact = false\n\n{}",
        provider_config(&mock.uri(), "context_window = 400\n")
    ))
    .await;
    let mut c = env.controller();
    for i in 0..6 {
        turn(&mut c, &format!("q{i} {}", "padding ".repeat(20))).await;
    }
    assert!(!notices(&c).iter().any(|n| n.contains("compact")));
    assert_eq!(chat_bodies(&mock).await.len(), 6);
}

#[tokio::test]
async fn the_status_line_carries_a_context_meter_and_context_breaks_it_down() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "context_window = 100000\n")).await;
    std::fs::write(env.project().join("ZARN.md"), "always run the tests").unwrap();
    let mut c = env.controller();
    turn(&mut c, "hello").await;
    let (used, window) = c.app().status.context.unwrap();
    assert_eq!(window, 100_000);
    assert!(used > 0);
    c.type_line("/context");
    let report = notices(&c).last().unwrap().clone();
    for part in ["system prompt", "memory", "skills index", "tools", "messages", "of 100000"] {
        assert!(report.contains(part), "{report}");
    }
}

#[tokio::test]
async fn a_changed_zarn_md_is_used_on_the_next_turn_without_a_restart() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    std::fs::write(env.project().join("ZARN.md"), "use tabs").unwrap();
    let mut c = env.controller();
    turn(&mut c, "first").await;
    std::fs::write(env.project().join("ZARN.md"), "use spaces").unwrap();
    std::fs::create_dir_all(env.user_dir().join("skills/lint")).unwrap();
    std::fs::write(
        env.user_dir().join("skills/lint/SKILL.md"),
        "---\nname: lint\ndescription: run the linter\n---\nbody",
    )
    .unwrap();
    turn(&mut c, "second").await;
    let sent = chat_bodies(&mock).await;
    assert!(system_text(&sent[0]).contains("use tabs"));
    assert!(system_text(&sent[1]).contains("use spaces"));
    assert!(system_text(&sent[1]).contains("lint: run the linter"));
    assert!(!system_text(&sent[1]).contains("use tabs"));
}

#[tokio::test]
async fn a_fallback_switch_appears_once_in_the_scrollback() {
    let limited = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(wiremock::ResponseTemplate::new(429))
        .mount(&limited)
        .await;
    let backup = echo_server("from backup").await;
    let env = Env::new(&format!(
        "[providers.anthropic]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[providers.openai]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[profiles.default]\nprovider = \"anthropic\"\nmodel = \"m\"\nfallback = [\"openai\"]\n",
        limited.uri(),
        backup.uri()
    ))
    .await;
    let mut c = env.controller();
    turn(&mut c, "one").await;
    turn(&mut c, "two").await;
    turn(&mut c, "three").await;
    let switched: Vec<String> = notices(&c)
        .into_iter()
        .filter(|n| n.contains("anthropic rate-limited, answered by openai"))
        .collect();
    assert_eq!(switched.len(), 2, "{switched:?}");
    assert!(switched[1].ends_with("anthropic keeps failing, so it sits out for 5 min"), "{switched:?}");
}

#[tokio::test]
async fn a_priced_model_shows_dollars_and_an_unpriced_one_says_so() {
    let mock = echo_server("ok").await;
    let env = Env::new(&format!(
        "[pricing.m]\ninput = 1000.0\noutput = 1000.0\n\n{}",
        provider_config(&mock.uri(), "")
    ))
    .await;
    let mut c = env.controller();
    turn(&mut c, "hello").await;
    assert!((c.app().status.cost.unwrap() - 0.012).abs() < 1e-9);
    c.type_line("/cost");
    assert!(notices(&c).last().unwrap().contains("$0.0120"));

    let env2 = {
        drop(c);
        drop(env);
        Env::new(&provider_config(&mock.uri(), "")).await
    };
    let mut c = env2.controller();
    turn(&mut c, "hello").await;
    assert!(c.app().status.cost.is_none());
    c.type_line("/cost");
    assert!(notices(&c).last().unwrap().contains("no rate configured"));
}
