#![cfg(unix)]

mod support;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support::*;
use zarn_tui::state::{Block, CardStatus, Overlay, ToolCard};

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn code(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
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

fn card(name: &str, args: serde_json::Value, status: CardStatus, output: Option<&str>) -> Block {
    Block::Tool(ToolCard {
        id: name.into(),
        name: name.into(),
        args,
        status,
        output: output.map(str::to_string),
        ..ToolCard::default()
    })
}

fn quiet(c: &mut zarn_tui::Controller) {
    c.app_mut().blocks.clear();
    c.app_mut().theme.banner = false;
}

#[tokio::test]
async fn the_todo_tool_fills_a_live_region_and_is_stored_in_the_session() {
    let todos = json!({ "todos": [
        { "content": "read the parser", "status": "done" },
        { "content": "fix the off-by-one", "status": "in_progress" },
        { "content": "add a regression test", "status": "pending" }
    ]});
    let mock = server(scripted(vec![tool_sse("t", "todo", &todos), sse("on it")])).await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    turn(&mut c, "fix the parser").await;
    assert_eq!(c.app().todos.len(), 3);
    assert_eq!(c.app().todos[1].status, "in_progress");
    quiet(&mut c);
    let out = screen(&c, 70, 14);
    snapshot("todo_region", &out);
    let text = out.join("\n");
    assert!(text.contains("todo 1/3 done"), "{text}");
    assert!(text.contains("☑ read the parser"), "{text}");
    assert!(text.contains("◐ fix the off-by-one"), "{text}");

    let sessions = zarn_core::session::list(&zarn_core::session::data_root(), &env.project());
    let (meta, _) = zarn_core::session::read(&sessions[0]).unwrap();
    assert_eq!(meta.todos.len(), 3);
    assert_eq!(meta.todos[2].content, "add a regression test");
}

#[tokio::test]
async fn snapshot_each_tool_kind_has_its_own_card() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    quiet(&mut c);
    let mut write = card("write", json!({ "path": "src/new.rs", "content": "x" }), CardStatus::Done, Some("wrote src/new.rs"));
    if let Block::Tool(w) = &mut write {
        w.detail = Some("+12 -3 src/new.rs".into());
    }
    let mut bash = card("bash", json!({ "command": "cargo test" }), CardStatus::Running, None);
    if let Block::Tool(b) = &mut bash {
        b.live = (1..=8).map(|i| format!("test case_{i} ... ok")).collect();
    }
    let blocks = vec![
        card("read", json!({ "path": "src/lib.rs", "offset": 10 }), CardStatus::Done, Some("10\tfn a() {}\n11\tfn b() {}\n12\tfn c() {}")),
        card("grep", json!({ "pattern": "TODO" }), CardStatus::Done, Some("src/a.rs:1: TODO one\nsrc/b.rs:9: TODO two")),
        card("glob", json!({ "pattern": "**/*.toml" }), CardStatus::Done, Some("Cargo.toml\ncrates/a/Cargo.toml\ncrates/b/Cargo.toml")),
        write,
        card("task", json!({ "agent": "reviewer", "prompt": "review it" }), CardStatus::Running, None),
        bash,
        card("mcp__docs__search", json!({ "query": "tokio select" }), CardStatus::Done, Some("3 results")),
    ];
    c.app_mut().blocks = blocks;
    let out = screen(&c, 80, 46);
    snapshot("cards_per_tool", &out);
    let text = out.join("\n");
    assert!(text.contains("read  done  src/lib.rs · lines 10–12"));
    assert!(text.contains("grep  done  'TODO' · 2 matches"));
    assert!(text.contains("glob  done  **/*.toml · 3 files"));
    assert!(text.contains("write  done  +12 -3 src/new.rs"));
    assert!(text.contains("task  running  agent reviewer · review it"));
    assert!(text.contains("│ 10  fn a() {}"));
    assert!(text.contains("$ cargo test"));
    assert!(text.contains("… 2 earlier lines"));
    assert!(text.contains("test case_8 ... ok"));
    assert!(text.contains("query: tokio select"));
}

#[tokio::test]
async fn bash_output_streams_into_the_card_before_the_command_finishes() {
    let command = "for i in 1 2 3 4; do echo step$i; sleep 0.4; done";
    let mock = server(scripted(vec![tool_sse("b", "bash", &json!({ "command": command })), sse("ran")])).await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    c.type_line("/mode bypass");
    c.key(key('y'));
    c.type_line("run it");
    wait_for(&mut c, Duration::from_secs(10), |c| {
        c.app().blocks.iter().any(|b| matches!(b, Block::Tool(card) if card.output.is_none() && !card.live.is_empty()))
    })
    .await;
    let live = c
        .app()
        .blocks
        .iter()
        .find_map(|b| match b {
            Block::Tool(card) if card.output.is_none() => Some(card.live.clone()),
            _ => None,
        })
        .expect("a running bash card with streamed lines");
    assert_eq!(live[0], "step1");
    c.settle(Duration::from_secs(10)).await;
    let output = c
        .app()
        .blocks
        .iter()
        .find_map(|b| match b {
            Block::Tool(card) => card.output.clone(),
            _ => None,
        })
        .unwrap();
    assert!(output.contains("step4"));
}

#[tokio::test]
async fn the_spinner_shows_elapsed_time_and_streamed_tokens() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    quiet(&mut c);
    c.app_mut().streaming = true;
    c.app_mut().status.turn_started = Some(Instant::now() - Duration::from_millis(12_200));
    c.app_mut().status.streamed_chars = 1_200;
    let status = screen(&c, 100, 10).join("\n");
    assert!(status.contains("12s · ~300 tok"), "{status}");
    c.app_mut().status.turn_started = Some(Instant::now() - Duration::from_secs(75));
    assert!(screen(&c, 100, 10).join("\n").contains("1m15s"));
}

#[tokio::test]
async fn ctrl_o_shows_every_output_in_full_and_returns_to_the_same_place() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    quiet(&mut c);
    let long: String = (1..=40).map(|i| format!("row {i}\n")).collect();
    for i in 0..6 {
        c.app_mut().notice(format!("note {i}"));
    }
    c.app_mut().blocks.push(card("bash", json!({ "command": "seq" }), CardStatus::Done, Some(&long)));
    c.app_mut().scroll_up = 4;
    let normal = screen(&c, 80, 80).join("\n");
    assert!(normal.contains("more lines"));
    assert!(!normal.contains("row 40"));

    c.key(ctrl('o'));
    assert!(c.app().transcript);
    let full = screen(&c, 80, 80).join("\n");
    assert!(full.contains("transcript · every tool output in full"));
    assert!(full.contains("row 40"), "{full}");
    assert!(!full.contains("more lines"));
    c.key(code(KeyCode::PageUp));
    assert_eq!(c.app().transcript_scroll, 10);
    c.key(code(KeyCode::Esc));
    assert!(!c.app().transcript);
    assert_eq!(c.app().scroll_up, 4, "the chat view keeps its position");
    c.key(ctrl('o'));
    c.key(ctrl('o'));
    assert!(!c.app().transcript);
}

fn capture() -> (zarn_tui::notify::Notifier, Arc<Mutex<Vec<u8>>>) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let mut n = zarn_tui::notify::Notifier::with_writer(true, true, buf.clone());
    n.long_turn = Duration::ZERO;
    (n, buf)
}

#[tokio::test]
async fn long_turns_and_unfocused_prompts_ring_the_bell_unless_turned_off() {
    let mock = server(scripted(vec![
        sse("first"),
        tool_sse("w", "write", &json!({ "path": "a.txt", "content": "x" })),
        sse("wrote"),
    ]))
    .await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    let (notifier, buf) = capture();
    c.set_notifier(notifier);
    turn(&mut c, "hello").await;
    let rung = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert_eq!(rung, "\x07\x1b]2;zarn: turn finished\x07");
    buf.lock().unwrap().clear();

    c.app_mut().focused = Some(false);
    c.type_line("write a file");
    wait_for(&mut c, Duration::from_secs(5), |c| c.app().pending.is_some()).await;
    let rung = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(rung.contains("zarn: write needs permission"), "{rung:?}");
    c.key(key('n'));
    c.settle(Duration::from_secs(5)).await;

    std::fs::write(
        env.user_dir().join("config.toml"),
        format!("[general]\nnotifications = false\n\n{}", provider_config(&mock.uri(), "")),
    )
    .unwrap();
    let mut c = env.controller();
    let (notifier, buf) = capture();
    c.set_notifier(notifier);
    turn(&mut c, "again").await;
    assert!(buf.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_status_line_template_replaces_the_built_in_one() {
    let mock = echo_server("ok").await;
    let env = Env::new(&format!(
        "[general]\nstatus_line = \"<{{model}}> {{mode}} on {{branch}} in {{cwd}} · {{context}} · {{cost}} · {{profile}}\"\n\n{}",
        provider_config(&mock.uri(), "")
    ))
    .await;
    std::fs::create_dir_all(env.project().join(".git")).unwrap();
    std::fs::write(env.project().join(".git/HEAD"), "ref: refs/heads/feature/status\n").unwrap();
    let mut c = env.controller();
    quiet(&mut c);
    let status = screen(&c, 120, 8).join("\n");
    assert!(status.contains("<m> default on feature/status in "), "{status}");
    assert!(status.contains("· default"), "{status}");
    assert!(!status.contains("0 in / 0 out"));

    std::fs::write(
        env.user_dir().join("config.toml"),
        format!("[general]\nstatus_line = \"{{nope}}\"\n\n{}", provider_config(&mock.uri(), "")),
    )
    .unwrap();
    let mut c = env.controller();
    assert!(notices(&c).iter().any(|n| n.contains("{nope} is not a status line field")));
    quiet(&mut c);
    assert!(screen(&c, 120, 8).join("\n").contains("0 in / 0 out"));
}

#[tokio::test]
async fn snapshot_themes_in_light_and_dark_with_colors() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    quiet(&mut c);
    c.app_mut().notice("a notice in the dim role");
    c.app_mut().blocks.push(Block::User("hello".into()));
    c.app_mut().blocks.push(Block::Assistant("Some `code` and **bold** text.".into()));
    c.app_mut().blocks.push(card("grep", json!({ "pattern": "x" }), CardStatus::Done, Some("a.rs:1: x")));
    c.app_mut().blocks.push(card("bash", json!({ "command": "false" }), CardStatus::Failed, Some("exit 1")));
    c.app_mut().theme.palette = zarn_tui::theme::DARK;
    let dark = styled_screen(&c, 72, 22);
    snapshot("theme_dark", &dark);
    c.app_mut().theme.palette = zarn_tui::theme::LIGHT;
    let light = styled_screen(&c, 72, 22);
    snapshot("theme_light", &light);
    let light_text = light.join("\n");
    assert!(dark.join("\n").contains("DarkGray"));
    assert!(!light_text.contains("DarkGray"), "light theme never uses DarkGray");
    assert!(!light_text.contains("«Yellow»"));
    c.type_line("/theme light");
    assert_eq!(c.app().theme.palette, zarn_tui::theme::LIGHT);
    assert!(std::fs::read_to_string(env.project().join(".zarn/config.toml")).unwrap_or_default().contains("theme = \"light\"")
        || std::fs::read_to_string(env.user_dir().join("config.toml")).unwrap().contains("theme = \"light\""));
}

#[tokio::test]
async fn mouse_scrolling_is_a_toggle_and_search_moves_between_hits() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    assert!(!c.mouse_capture(), "off by default");
    c.app_mut().on_mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(c.app().scroll_up, 3);
    c.app_mut().scroll_up = 0;

    quiet(&mut c);
    for i in 0..40 {
        let text = if i == 3 || i == 30 { format!("line {i} has the needle") } else { format!("line {i}") };
        c.app_mut().notice(text);
    }
    c.key(ctrl('s'));
    for ch in "needle".chars() {
        c.key(key(ch));
    }
    assert!(screen(&c, 80, 20).join("\n").contains("search: needle"));
    c.key(code(KeyCode::Enter));
    let first = screen(&c, 80, 20).join("\n");
    assert!(first.contains("search 'needle' 1/2"), "{first}");
    assert!(first.contains("line 30 has the needle"));
    c.key(key('n'));
    let second = screen(&c, 80, 20).join("\n");
    assert!(second.contains("search 'needle' 2/2"), "{second}");
    assert!(second.contains("line 3 has the needle"), "{second}");
    c.key(key('N'));
    assert!(screen(&c, 80, 20).join("\n").contains("1/2"));
    c.key(code(KeyCode::Esc));
    assert!(c.app().search.is_none());

    std::fs::write(
        env.user_dir().join("config.toml"),
        format!("[general]\nmouse = true\n\n{}", provider_config(&mock.uri(), "")),
    )
    .unwrap();
    let c = env.controller();
    assert!(c.mouse_capture());
    let check = zarn_tui::terminal::mouse_check(true);
    assert!(check.detail.contains("native text selection now needs shift"));
}

#[tokio::test]
async fn export_copy_rename_and_resume_filtering() {
    zarn_config::auth::register_secret("test-key-not-real-phase5-export");
    let mock = echo_server("the answer").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    unsafe {
        std::env::remove_var("DISPLAY");
        std::env::remove_var("WAYLAND_DISPLAY");
    }
    let mut c = env.controller();
    turn(&mut c, "my key is test-key-not-real-phase5-export").await;
    c.type_line("/export notes/out.md");
    assert!(notices(&c).last().unwrap().contains("could not write"));
    c.type_line("/export out.md");
    let md = std::fs::read_to_string(env.project().join("out.md")).unwrap();
    assert!(md.contains("## User"));
    assert!(md.contains("## Assistant\n\nthe answer"));
    assert!(!md.contains("test-key-not-real-phase5-export"));

    c.type_line("/copy");
    assert_eq!(
        notices(&c).last().unwrap(),
        "no clipboard is available here; the last reply is:\nthe answer"
    );

    c.type_line("/rename parser bug hunt");
    assert!(notices(&c).last().unwrap().contains("renamed to \"parser bug hunt\""));
    drop(c);

    let mut other = env.controller();
    turn(&mut other, "an unrelated question about css").await;
    drop(other);

    let mut c = env.controller();
    c.type_line("/resume");
    let count = |c: &zarn_tui::Controller| match &c.app().overlay {
        Some(Overlay::Pick(p, _)) => p.visible().len(),
        _ => usize::MAX,
    };
    assert_eq!(count(&c), 2);
    for ch in "bug hunt".chars() {
        c.key(key(ch));
    }
    assert_eq!(count(&c), 1);
    for _ in 0.."bug hunt".len() {
        c.key(code(KeyCode::Backspace));
    }
    for ch in "css".chars() {
        c.key(key(ch));
    }
    assert_eq!(count(&c), 1);
    c.key(code(KeyCode::Enter));
    assert!(c.messages().iter().any(|m| m.text().contains("unrelated question about css")));
}
