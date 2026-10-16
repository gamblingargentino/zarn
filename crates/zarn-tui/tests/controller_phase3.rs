#![cfg(unix)]

mod support;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use support::*;
use zarn_tui::state::{Block, Overlay};

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
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
async fn saving_a_pattern_from_the_prompt_writes_it_through_ops_and_stops_asking() {
    let write = |content: &str| tool_sse("w", "write", &json!({ "path": "notes.md", "content": content }));
    let mock = server(scripted(vec![write("one"), sse("ok"), write("two"), sse("ok")])).await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    c.type_line("write notes");
    c.settle(Duration::from_secs(5)).await;
    let pending = c.app().pending.as_ref().expect("a permission prompt");
    assert_eq!(pending.pattern, "write(notes.md)");
    assert!(pending.preview.as_ref().unwrap().new_file);
    c.key(key('p'));
    c.settle(Duration::from_secs(5)).await;
    let project = std::fs::read_to_string(env.project().join(".zarn/config.toml")).unwrap();
    assert!(project.contains("allow = [\"write(notes.md)\"]"), "{project}");
    c.type_line("again");
    c.settle(Duration::from_secs(5)).await;
    assert!(c.app().pending.is_none());
    assert_eq!(std::fs::read_to_string(env.project().join("notes.md")).unwrap(), "two");
}

#[tokio::test]
async fn read_only_commands_run_without_a_prompt() {
    let mock = server(scripted(vec![tool_sse("b", "bash", &json!({ "command": "ls -la" }))])).await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    turn(&mut c, "list").await;
    assert!(c.app().pending.is_none());
    assert!(c.app().blocks.iter().any(|b| matches!(b, Block::Tool(card) if card.output.is_some())));
}

#[tokio::test]
async fn prompt_submit_hooks_block_and_turn_end_and_post_tool_hooks_fire() {
    let mock = server(scripted(vec![
        tool_sse("r", "read", &json!({ "path": "a.txt" })),
        sse("read it"),
    ]))
    .await;
    let env = Env::new("").await;
    std::fs::write(env.project().join("a.txt"), "content of a").unwrap();
    let hooks = env.root().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(
        hooks.join("submit.sh"),
        "input=$(cat)\ncase \"$input\" in *forbidden*) echo 'that word is not allowed' >&2; exit 1;; esac\n",
    )
    .unwrap();
    std::fs::write(hooks.join("post.sh"), format!("cat > {}\n", hooks.join("post.json").display())).unwrap();
    std::fs::write(hooks.join("end.sh"), format!("cat > {}\n", hooks.join("end.json").display())).unwrap();
    std::fs::write(
        env.user_dir().join("config.toml"),
        format!(
            "{}\n[[hooks.prompt_submit]]\ncommand = \"sh {}\"\n\n[[hooks.post_tool]]\nmatch = \"read\"\ncommand = \"sh {}\"\n\n[[hooks.turn_end]]\ncommand = \"sh {}\"\n",
            provider_config(&mock.uri(), ""),
            hooks.join("submit.sh").display(),
            hooks.join("post.sh").display(),
            hooks.join("end.sh").display()
        ),
    )
    .unwrap();
    let mut c = env.controller();
    turn(&mut c, "say the forbidden word").await;
    assert!(
        notices(&c).iter().any(|n| n.contains("blocked by a prompt_submit hook") && n.contains("not allowed")),
        "{:?}",
        notices(&c)
    );
    assert!(chat_bodies(&mock).await.is_empty());

    turn(&mut c, "read a.txt").await;
    wait_for(&mut c, Duration::from_secs(5), |_| hooks.join("end.json").exists()).await;
    let post: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(hooks.join("post.json")).unwrap()).unwrap();
    assert_eq!(post["tool"], "read");
    assert!(post["output"].as_str().unwrap().contains("content of a"));
    let end: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(hooks.join("end.json")).unwrap()).unwrap();
    assert_eq!(end["event"], "turn_end");

    c.type_line("/hooks");
    assert!(notices(&c).last().unwrap().contains("prompt_submit"));
}

#[tokio::test]
async fn rewind_restores_files_and_truncates_the_conversation_after_confirming() {
    let mock = server(scripted(vec![
        tool_sse("r1", "read", &json!({ "path": "plan.txt" })),
        tool_sse("w1", "write", &json!({ "path": "plan.txt", "content": "draft" })),
        sse("wrote it"),
        tool_sse("w2", "write", &json!({ "path": "plan.txt", "content": "final" })),
        sse("rewrote it"),
    ]))
    .await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    std::fs::write(env.project().join("plan.txt"), "original").unwrap();
    let mut c = env.controller();
    c.type_line("/mode bypass");
    c.key(key('y'));
    turn(&mut c, "first change").await;
    turn(&mut c, "second change").await;
    assert_eq!(std::fs::read_to_string(env.project().join("plan.txt")).unwrap(), "final");
    let before = c.messages().len();

    c.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    c.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    match &c.app().overlay {
        Some(Overlay::Pick(picker, _)) => {
            assert_eq!(picker.choices.len(), 2);
            assert!(picker.choices[0].label.starts_with("turn 2: second change"));
        }
        _ => panic!("expected the rewind picker"),
    }
    c.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    c.key(enter());
    match &c.app().overlay {
        Some(Overlay::Confirm(text, _)) => {
            assert!(text.contains("Rewind to before turn 1?"), "{text}");
            assert!(text.contains("restore plan.txt"), "{text}");
        }
        _ => panic!("expected a confirmation"),
    }
    assert_eq!(std::fs::read_to_string(env.project().join("plan.txt")).unwrap(), "final");
    c.key(key('y'));
    assert_eq!(std::fs::read_to_string(env.project().join("plan.txt")).unwrap(), "original");
    assert!(c.messages().len() < before);
    assert_eq!(c.messages().len(), 1, "back to just the system prompt");
    assert!(notices(&c).last().unwrap().starts_with("rewound to before turn 1"));
}

#[tokio::test]
async fn snapshot_permissions_shows_rules_mode_and_resolution() {
    let mock = echo_server("ok").await;
    let env = Env::new(&format!(
        "[permissions]\nedit = \"ask\"\nallow = [\"bash(cargo test:*)\"]\ndeny = [\"bash(rm -rf:*)\"]\n\n{}",
        provider_config(&mock.uri(), "")
    ))
    .await;
    let mut c = env.controller();
    c.type_line("/mode accept-edits");
    c.app_mut().blocks.clear();
    c.app_mut().theme.banner = false;
    c.type_line("/permissions");
    let out = screen(&c, 100, 30);
    snapshot("chat_permissions_summary", &out);
    let text = out.join("\n");
    assert!(text.contains("mode     accept-edits"));
    assert!(text.contains("deny  bash(rm -rf:*)  (user)"));
    assert!(text.contains("edit           allow accept-edits mode"));
    assert!(text.contains("read           allow default for its risk"));
    assert!(text.contains("webfetch       ask   default for its risk"));
}
