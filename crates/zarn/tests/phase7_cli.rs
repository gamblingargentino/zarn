mod support;

use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use support::*;

const KEY: &str = "test-key-not-real-phase7-print-0001";

fn scripted(steps: Vec<String>) -> Mock {
    let n = Arc::new(AtomicUsize::new(0));
    Mock::start(Arc::new(move |_seen: &Seen| {
        let i = n.fetch_add(1, Ordering::SeqCst);
        (200, "text/event-stream".into(), steps.get(i).cloned().unwrap_or_else(|| sse("done")))
    }))
}

fn setup(home: &Home, mock: &Mock) {
    home.config(&format!(
        "[providers.mock]\nkind = \"openai-compat\"\nbase_url = \"{}\"\napi_key_env = \"ZARN_PHASE7_KEY\"\n\n[profiles.default]\nprovider = \"mock\"\nmodel = \"m\"\n",
        mock.base()
    ));
}

fn run(home: &Home, args: &[&str], stdin: &str) -> std::process::Output {
    let mut child = home.command(args).env("ZARN_PHASE7_KEY", KEY).spawn().unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn bodies(mock: &Mock) -> Vec<Value> {
    mock.requests().iter().filter_map(|r| serde_json::from_str(&r.body).ok()).collect()
}

fn tool_names(body: &Value) -> Vec<String> {
    body["tools"].as_array().map(|t| t.iter().filter_map(|t| t["function"]["name"].as_str().map(str::to_string)).collect()).unwrap_or_default()
}

fn sessions(home: &Home) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let Ok(dirs) = std::fs::read_dir(home.root().join("data")) else { return found };
    for dir in dirs.flatten() {
        if let Ok(files) = std::fs::read_dir(dir.path()) {
            found.extend(files.flatten().map(|f| f.path()).filter(|p| p.extension().is_some_and(|x| x == "jsonl")));
        }
    }
    found
}

#[test]
fn json_output_is_one_object_with_text_tools_usage_and_cost_and_no_key() {
    let home = Home::new();
    std::fs::write(home.project().join("notes.txt"), "remember the milk").unwrap();
    let mock = scripted(vec![tool_call_sse("r1", "read", &json!({ "path": "notes.txt" })), sse(&format!("done with {KEY}"))]);
    setup(&home, &mock);
    let out = run(&home, &["--print", "read notes", "--output-format", "json"], "");
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(!text.contains(KEY), "{text}");
    let value: Value = serde_json::from_str(text.trim()).expect("one JSON object");
    assert_eq!(value["text"], "done with [redacted]");
    assert_eq!(value["tool_calls"][0]["name"], "read");
    assert!(value["tool_calls"][0]["output"].as_str().unwrap().contains("remember the milk"));
    assert_eq!(value["usage"]["output_tokens"], 10);
    assert!(value["cost_usd"].is_null());
    assert_eq!(value["stop_reason"], "end_turn");
    assert!(value["session_id"].is_string());
}

#[test]
fn stream_json_emits_one_object_per_event_and_ends_with_the_result() {
    let home = Home::new();
    std::fs::write(home.project().join("a.txt"), "alpha").unwrap();
    let mock = scripted(vec![tool_call_sse("r1", "read", &json!({ "path": "a.txt" })), sse(&format!("key {KEY}"))]);
    setup(&home, &mock);
    let out = run(&home, &["--print", "go", "--output-format", "stream-json"], "");
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(!text.contains(KEY));
    let lines: Vec<Value> = text.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).expect("each line is JSON")).collect();
    let kinds: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"tool_use") && kinds.contains(&"tool_result") && kinds.contains(&"text") && kinds.contains(&"usage"), "{kinds:?}");
    assert_eq!(*kinds.last().unwrap(), "result");
    assert_eq!(lines.last().unwrap()["tool_calls"][0]["name"], "read");
}

#[test]
fn the_prompt_can_come_from_stdin() {
    let home = Home::new();
    let mock = Mock::openai("ok");
    setup(&home, &mock);
    let out = run(&home, &["--print"], "hello from stdin\n");
    assert!(out.status.success(), "{}", stderr(&out));
    let body = bodies(&mock).pop().unwrap();
    assert!(body.to_string().contains("hello from stdin"));
}

#[test]
fn allowed_and_disallowed_tools_change_what_runs_and_what_is_offered() {
    let home = Home::new();
    let write = || tool_call_sse("w1", "write", &json!({ "path": "out.txt", "content": "x" }));
    let mock = scripted(vec![write(), sse("tried"), write(), sse("wrote")]);
    setup(&home, &mock);
    run(&home, &["--print", "write it"], "");
    assert!(!home.project().join("out.txt").exists(), "headless runs refuse what would ask");
    let out = run(&home, &["--print", "write it", "--allowed-tools", "write"], "");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(home.project().join("out.txt").exists());

    let mock = Mock::openai("ok");
    setup(&home, &mock);
    run(&home, &["--print", "hi", "--disallowed-tools", "read,bash"], "");
    let names = tool_names(&bodies(&mock).pop().unwrap());
    assert!(!names.contains(&"read".to_string()) && !names.contains(&"bash".to_string()), "{names:?}");
    assert!(names.contains(&"grep".to_string()));
}

#[test]
fn max_turns_stops_the_loop() {
    let home = Home::new();
    let read = || tool_call_sse("r", "glob", &json!({ "pattern": "*" }));
    let mock = scripted(vec![read(), read(), read(), sse("never")]);
    setup(&home, &mock);
    let out = run(&home, &["--print", "loop", "--max-turns", "2", "--output-format", "json"], "");
    assert!(stderr(&out).contains("stopped after --max-turns 2"), "{}", stderr(&out));
    let value: Value = serde_json::from_str(stdout(&out).trim()).unwrap();
    assert_eq!(value["stop_reason"], "max_turns");
    assert_eq!(mock.requests().len(), 2);
}

#[test]
fn add_dir_lets_file_tools_reach_another_directory() {
    let home = Home::new();
    let outside = home.root().join("shared");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("data.txt"), "outside data").unwrap();
    let path = outside.join("data.txt").display().to_string();
    let read = || tool_call_sse("r", "read", &json!({ "path": path }));
    let mock = scripted(vec![read(), sse("a"), read(), sse("b")]);
    setup(&home, &mock);
    let refused = run(&home, &["--print", "read it", "--output-format", "json"], "");
    let value: Value = serde_json::from_str(stdout(&refused).trim()).unwrap();
    assert_eq!(value["tool_calls"][0]["is_error"], true);
    assert!(value["tool_calls"][0]["output"].as_str().unwrap().contains("outside the working directory"));
    let allowed = run(&home, &["--print", "read it", "--add-dir", outside.to_str().unwrap(), "--output-format", "json"], "");
    let value: Value = serde_json::from_str(stdout(&allowed).trim()).unwrap();
    assert!(value["tool_calls"][0]["output"].as_str().unwrap().contains("outside data"), "{value}");
}

#[test]
fn system_prompt_file_and_append_shape_the_system_prompt() {
    let home = Home::new();
    std::fs::write(home.root().join("base.md"), "You are a terse release bot.").unwrap();
    std::fs::write(home.project().join("ZARN.md"), "project memory line").unwrap();
    let mock = Mock::openai("ok");
    setup(&home, &mock);
    let base = home.root().join("base.md");
    run(&home, &["--print", "hi", "--system-prompt-file", base.to_str().unwrap(), "--append-system-prompt", "Always end with DONE."], "");
    let body = bodies(&mock).pop().unwrap();
    let system = body["messages"][0]["content"].as_str().unwrap().to_string();
    assert!(system.starts_with("You are a terse release bot."), "{system}");
    assert!(!system.contains("You are zarn"));
    assert!(system.contains("project memory line"));
    assert!(system.ends_with("Always end with DONE."));
}

#[test]
fn sessions_persist_unless_asked_and_fork_leaves_the_original_alone() {
    let home = Home::new();
    let mock = Mock::openai("ok");
    setup(&home, &mock);
    run(&home, &["--print", "nothing saved", "--no-session-persistence"], "");
    assert!(sessions(&home).is_empty());
    run(&home, &["--print", "first turn"], "");
    let saved = sessions(&home);
    assert_eq!(saved.len(), 1);
    let original = std::fs::read_to_string(&saved[0]).unwrap();

    let out = run(&home, &["--print", "forked turn", "--continue", "--fork-session"], "");
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(sessions(&home).len(), 2);
    assert_eq!(std::fs::read_to_string(&saved[0]).unwrap(), original, "the original is unchanged");
    let last = bodies(&mock).pop().unwrap().to_string();
    assert!(last.contains("first turn") && last.contains("forked turn"), "the fork carries the history");

    run(&home, &["--print", "appended turn", "--resume", saved[0].file_stem().unwrap().to_str().unwrap()], "");
    assert!(std::fs::read_to_string(&saved[0]).unwrap().contains("appended turn"));
}

#[cfg(unix)]
#[test]
fn worktree_runs_in_a_new_branch_under_dot_zarn() {
    let home = Home::new();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(home.project())
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.test")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.test")
            .output()
            .unwrap();
        assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
        String::from_utf8_lossy(&ok.stdout).to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(home.project().join("a.txt"), "a").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "first"]);
    let mock = scripted(vec![tool_call_sse("w", "write", &json!({ "path": "made.txt", "content": "x" })), sse("done")]);
    setup(&home, &mock);
    let out = run(&home, &["--worktree", "feature-x", "--print", "work", "--allowed-tools", "write"], "");
    assert!(out.status.success(), "{}", stderr(&out));
    let tree = home.project().join(".zarn/worktrees/feature-x");
    assert!(tree.join("made.txt").exists(), "the run happened inside the worktree");
    assert!(!home.project().join("made.txt").exists());
    assert!(git(&["branch", "--list", "feature-x"]).contains("feature-x"));
    assert!(git(&["status", "--porcelain"]).trim().is_empty(), "the worktree does not dirty the main checkout");
}

#[test]
fn scheduled_prompts_are_stored_and_run_headlessly() {
    let home = Home::new();
    let mock = scripted(vec![tool_call_sse("w", "write", &json!({ "path": "sched.txt", "content": "x" })), sse("scheduled reply")]);
    setup(&home, &mock);
    let add = run(&home, &["schedule", "add", "check the build", "--when", "*/30 * * * *"], "");
    assert!(stdout(&add).contains("stored s1"), "{}", stderr(&add));
    assert!(stdout(&add).contains("*/30 * * * * cd "));
    assert!(stdout(&add).contains("zarn run-scheduled s1"));
    assert!(stdout(&run(&home, &["schedule", "list"], "")).contains("check the build"));
    assert!(!run(&home, &["schedule", "add", "x", "--mode", "bypass"], "").status.success());

    let ran = run(&home, &["run-scheduled", "s1"], "");
    assert!(stdout(&ran).contains("scheduled reply"), "{}", stderr(&ran));
    assert!(!home.project().join("sched.txt").exists(), "scheduled runs use the headless policy");
    assert!(stdout(&run(&home, &["schedule", "remove", "s1"], "")).contains("removed s1"));
    assert!(stdout(&run(&home, &["schedule", "list"], "")).contains("no scheduled prompts"));
}
