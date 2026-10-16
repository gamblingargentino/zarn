use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;
use std::path::Path;
use zarn_core::tools::Risk;
use zarn_tui::render;
use zarn_tui::state::{App, PermissionRequest};

fn rows(width: u16, height: u16, app: &App) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render::draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer.cell((x, y)).map(|c| c.symbol().to_string()).unwrap_or_default())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

fn snapshot(name: &str, rendered: &[String]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.txt"));
    let text = rendered.join("\n") + "\n";
    if std::env::var("ZARN_UPDATE_SNAPSHOTS").is_ok() {
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing snapshot {}\n{text}", path.display()));
    assert_eq!(text, expected, "snapshot {name} differs");
}

fn prompt(dir: &Path, tool: &str, args: serde_json::Value) -> App {
    let mut app = App::new("m");
    app.theme.banner = false;
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let mut request = PermissionRequest::new(tool, Risk::Write, args.clone(), tx);
    request.pattern = zarn_tools::permissions::suggest(tool, &args, dir);
    request.preview = zarn_tools::diff::preview(tool, &args, dir);
    app.pending = Some(request);
    app
}

#[test]
fn snapshot_an_edit_shows_a_unified_diff() {
    let dir = tempfile::tempdir().unwrap();
    let body = (1..=12).map(|i| format!("fn step_{i}() {{}}")).collect::<Vec<_>>().join("\n");
    std::fs::write(dir.path().join("lib.rs"), &body).unwrap();
    let app = prompt(
        dir.path(),
        "edit",
        json!({ "path": "lib.rs", "old_string": "fn step_6() {}", "new_string": "fn step_six() {\n    run();\n}" }),
    );
    let out = rows(90, 20, &app);
    snapshot("permission_edit_diff", &out);
    let text = out.join("\n");
    assert!(text.contains("edit lib.rs (+3 -1)"));
    assert!(text.contains("6 - fn step_6() {}"));
    assert!(text.contains("6 + fn step_six() {"));
    assert!(text.contains("[a] allow edit(lib.rs) for this session"));
    assert_eq!(std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(), body);
}

#[test]
fn snapshot_a_new_file_shows_its_content_under_a_new_file_header() {
    let dir = tempfile::tempdir().unwrap();
    let app = prompt(
        dir.path(),
        "write",
        json!({ "path": "notes/todo.md", "content": "# Todo\n\n- ship it\n" }),
    );
    let out = rows(90, 12, &app);
    snapshot("permission_new_file", &out);
    assert!(out.join("\n").contains("write new file notes/todo.md (+3 -0)"));
    assert!(!dir.path().join("notes/todo.md").exists());
}

#[test]
fn snapshot_a_large_diff_is_truncated_with_a_count_and_expands() {
    let dir = tempfile::tempdir().unwrap();
    let body: String = (1..=80).map(|i| format!("old {i}\n")).collect();
    std::fs::write(dir.path().join("big.txt"), &body).unwrap();
    let replacement: String = (1..=80).map(|i| format!("new {i}\n")).collect();
    let mut app = prompt(
        dir.path(),
        "write",
        json!({ "path": "big.txt", "content": replacement }),
    );
    let out = rows(90, 40, &app);
    snapshot("permission_truncated_diff", &out);
    let text = out.join("\n");
    assert!(text.contains("… 131 more diff lines ([e] expands)"), "{text}");
    app.on_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('e'),
        crossterm::event::KeyModifiers::NONE,
    ));
    assert!(app.pending_expanded);
    let expanded = rows(90, 200, &app).join("\n");
    assert!(expanded.contains("80 + new 80"));
    assert!(!expanded.contains("more diff lines"));
}
