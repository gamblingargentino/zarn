#![cfg(unix)]

mod support;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use support::*;
use zarn_tui::state::{Mode, Overlay};

fn code(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn click_on(c: &mut zarn_tui::Controller, text: &str) {
    let lines = screen(c, 100, 30);
    let (row, line) = lines
        .iter()
        .enumerate()
        .find(|(_, l)| l.contains(text))
        .unwrap_or_else(|| panic!("'{text}' is not on screen:\n{}", lines.join("\n")));
    let column = line[..line.find(text).unwrap()].chars().count() as u16;
    c.mouse(mouse(MouseEventKind::Down(MouseButton::Left), column, row as u16));
}

fn scripted(steps: Vec<String>) -> impl Fn(&serde_json::Value) -> (String, u64) + Send + Sync + 'static {
    let n = Arc::new(AtomicUsize::new(0));
    move |body| {
        if system_text(body).starts_with("Write a title") {
            return (sse("Title"), 0);
        }
        let i = n.fetch_add(1, Ordering::SeqCst);
        (steps.get(i).cloned().unwrap_or_else(|| sse("done")), 0)
    }
}

#[tokio::test]
async fn the_permission_prompt_is_a_menu_for_arrows_enter_and_clicks() {
    let write = |path: &str| tool_sse("w", "write", &json!({ "path": path, "content": "x" }));
    let mock = server(scripted(vec![write("notes.md"), sse("ok"), write("other.md"), sse("ok")])).await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    c.type_line("write notes");
    c.settle(Duration::from_secs(5)).await;
    assert!(c.app().pending.is_some());
    assert!(screen(&c, 100, 30).join("\n").contains("› 1. Yes, once [y]"));
    c.key(code(KeyCode::Down));
    c.key(code(KeyCode::Down));
    assert!(screen(&c, 100, 30).join("\n").contains("› 3. Yes, and save the rule for this project [p]"));
    c.key(code(KeyCode::Enter));
    c.settle(Duration::from_secs(5)).await;
    let project = std::fs::read_to_string(env.project().join(".zarn/config.toml")).unwrap();
    assert!(project.contains("allow = [\"write(notes.md)\"]"), "{project}");

    c.type_line("write the other one");
    c.settle(Duration::from_secs(5)).await;
    assert!(c.app().pending.is_some());
    click_on(&mut c, "6. No [n]");
    c.settle(Duration::from_secs(5)).await;
    assert!(c.app().pending.is_none());
    assert!(!env.project().join("other.md").exists());
}

#[tokio::test]
async fn the_wheel_scrolls_the_conversation_while_arrows_recall_prompts() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    assert!(c.mouse_capture());
    turn(&mut c, "first").await;
    turn(&mut c, "second").await;
    c.mouse(mouse(MouseEventKind::ScrollUp, 10, 5));
    assert_eq!(c.app().scroll_up, 3);
    assert!(c.app().input.is_empty(), "the wheel never touches the prompt");
    c.mouse(mouse(MouseEventKind::ScrollDown, 10, 5));
    assert_eq!(c.app().scroll_up, 0);
    c.key(code(KeyCode::Up));
    assert_eq!(c.app().input, "second");
    c.key(code(KeyCode::Up));
    assert_eq!(c.app().input, "first");
    c.key(code(KeyCode::Down));
    c.key(code(KeyCode::Down));
    assert!(c.app().input.is_empty());
}

#[tokio::test]
async fn commands_complete_their_arguments_and_show_their_syntax() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    c.app_mut().set_input("/mode ");
    let shown = screen(&c, 100, 30).join("\n");
    assert!(shown.contains("plan") && shown.contains("accept-edits"), "{shown}");
    c.key(code(KeyCode::Char('p')));
    c.key(code(KeyCode::Char('l')));
    c.key(code(KeyCode::Enter));
    assert_eq!(c.app().status.mode, "plan");

    c.app_mut().set_input("/limits ");
    let shown = screen(&c, 100, 30).join("\n");
    assert!(shown.contains("/limits [session|day <usd|off>]"), "{shown}");
    assert!(shown.contains("session <usd|off>"), "{shown}");
    c.key(code(KeyCode::Tab));
    assert!(screen(&c, 100, 30).join("\n").contains("/limits session <usd|off>"));

    c.app_mut().set_input("/profile ");
    assert!(c.app().popup_matches().iter().any(|e| e.name == "default"));
    c.app_mut().set_input("/login ");
    let names: Vec<String> = c.app().popup_matches().into_iter().map(|e| e.name).collect();
    assert!(names.contains(&"mock".to_string()) && names.contains(&"google".to_string()) && names.contains(&"web".to_string()), "{names:?}");
}

#[tokio::test]
async fn popups_pickers_and_settings_take_clicks() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    c.app_mut().set_input("/mod");
    click_on(&mut c, "/mode ");
    assert!(matches!(c.app().overlay, Some(Overlay::Pick(..))), "clicking /mode opens its picker");
    click_on(&mut c, "accept-edits");
    assert_eq!(c.app().status.mode, "accept-edits");

    c.app_mut().set_input("/mode ");
    c.mouse(mouse(MouseEventKind::ScrollDown, 10, 10));
    assert_eq!(c.app().popup_selected, 1, "the wheel moves through an open list");
    c.app_mut().set_input("");

    c.type_line("/config");
    assert!(matches!(c.app().mode, Mode::Panel(_)));
    click_on(&mut c, "General");
    match &c.app().mode {
        Mode::Panel(panel) => assert_eq!(panel.tab.label(), "General"),
        _ => panic!("the panel closed"),
    }
    click_on(&mut c, "mouse    ");
    let user = std::fs::read_to_string(env.user_dir().join("config.toml")).unwrap();
    assert!(user.contains("mouse = false"), "{user}");
    click_on(&mut c, "Modes");
    match &c.app().mode {
        Mode::Panel(panel) => assert_eq!(panel.tab.label(), "Modes"),
        _ => panic!("the panel closed"),
    }
}

#[tokio::test]
async fn switching_provider_picks_one_of_its_models_and_model_names_are_checked() {
    let mock = echo_server("ok").await;
    let other = echo_server("from other").await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/models"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(
            "{\"data\":[{\"id\":\"text-embedding-3\"},{\"id\":\"vendor/model-a\"},{\"id\":\"vendor/model-b\"}]}",
        ))
        .mount(&other)
        .await;
    let env = Env::new(&format!(
        "{}\n[providers.other]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n",
        provider_config(&mock.uri(), ""),
        other.uri()
    ))
    .await;
    let mut c = env.controller();
    c.type_line("/provider switch other");
    wait_for(&mut c, Duration::from_secs(10), |c| notices(c).iter().any(|n| n.contains("models found"))).await;
    assert!(
        notices(&c).iter().any(|n| n.contains("other: 3 models found · using vendor/model-a")),
        "{:?}",
        notices(&c)
    );
    wait_for(&mut c, Duration::from_secs(2), |c| c.app().status.model == "vendor/model-a").await;
    assert_eq!(c.app().status.model, "vendor/model-a", "the model of the old provider is not carried over");

    c.type_line("/model aion");
    assert!(notices(&c).last().unwrap().contains("'aion' is not one of the 3 models other lists"), "{:?}", notices(&c));
    assert_eq!(c.app().status.model, "vendor/model-a");
    c.app_mut().set_input("/model vendor");
    c.key(code(KeyCode::Esc));
    c.key(code(KeyCode::Enter));
    assert!(notices(&c).last().unwrap().contains("matches 2 models"), "{:?}", notices(&c));
    c.type_line("/model model-b");
    assert_eq!(c.app().status.model, "vendor/model-b");

    c.type_line("/model");
    assert!(matches!(c.app().overlay, Some(Overlay::Pick(..))));
    for ch in "model-a".chars() {
        c.key(code(KeyCode::Char(ch)));
    }
    c.key(code(KeyCode::Tab));
    assert_eq!(c.app().status.model, "vendor/model-a", "tab takes the highlighted model, not the typed text");
    turn(&mut c, "hi").await;
    assert_eq!(assistant(&c).last().map(String::as_str), Some("from other"));
}
