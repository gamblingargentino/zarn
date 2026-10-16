mod support;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use support::*;
use zarn_tui::state::Overlay;

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn shift_tab() -> KeyEvent {
    KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)
}

fn tool_names(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn last_tool_result(body: &Value) -> String {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "tool")
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default()
        .to_string()
}

fn status_line(c: &zarn_tui::Controller) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 20)).unwrap();
    terminal
        .draw(|frame| zarn_tui::render::draw(frame, c.app()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..20)
        .map(|y| {
            (0..140)
                .map(|x| buffer.cell((x, y)).map(|c| c.symbol().to_string()).unwrap_or_default())
                .collect::<String>()
        })
        .find(|line| line.contains(" mode  ·"))
        .unwrap_or_default()
}

#[tokio::test]
async fn shift_tab_cycles_modes_shows_them_and_persists_the_choice() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    assert!(status_line(&c).contains("default mode"));

    c.key(shift_tab());
    assert_eq!(c.app().status.mode, "plan");
    assert!(status_line(&c).contains("plan mode"));
    let saved = std::fs::read_to_string(env.user_dir().join("config.toml")).unwrap();
    assert!(saved.contains("mode = \"plan\""), "{saved}");

    c.key(shift_tab());
    assert_eq!(c.app().status.mode, "accept-edits");
    c.key(shift_tab());
    assert_eq!(c.app().status.mode, "default");

    c.type_line("/mode plan");
    assert_eq!(c.app().status.mode, "plan");
    let reopened = env.controller();
    assert_eq!(reopened.app().status.mode, "plan");
}

#[tokio::test]
async fn mode_with_no_argument_opens_a_picker() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    c.type_line("/mode");
    match &c.app().overlay {
        Some(Overlay::Pick(picker, _)) => {
            let values: Vec<&str> = picker.choices.iter().map(|c| c.value.as_str()).collect();
            assert_eq!(values, vec!["default", "plan", "accept-edits", "bypass"]);
        }
        _ => panic!("expected the mode picker"),
    }
    for ch in "accept".chars() {
        c.key(key(ch));
    }
    c.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(c.app().status.mode, "accept-edits");
}

#[tokio::test]
async fn bypass_needs_a_confirmation_that_names_the_risk_and_is_not_restored() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    c.type_line("/mode bypass");
    match &c.app().overlay {
        Some(Overlay::Confirm(text, _)) => assert!(text.contains("without asking")),
        _ => panic!("expected a confirmation"),
    }
    assert_eq!(c.app().status.mode, "default");
    c.key(key('n'));
    assert_eq!(c.app().status.mode, "default");

    c.type_line("/mode bypass");
    c.key(key('y'));
    assert_eq!(c.app().status.mode, "bypass");
    let saved = std::fs::read_to_string(env.user_dir().join("config.toml")).unwrap();
    assert!(!saved.contains("bypass"));

    c.key(shift_tab());
    assert_eq!(c.app().status.mode, "default");
}

#[tokio::test]
async fn a_custom_mode_restricts_tools_overlays_permissions_and_switches_model() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    std::fs::create_dir_all(env.project().join(".zarn/modes")).unwrap();
    std::fs::write(
        env.project().join(".zarn/modes/review.md"),
        "---\nname: review\ndescription: read-only review\ntools: read, glob, grep, edit\npermissions: edit=deny\nmodel: review-model\n---\nReview carefully.\n",
    )
    .unwrap();
    let mut c = env.controller();
    c.type_line("/mode review");
    turn(&mut c, "look").await;
    c.type_line("/mode default");
    turn(&mut c, "again").await;

    let sent = main_bodies(&mock).await;
    let mut in_review = tool_names(&sent[0]);
    in_review.sort();
    assert_eq!(in_review, vec!["edit", "glob", "grep", "read"]);
    assert_eq!(sent[0]["model"], "review-model");
    assert!(system_text(&sent[0]).contains("Review carefully."));

    let after = tool_names(&sent[1]);
    assert!(after.contains(&"bash".to_string()) && after.contains(&"write".to_string()));
    assert_eq!(sent[1]["model"], "m");
    assert!(!system_text(&sent[1]).contains("Review carefully."));
}

#[tokio::test]
async fn a_mode_permission_overlay_denies_and_is_removed_on_exit() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mock = server(move |body| {
        if role_of(body) == "title" {
            return (sse("Title"), 0);
        }
        let n = counter.fetch_add(1, Ordering::SeqCst);
        match n {
            0 | 2 => (tool_sse(&format!("c{n}"), "write", &json!({ "path": "x.txt", "content": "hi" })), 0),
            _ => (sse("done"), 0),
        }
    })
    .await;
    let env = Env::new(&format!(
        "[permissions]\nwrite = \"allow\"\n\n{}",
        provider_config(&mock.uri(), "")
    ))
    .await;
    std::fs::create_dir_all(env.project().join(".zarn/modes")).unwrap();
    std::fs::write(
        env.project().join(".zarn/modes/locked.md"),
        "---\nname: locked\ndescription: no writes\npermissions: write=deny\n---\n",
    )
    .unwrap();
    let mut c = env.controller();
    c.type_line("/mode locked");
    turn(&mut c, "write it").await;
    assert!(!env.project().join("x.txt").exists());
    c.type_line("/mode default");
    turn(&mut c, "write it now").await;
    assert!(env.project().join("x.txt").exists());
    let sent = main_bodies(&mock).await;
    assert!(last_tool_result(&sent[1]).contains("denied in locked mode"));
}

#[tokio::test]
async fn plan_mode_denies_write_shows_the_plan_and_continues_after_approval() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mock = server(move |body| {
        if role_of(body) == "title" {
            return (sse("Title"), 0);
        }
        let n = counter.fetch_add(1, Ordering::SeqCst);
        match n {
            0 => (tool_sse("a", "write", &json!({ "path": "out.txt", "content": "early" })), 0),
            1 => (tool_sse("b", "exit_plan", &json!({ "plan": "1. create out.txt" })), 0),
            2 => (tool_sse("c", "write", &json!({ "path": "out.txt", "content": "approved" })), 0),
            _ => (sse("finished"), 0),
        }
    })
    .await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    c.type_line("/mode plan");
    c.type_line("make out.txt");
    c.settle(Duration::from_secs(10)).await;

    let review = c.app().plan_review.as_ref().expect("plan shown for approval");
    assert_eq!(review.plan, "1. create out.txt");
    assert!(!env.project().join("out.txt").exists());

    c.key(key('a'));
    c.settle(Duration::from_secs(10)).await;
    assert_eq!(c.app().status.mode, "accept-edits");
    assert_eq!(
        std::fs::read_to_string(env.project().join("out.txt")).unwrap(),
        "approved"
    );
    assert!(c.app().pending.is_none(), "accept-edits must not prompt for write");
    let sent = main_bodies(&mock).await;
    assert!(last_tool_result(&sent[1]).contains("plan mode"));
    assert!(last_tool_result(&sent[2]).contains("approved the plan"));
    assert!(assistant(&c).iter().any(|t| t == "finished"));
}

#[tokio::test]
async fn a_rejected_plan_keeps_plan_mode_and_returns_the_feedback() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mock = server(move |body| {
        if role_of(body) == "title" {
            return (sse("Title"), 0);
        }
        let n = counter.fetch_add(1, Ordering::SeqCst);
        match n {
            0 => (tool_sse("b", "exit_plan", &json!({ "plan": "1. guess" })), 0),
            _ => (sse("revised"), 0),
        }
    })
    .await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    c.type_line("/mode plan");
    c.type_line("plan it");
    c.settle(Duration::from_secs(10)).await;
    c.key(key('r'));
    for ch in "read the tests first".chars() {
        c.key(key(ch));
    }
    c.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    c.settle(Duration::from_secs(10)).await;
    assert_eq!(c.app().status.mode, "plan");
    let sent = main_bodies(&mock).await;
    assert!(last_tool_result(&sent[1]).contains("read the tests first"));
}

async fn main_bodies(server: &wiremock::MockServer) -> Vec<Value> {
    bodies(server)
        .await
        .into_iter()
        .filter(|b| role_of(b) == "main")
        .collect()
}

fn role_of(body: &Value) -> &'static str {
    let system = system_text(body);
    if system.starts_with("Summarize the conversation") {
        "compact"
    } else if system.starts_with("Write a title") {
        "title"
    } else if system.starts_with("be a helper") {
        "subagent"
    } else {
        "main"
    }
}

async fn routing_run(enabled: bool) -> Vec<(String, Vec<&'static str>)> {
    let main_calls = Arc::new(AtomicUsize::new(0));
    let counter = main_calls.clone();
    let main = server(move |body| {
        if role_of(body) != "main" {
            return (sse("Side Answer"), 0);
        }
        match counter.fetch_add(1, Ordering::SeqCst) {
            0 => (tool_sse("t", "task", &json!({ "agent": "helper", "prompt": "help" })), 0),
            _ => (sse("main answer"), 0),
        }
    })
    .await;
    let sub = server(|_| (sse("helped"), 0)).await;
    let compact = server(|_| (sse("Summary"), 0)).await;
    let title = server(|_| (sse("Routed Title"), 0)).await;
    let config = format!(
        "[providers.main]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[providers.sub]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[providers.compactor]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[providers.titler]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[profiles.default]\nprovider = \"main\"\nmodel = \"m\"\n\n[routing]\nenabled = {enabled}\n\n[routing.roles.subagent]\nprovider = \"sub\"\nmodel = \"sub-model\"\n\n[routing.roles.compact]\nprovider = \"compactor\"\nmodel = \"compact-model\"\n\n[routing.roles.title]\nprovider = \"titler\"\nmodel = \"title-model\"\n",
        main.uri(),
        sub.uri(),
        compact.uri(),
        title.uri()
    );
    let env = Env::new(&config).await;
    std::fs::create_dir_all(env.project().join(".zarn/agents")).unwrap();
    std::fs::write(
        env.project().join(".zarn/agents/helper.md"),
        "---\nname: helper\ndescription: helps\n---\nbe a helper",
    )
    .unwrap();
    let mut c = env.controller();
    c.type_line("/mode bypass");
    c.key(key('y'));
    for i in 0..5 {
        turn(&mut c, &format!("question {i}")).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    c.settle(Duration::from_secs(2)).await;
    c.type_line("/compact");
    c.settle(Duration::from_secs(10)).await;

    let mut out = Vec::new();
    for (name, server) in [("main", &main), ("sub", &sub), ("compact", &compact), ("title", &title)] {
        let roles = bodies(server).await.iter().map(role_of).collect::<Vec<_>>();
        out.push((name.to_string(), roles));
    }
    out
}

#[tokio::test]
async fn with_routing_each_role_reaches_its_own_server() {
    let seen = routing_run(true).await;
    let get = |name: &str| seen.iter().find(|(n, _)| n == name).unwrap().1.clone();
    assert!(get("main").iter().all(|r| *r == "main"), "{seen:?}");
    assert!(!get("main").is_empty());
    assert_eq!(get("sub"), vec!["subagent"]);
    assert_eq!(get("compact"), vec!["compact"]);
    assert_eq!(get("title"), vec!["title"]);
}

#[tokio::test]
async fn without_routing_every_role_goes_to_main_even_when_configured() {
    let seen = routing_run(false).await;
    let get = |name: &str| seen.iter().find(|(n, _)| n == name).unwrap().1.clone();
    let main = get("main");
    for role in ["main", "subagent", "compact", "title"] {
        assert!(main.contains(&role), "{role} missing from {main:?}");
    }
    assert!(get("sub").is_empty() && get("compact").is_empty() && get("title").is_empty());
}

#[tokio::test]
async fn a_role_without_a_key_falls_back_to_main_with_one_notice_and_no_network() {
    let main = echo_server("ok").await;
    let env = Env::new(&format!(
        "[providers.claude]\nkind = \"anthropic\"\n\n{}\n[routing]\nenabled = true\n\n[routing.roles.title]\nprovider = \"claude\"\nmodel = \"claude-haiku-5-5\"\n",
        provider_config(&main.uri(), "")
    ))
    .await;
    let mut c = env.controller();
    turn(&mut c, "hello").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    c.settle(Duration::from_secs(2)).await;
    let said: Vec<String> = notices(&c)
        .into_iter()
        .filter(|n| n.contains("routing role title") && n.contains("claude has no API key"))
        .collect();
    assert_eq!(said.len(), 1, "{:?}", notices(&c));
    assert_eq!(bodies(&main).await.iter().filter(|b| role_of(b) == "title").count(), 1);
}

#[tokio::test]
async fn a_role_whose_provider_fails_transiently_is_answered_by_main_with_a_notice() {
    let main = echo_server("Main Title").await;
    let limited = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(wiremock::ResponseTemplate::new(429))
        .mount(&limited)
        .await;
    let env = Env::new(&format!(
        "[providers.groq]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n{}\n[routing]\nenabled = true\n\n[routing.roles.title]\nprovider = \"groq\"\nmodel = \"llama\"\n",
        limited.uri(),
        provider_config(&main.uri(), "")
    ))
    .await;
    let mut c = env.controller();
    turn(&mut c, "hello").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    c.settle(Duration::from_secs(2)).await;
    assert!(
        notices(&c).iter().any(|n| n.contains("groq rate-limited, answered by mock")),
        "{:?}",
        notices(&c)
    );
}

#[tokio::test]
async fn a_title_is_generated_without_blocking_and_shown_by_resume() {
    let mock = server(|body| {
        if role_of(body) == "title" {
            (sse("Fixing The Parser"), 1_500)
        } else {
            (sse("sure"), 0)
        }
    })
    .await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    c.type_line("fix the parser please");
    c.settle(Duration::from_secs(1)).await;
    assert!(!c.app().streaming, "the title request must not hold the turn");
    turn(&mut c, "and the lexer").await;
    tokio::time::sleep(Duration::from_millis(1_600)).await;
    c.settle(Duration::from_secs(2)).await;

    let root = env.root().join("data");
    let session = zarn_core::session::list(&root, &env.project().canonicalize().unwrap())
        .into_iter()
        .chain(zarn_core::session::list(&root, &env.project()))
        .next()
        .expect("a session file");
    let (meta, _) = zarn_core::session::read(&session).unwrap();
    assert_eq!(meta.title.as_deref(), Some("Fixing The Parser"));

    let mut fresh = env.controller();
    fresh.type_line("/resume");
    match &fresh.app().overlay {
        Some(Overlay::Pick(picker, _)) => {
            assert!(picker.choices.iter().any(|c| c.label.contains("Fixing The Parser")))
        }
        _ => panic!("expected the resume picker"),
    }
    let titles = bodies(&mock).await.iter().filter(|b| role_of(b) == "title").count();
    assert_eq!(titles, 1, "one title per session");
}
