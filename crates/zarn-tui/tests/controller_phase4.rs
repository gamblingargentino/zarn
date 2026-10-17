mod support;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;
use std::time::Duration;
use support::*;
use zarn_tui::state::{Block, Overlay};

fn user_content(body: &Value) -> Vec<Value> {
    let last = body["messages"].as_array().unwrap().last().unwrap().clone();
    match &last["content"] {
        Value::Array(parts) => parts.clone(),
        Value::String(text) => vec![serde_json::json!({ "type": "text", "text": text })],
        _ => Vec::new(),
    }
}

fn text_of(body: &Value) -> String {
    user_content(body)
        .iter()
        .filter_map(|p| p["text"].as_str().map(str::to_string))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn mentioned_files_go_with_the_message_and_outside_paths_are_refused() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    std::fs::create_dir_all(env.project().join("src")).unwrap();
    std::fs::write(env.project().join("src/lib.rs"), "pub fn answer() -> u32 { 42 }").unwrap();
    std::fs::write(env.root().join("outside.txt"), "private").unwrap();
    let mut c = env.controller();
    assert!(c.app().files.contains(&"src/lib.rs".to_string()));
    turn(&mut c, "explain @src/lib.rs and @../outside.txt").await;
    let sent = text_of(&chat_bodies(&mock).await[0]);
    assert!(sent.contains("<file path=\"src/lib.rs\">\npub fn answer() -> u32 { 42 }\n</file>"), "{sent}");
    assert!(!sent.contains("private"));
    assert!(notices(&c).iter().any(|n| n.contains("@../outside.txt is outside the working directory")));
}

#[tokio::test]
async fn images_attach_for_vision_providers_and_explain_themselves_otherwise() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "vision = true\n")).await;
    std::fs::write(env.project().join("shot.png"), [137u8, 80, 78, 71, 13, 10]).unwrap();
    let mut c = env.controller();
    turn(&mut c, "what is in @shot.png").await;
    let parts = user_content(&chat_bodies(&mock).await[0]);
    assert!(parts.iter().any(|p| p["type"] == "image_url"), "{parts:?}");

    let plain = echo_server("ok").await;
    drop(c);
    drop(env);
    let env = Env::new(&provider_config(&plain.uri(), "")).await;
    std::fs::write(env.project().join("shot.png"), [137u8, 80, 78, 71]).unwrap();
    std::fs::write(env.project().join("paper.pdf"), b"%PDF-1.7").unwrap();
    let mut c = env.controller();
    turn(&mut c, "look at @shot.png and @paper.pdf").await;
    let said = notices(&c).join("\n");
    assert!(said.contains("does not accept images"), "{said}");
    assert!(said.contains("cannot extract PDF text"), "{said}");
    assert!(user_content(&chat_bodies(&plain).await[0]).iter().all(|p| p["type"] != "image_url"));
}

#[tokio::test]
async fn bang_runs_a_shell_command_without_the_model_and_can_be_attached() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    std::fs::write(env.project().join("visible.txt"), "x").unwrap();
    let mut c = env.controller();
    c.type_line("!ls");
    wait_for(&mut c, Duration::from_secs(5), |c| {
        c.app().blocks.iter().any(|b| matches!(b, Block::Tool(card) if card.output.is_some()))
    })
    .await;
    assert!(chat_bodies(&mock).await.is_empty(), "nothing goes to the model");
    let output = c
        .app()
        .blocks
        .iter()
        .find_map(|b| match b {
            Block::Tool(card) => card.output.clone(),
            _ => None,
        })
        .unwrap();
    assert!(output.contains("visible.txt"));
    c.type_line("/attach-output");
    turn(&mut c, "what did that print?").await;
    let sent = text_of(&chat_bodies(&mock).await[0]);
    assert!(sent.contains("Output of `ls`, run by the user:"));
    assert!(sent.contains("visible.txt"));
    assert!(sent.ends_with("what did that print?"));
}

#[tokio::test]
async fn bang_goes_through_the_bash_permission_policy() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    c.type_line("!touch made-by-bang.txt");
    wait_for(&mut c, Duration::from_secs(5), |c| c.app().pending.is_some()).await;
    assert!(c.app().pending.is_some(), "a non read-only command asks first");
    c.key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    wait_for(&mut c, Duration::from_secs(5), |c| {
        c.app().blocks.iter().any(|b| matches!(b, Block::Tool(card) if card.output.is_some()))
    })
    .await;
    assert!(!env.project().join("made-by-bang.txt").exists());
}

#[tokio::test]
async fn hash_appends_to_the_project_zarn_md_or_asks_when_both_exist() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    c.type_line("# run cargo fmt before committing");
    assert_eq!(
        std::fs::read_to_string(env.project().join("ZARN.md")).unwrap(),
        "- run cargo fmt before committing\n"
    );
    std::fs::write(env.user_dir().join("ZARN.md"), "user rules\n").unwrap();
    c.type_line("# prefer small commits");
    let user_path = env.user_dir().join("ZARN.md");
    match &c.app().overlay {
        Some(Overlay::Pick(picker, _)) => assert_eq!(picker.choices.len(), 2),
        _ => panic!("expected the file picker"),
    }
    for _ in 0..5 {
        c.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    c.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(std::fs::read_to_string(&user_path).unwrap(), "user rules\n- prefer small commits\n");
    assert_eq!(
        std::fs::read_to_string(env.project().join("ZARN.md")).unwrap(),
        "- run cargo fmt before committing\n"
    );
    let files: Vec<_> = std::fs::read_dir(env.project())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(files.iter().filter(|f| !f.starts_with('.')).count(), 1, "{files:?}");
}

#[tokio::test]
async fn doctor_reports_keybinding_conflicts() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    std::fs::write(env.user_dir().join("keybindings.toml"), "transcript = \"ctrl+r\"\n").unwrap();
    let mut c = env.controller();
    c.type_line("/doctor");
    wait_for(&mut c, Duration::from_secs(20), |c| {
        notices(c).iter().any(|n| n.contains("failed,"))
    })
    .await;
    let report = notices(&c).into_iter().find(|n| n.contains("failed,")).unwrap();
    assert!(report.contains("ctrl+r is bound to both"), "{report}");
    assert!(report.contains("keybindings.toml to fix it"), "{report}");
}

#[cfg(unix)]
#[tokio::test]
async fn ctrl_g_edits_the_input_in_the_external_editor() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let script = env.root().join("ed.sh");
    std::fs::write(&script, "printf 'from the editor' > \"$1\"\n").unwrap();
    unsafe {
        std::env::set_var("VISUAL", format!("sh {}", script.display()));
    }
    let mut c = env.controller();
    c.app_mut().set_input("draft");
    c.key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
    assert_eq!(c.app().input, "from the editor");
    unsafe {
        std::env::remove_var("VISUAL");
        std::env::remove_var("EDITOR");
    }
    c.key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
    assert!(notices(&c).last().unwrap().contains("set $VISUAL or $EDITOR"));
}

#[tokio::test]
async fn diff_shows_uncommitted_changes_without_the_model() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(env.project())
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q"]);
    std::fs::write(env.project().join("tracked.txt"), "before\n").unwrap();
    git(&["add", "tracked.txt"]);
    git(&["commit", "-q", "-m", "first"]);
    std::fs::write(env.project().join("tracked.txt"), "after\n").unwrap();
    let mut c = env.controller();
    c.type_line("/diff");
    wait_for(&mut c, Duration::from_secs(5), |c| {
        c.app().blocks.iter().any(|b| matches!(b, Block::Tool(card) if card.name == "git diff" && card.output.is_some()))
    })
    .await;
    let output = c
        .app()
        .blocks
        .iter()
        .find_map(|b| match b {
            Block::Tool(card) if card.name == "git diff" => card.output.clone(),
            _ => None,
        })
        .unwrap();
    assert!(output.contains("-before") && output.contains("+after"), "{output}");
    assert!(
        c.app().blocks.iter().any(|b| matches!(b, Block::Tool(card) if card.name == "git diff" && card.expanded)),
        "a short diff opens unfolded"
    );
    assert!(chat_bodies(&mock).await.is_empty(), "nothing goes to the model");
    c.type_line("/attach-output");
    turn(&mut c, "look at this").await;
    assert!(text_of(&chat_bodies(&mock).await[0]).contains("+after"));
}

#[tokio::test]
async fn style_changes_the_system_prompt_and_default_removes_it() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let styles = env.project().join(".zarn").join("styles");
    std::fs::create_dir_all(&styles).unwrap();
    std::fs::write(styles.join("haiku.md"), "---\ndescription: answers in haiku\n---\nAnswer every question as a haiku.").unwrap();
    let mut c = env.controller();
    let system = |body: &Value| body["messages"][0]["content"].as_str().unwrap_or_default().to_string();

    c.type_line("/style explanatory");
    assert!(notices(&c).last().unwrap().contains("output style is now explanatory"));
    turn(&mut c, "first").await;
    assert!(system(&chat_bodies(&mock).await[0]).contains("Output style: explanatory."));

    c.type_line("/style haiku");
    turn(&mut c, "second").await;
    let second = system(&chat_bodies(&mock).await[1]);
    assert!(second.contains("Answer every question as a haiku.") && !second.contains("Output style: explanatory."));

    c.type_line("/style nope");
    assert!(notices(&c).last().unwrap().contains("no style named 'nope'"));

    c.type_line("/style default");
    turn(&mut c, "third").await;
    let third = system(&chat_bodies(&mock).await[2]);
    assert!(!third.contains("haiku") && !third.contains("Output style"), "{third}");
}
