#![cfg(unix)]

mod support;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;
use support::*;
use zarn_tui::state::Mode;

fn code(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[tokio::test]
async fn a_panel_change_mid_turn_applies_to_the_next_turn_and_says_so() {
    let slow = server(|_| (sse("from a"), 1500)).await;
    let fast = echo_server("from b").await;
    let config = format!(
        "[providers.a]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[providers.b]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[profiles.default]\nprovider = \"a\"\nmodel = \"m\"\n\n[profiles.other]\nprovider = \"b\"\nmodel = \"m\"\n",
        slow.uri(),
        fast.uri()
    );
    let env = Env::new(&config).await;
    let mut c = env.controller();
    c.type_line("first question");
    assert!(c.app().streaming);

    c.type_line("/config");
    assert!(matches!(c.app().mode, Mode::Panel(_)));
    c.key(code(KeyCode::Right));
    c.key(code(KeyCode::Enter));
    c.key(code(KeyCode::Down));
    c.key(code(KeyCode::Enter));
    c.key(code(KeyCode::Esc));
    assert!(std::fs::read_to_string(env.user_dir().join("config.toml")).unwrap().contains("default_profile = \"other\""));
    assert!(
        notices(&c).iter().any(|n| n.contains("the current turn finishes on a (m); the next turn uses b (m)")),
        "{:?}",
        notices(&c)
    );

    c.settle(Duration::from_secs(10)).await;
    assert_eq!(assistant(&c).last().map(String::as_str), Some("from a"), "the turn in flight stayed on a");
    turn(&mut c, "second question").await;
    assert_eq!(assistant(&c).last().map(String::as_str), Some("from b"));
    let asked = |bodies: Vec<serde_json::Value>, text: &str| bodies.iter().filter(|b| b["messages"].to_string().contains(text)).count();
    assert_eq!(asked(chat_bodies(&slow).await, "first question"), 1);
    assert_eq!(asked(chat_bodies(&slow).await, "second question"), 0);
    assert_eq!(asked(chat_bodies(&fast).await, "second question"), 1);
}
