use ratatui::backend::TestBackend;
use ratatui::Terminal;
use serde_json::json;
use zarn_core::agent::AgentEvent;
use zarn_core::provider::Event;
use zarn_core::tools::Risk;
use zarn_tui::render;
use zarn_tui::state::{App, Block, CardStatus, PermissionRequest, ToolCard};

fn render_app(width: u16, height: u16, app: &App) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|frame| {
            render::draw(frame, app);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let mut rows = Vec::new();

    for y in 0..height {
        let mut row = String::new();
        for x in 0..width {
            let cell = buffer.cell((x, y)).cloned().unwrap_or_default();
            row.push_str(cell.symbol());
        }
        rows.push(row);
    }

    rows
}

#[test]
fn empty_app_renders_without_panic() {
    let app = App::new("claude-3-5-sonnet");
    let output = render_app(80, 24, &app);
    assert!(!output.is_empty());
}

#[test]
fn empty_app_shows_model_name_in_status_line() {
    let app = App::new("gpt-4-turbo");
    let output = render_app(80, 24, &app);

    let status_line = &output[20];
    assert!(status_line.contains("gpt-4-turbo"));
}

#[test]
fn streamed_assistant_text_appears_in_scrollback() {
    let mut app = App::new("model");
    app.apply(AgentEvent::Provider(Event::TextDelta("Hello, ".into())));
    app.apply(AgentEvent::Provider(Event::TextDelta("world!".into())));

    let output = render_app(80, 24, &app);
    let text = output.join("\n");

    assert!(text.contains("Hello, world!"));
}

#[test]
fn user_message_and_assistant_reply_appear_in_order() {
    let mut app = App::new("model");

    app.blocks.push(Block::User("What is 2+2?".into()));
    app.apply(AgentEvent::Provider(Event::TextDelta("The answer is 4.".into())));

    let output = render_app(80, 24, &app);
    let text = output.join("\n");

    let user_pos = text.find("What is 2+2?").expect("user message not found");
    let assistant_pos = text
        .find("The answer is 4.")
        .expect("assistant message not found");

    assert!(user_pos < assistant_pos, "user message should come before assistant message");
}

#[test]
fn tool_card_renders_name_and_status() {
    let mut app = App::new("model");
    app.blocks.push(Block::Tool(ToolCard {
        id: "tool-1".into(),
        name: "bash".into(),
        args: json!({ "command": "ls -la" }),
        status: CardStatus::Done,
        output: Some("file1.txt\nfile2.txt".into()),
        expanded: false,
        ..ToolCard::default()
    }));

    let output = render_app(80, 24, &app);
    let text = output.join("\n");

    assert!(text.contains("bash"), "tool name should appear");
    assert!(text.contains("done"), "status label should appear");
}

#[test]
fn long_tool_output_is_collapsed_with_line_count() {
    let mut app = App::new("model");
    let long_output = (0..10)
        .map(|i| format!("line {}", i))
        .collect::<Vec<_>>()
        .join("\n");

    app.blocks.push(Block::Tool(ToolCard {
        id: "tool-1".into(),
        name: "bash".into(),
        args: json!({}),
        status: CardStatus::Done,
        output: Some(long_output),
        expanded: false,
        ..ToolCard::default()
    }));

    let output = render_app(80, 24, &app);
    let text = output.join("\n");

    assert!(
        text.contains("more lines"),
        "should show 'more lines' message for collapsed output"
    );
}

#[test]
fn expanding_output_shows_all_lines_and_removes_more_marker() {
    let mut app = App::new("model");
    let long_output = (0..10)
        .map(|i| format!("line {}", i))
        .collect::<Vec<_>>()
        .join("\n");

    app.blocks.push(Block::Tool(ToolCard {
        id: "tool-1".into(),
        name: "bash".into(),
        args: json!({}),
        status: CardStatus::Done,
        output: Some(long_output),
        expanded: false,
        ..ToolCard::default()
    }));

    app.expand_last_output();

    let output = render_app(80, 24, &app);
    let text = output.join("\n");

    assert!(
        !text.contains("more lines"),
        "should not show 'more lines' when expanded"
    );
    assert!(text.contains("line 9"), "should show last line when expanded");
}

#[test]
fn pending_permission_prompt_renders_options() {
    let mut app = App::new("model");
    let (tx, _rx) = tokio::sync::oneshot::channel();

    app.pending = Some(PermissionRequest::new(
        "bash",
        Risk::Execute,
        json!({ "command": "rm -rf /" }),
        tx,
    ));

    let output = render_app(80, 24, &app);
    let text = output.join("\n");

    assert!(text.contains("bash"), "should show tool name");
    assert!(text.contains("wants to run"), "should show permission prompt");
    assert!(text.contains("[y]"), "should show 'y' option");
    assert!(text.contains("[n]"), "should show 'n' option");
    assert!(text.contains("[a]"), "should show 'a' option");
}

#[test]
fn status_line_shows_token_counts_after_usage_event() {
    let mut app = App::new("model");
    app.apply(AgentEvent::Provider(Event::Usage {
        input: 100,
        output: 50,
        cached: 0,
    }));

    let output = render_app(80, 24, &app);
    let status_line = &output[20];

    assert!(status_line.contains("100"), "should show input token count");
    assert!(status_line.contains("50"), "should show output token count");
}

#[test]
fn narrow_width_40_does_not_panic() {
    let mut app = App::new("claude-3-5-sonnet");
    app.blocks.push(Block::User("What is the meaning of life, the universe, and everything?".into()));
    app.apply(AgentEvent::Provider(Event::TextDelta(
        "The answer is 42.".into(),
    )));

    let output = render_app(40, 24, &app);
    assert!(!output.is_empty(), "render should produce output");
    assert!(output.join("\n").contains("What is the meaning"));
}

#[test]
fn very_long_unbroken_word_does_not_panic_at_narrow_width() {
    let mut app = App::new("model");
    let long_path = "/".to_string()
        + &"very/long/path/component/".repeat(20);
    app.blocks.push(Block::User(long_path));

    let output = render_app(40, 24, &app);
    assert!(!output.is_empty(), "render should complete without panic");
}

#[test]
fn tool_status_pending_shows_correct_label() {
    let mut app = App::new("model");
    app.blocks.push(Block::Tool(ToolCard {
        id: "tool-1".into(),
        name: "read".into(),
        args: json!({}),
        status: CardStatus::Pending,
        output: None,
        expanded: false,
        ..ToolCard::default()
    }));

    let output = render_app(80, 24, &app);
    let text = output.join("\n");

    assert!(text.contains("pending"));
}

#[test]
fn tool_status_running_shows_correct_label() {
    let mut app = App::new("model");
    app.blocks.push(Block::Tool(ToolCard {
        id: "tool-1".into(),
        name: "bash".into(),
        args: json!({}),
        status: CardStatus::Running,
        output: None,
        expanded: false,
        ..ToolCard::default()
    }));

    let output = render_app(80, 24, &app);
    let text = output.join("\n");

    assert!(text.contains("running"));
}

#[test]
fn tool_status_failed_shows_correct_label() {
    let mut app = App::new("model");
    app.blocks.push(Block::Tool(ToolCard {
        id: "tool-1".into(),
        name: "write".into(),
        args: json!({}),
        status: CardStatus::Failed,
        output: Some("Error: permission denied".into()),
        expanded: false,
        ..ToolCard::default()
    }));

    let output = render_app(80, 24, &app);
    let text = output.join("\n");

    assert!(text.contains("failed"));
}

#[test]
fn notice_block_appears_in_output() {
    let mut app = App::new("model");
    app.notice("This is a notice");

    let output = render_app(80, 24, &app);
    let text = output.join("\n");

    assert!(text.contains("This is a notice"));
}

#[test]
fn cached_tokens_appear_in_status_when_present() {
    let mut app = App::new("model");
    app.apply(AgentEvent::Provider(Event::Usage {
        input: 50,
        output: 30,
        cached: 100,
    }));

    let output = render_app(80, 24, &app);
    let status_line = &output[20];

    assert!(status_line.contains("100"), "should show cached token count");
    assert!(status_line.contains("cached"));
}

#[test]
fn cost_appears_in_status_line() {
    let mut app = App::new("model");
    app.status.cost = Some(0.0042);

    let output = render_app(80, 24, &app);
    let status_line = &output[20];

    assert!(
        status_line.contains("$"),
        "should show cost with dollar sign"
    );
}

#[test]
fn multiple_blocks_separated_by_blank_lines() {
    let mut app = App::new("model");
    app.blocks.push(Block::User("First message".into()));
    app.apply(AgentEvent::Provider(Event::TextDelta("Response".into())));
    app.blocks.push(Block::User("Second message".into()));

    let output = render_app(80, 24, &app);
    let text = output.join("\n");

    assert!(text.contains("First message"));
    assert!(text.contains("Response"));
    assert!(text.contains("Second message"));
}

#[test]
fn tool_with_args_shows_truncated_argument_values() {
    let mut app = App::new("model");
    app.blocks.push(Block::Tool(ToolCard {
        id: "tool-1".into(),
        name: "mcp__shell__run".into(),
        args: json!({ "command": "echo 'hello'" }),
        status: CardStatus::Done,
        output: None,
        expanded: false,
        ..ToolCard::default()
    }));
    app.blocks.push(Block::Tool(ToolCard {
        id: "tool-2".into(),
        name: "bash".into(),
        args: json!({ "command": "echo 'hello'" }),
        status: CardStatus::Done,
        ..ToolCard::default()
    }));

    let output = render_app(80, 24, &app);
    let text = output.join("\n");

    assert!(text.contains("command: echo 'hello'"));
    assert!(text.contains("$ echo 'hello'"));
}

#[test]
fn narrow_width_with_multiline_user_message() {
    let mut app = App::new("model");
    app.blocks.push(Block::User("First line\nSecond line\nThird line".into()));

    let output = render_app(40, 24, &app);
    assert!(!output.is_empty(), "render should complete without panic");
    let text = output.join("\n");
    assert!(text.contains("First line"));
    assert!(text.contains("Second line"));
}

#[test]
fn the_banner_is_painted_on_a_fresh_session() {
    let app = App::new("claude-sonnet-5");
    let output = render_app(80, 24, &app);
    let joined = output.join("\n");

    for row in zarn_tui::brand::MARK {
        assert!(joined.contains(row.trim_end()), "missing mark row {row:?}");
    }
    assert!(joined.contains("claude-sonnet-5"));
    assert!(joined.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn the_status_icon_only_appears_while_streaming_and_changes_per_tick() {
    let mut app = App::new("m");
    let idle = render_app(80, 24, &app);
    let status_row = idle.len() - 4;
    assert!(!idle[status_row].starts_with(zarn_tui::brand::FRAMES[0]));

    app.streaming = true;
    let first = render_app(80, 24, &app);
    assert!(
        first[status_row].starts_with(zarn_tui::brand::FRAMES[0]),
        "{:?}",
        first[status_row]
    );

    app.advance_frame();
    let second = render_app(80, 24, &app);
    assert_ne!(first[status_row], second[status_row]);
}
