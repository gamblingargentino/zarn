#![cfg(unix)]

mod support;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use support::*;

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn multi_tool_sse(calls: &[(&str, &str, Value)]) -> String {
    let mut out = String::new();
    for (i, (id, name, args)) in calls.iter().enumerate() {
        let args = serde_json::to_string(&args.to_string()).unwrap();
        out.push_str(&format!(
            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":{i},\"id\":\"{id}\",\"function\":{{\"name\":\"{name}\",\"arguments\":{args}}}}}]}}}}]}}\n\n"
        ));
    }
    out.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":8}}\n\ndata: [DONE]\n\n");
    out
}

fn two_agents(main: Vec<String>, helper: Vec<String>, helper_delay: u64) -> impl Fn(&Value) -> (String, u64) + Send + Sync + 'static {
    let m = Arc::new(AtomicUsize::new(0));
    let h = Arc::new(AtomicUsize::new(0));
    move |body| {
        let system = system_text(body);
        if system.starts_with("Write a title") {
            return (sse("Title"), 0);
        }
        if system.starts_with("You are the helper agent") {
            let i = h.fetch_add(1, Ordering::SeqCst);
            return (helper.get(i).cloned().unwrap_or_else(|| sse("helper result")), helper_delay);
        }
        let i = m.fetch_add(1, Ordering::SeqCst);
        (main.get(i).cloned().unwrap_or_else(|| sse("done")), 0)
    }
}

fn agent_file(env: &Env) {
    let dir = env.project().join(".zarn/agents");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("helper.md"), "---\nname: helper\ndescription: helps\n---\nYou are the helper agent.\n").unwrap();
}

fn last_user_text(body: &Value) -> String {
    let last = body["messages"].as_array().unwrap().iter().rev().find(|m| m["role"] == "user").unwrap().clone();
    match &last["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

#[tokio::test]
async fn parallel_task_calls_show_live_rows_that_clear_when_the_turn_ends() {
    let calls = multi_tool_sse(&[
        ("t1", "task", json!({ "agent": "helper", "prompt": "first" })),
        ("t2", "task", json!({ "agent": "helper", "prompt": "second" })),
    ]);
    let mock = server(two_agents(vec![calls, sse("both done")], vec![], 700)).await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    agent_file(&env);
    let mut c = env.controller();
    c.type_line("/mode bypass");
    c.key(key('y'));
    c.app_mut().blocks.clear();
    c.app_mut().theme.banner = false;
    c.type_line("split the work");
    wait_for(&mut c, Duration::from_secs(5), |c| c.app().agents.len() == 2).await;
    assert_eq!(c.app().agents.len(), 2, "both agents run at once");
    let shown = screen(&c, 90, 16).join("\n");
    assert!(shown.contains("helper  thinking  ·  0s"), "{shown}");
    c.settle(Duration::from_secs(10)).await;
    assert!(c.app().agents.is_empty());
    assert!(assistant(&c).iter().any(|t| t.contains("both done")));
}

#[tokio::test]
async fn background_results_are_listed_by_tasks_and_delivered_on_the_next_turn() {
    let start = tool_sse("t1", "task", &json!({ "agent": "helper", "prompt": "dig", "background": true }));
    let mock = server(two_agents(vec![start, sse("started it"), sse("thanks")], vec![], 300)).await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    agent_file(&env);
    let mut c = env.controller();
    c.type_line("/mode bypass");
    c.key(key('y'));
    turn(&mut c, "start the helper").await;
    wait_for(&mut c, Duration::from_secs(5), |c| {
        notices(c).iter().any(|n| n.contains("background agent bg-1 (helper) finished"))
    })
    .await;
    c.type_line("/tasks");
    let listed = notices(&c).last().unwrap().clone();
    assert!(listed.contains("bg-1  helper  done"), "{listed}");
    assert!(listed.contains("helper result  (goes to the model with your next message)"), "{listed}");
    turn(&mut c, "what did it find?").await;
    let bodies = chat_bodies(&mock).await;
    let last_main = bodies.iter().rev().find(|b| !system_text(b).starts_with("You are the helper")).unwrap();
    let sent = last_user_text(last_main);
    assert!(sent.contains("<background_result id=\"bg-1\" agent=\"helper\" status=\"done\">\nhelper result"), "{sent}");
    assert!(sent.ends_with("what did it find?"));
    c.type_line("/tasks cancel bg-1");
    assert!(notices(&c).last().unwrap().contains("already done"));
}

#[tokio::test]
async fn subagent_tool_calls_go_through_the_same_permission_prompt() {
    let start = tool_sse("t1", "task", &json!({ "agent": "helper", "prompt": "write a file" }));
    let helper_write = tool_sse("w1", "write", &json!({ "path": "from-helper.txt", "content": "x" }));
    let mock = server(two_agents(vec![start, sse("ok")], vec![helper_write, sse("could not write")], 0)).await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    agent_file(&env);
    let mut c = env.controller();
    c.type_line("delegate");
    wait_for(&mut c, Duration::from_secs(5), |c| c.app().pending.is_some()).await;
    assert_eq!(c.app().pending.as_ref().unwrap().tool, "task");
    c.key(key('y'));
    wait_for(&mut c, Duration::from_secs(5), |c| c.app().pending.as_ref().is_some_and(|p| p.tool == "write")).await;
    assert_eq!(c.app().pending.as_ref().map(|p| p.tool.as_str()), Some("write"), "the subagent asks too");
    c.key(key('n'));
    c.settle(Duration::from_secs(10)).await;
    assert!(!env.project().join("from-helper.txt").exists());
}

fn command(env: &Env, name: &str, body: &str) {
    let dir = env.project().join(".zarn/commands");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.md")), body).unwrap();
}

#[tokio::test]
async fn custom_commands_substitute_arguments_include_files_and_switch_models() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    command(
        &env,
        "greet",
        "---\ndescription: greet someone\nargument-hint: <name> <role>\nmodel: other-model\n---\nHello $1, $2. All: $ARGUMENTS. See @notes.txt\n",
    );
    std::fs::write(env.project().join("notes.txt"), "the notes body").unwrap();
    let mut c = env.controller();
    let entry = c.app().entries.iter().find(|e| e.name == "greet").expect("a popup entry").clone();
    assert_eq!(entry.description, "greet someone");
    assert_eq!(entry.usage, "<name> <role>");
    turn(&mut c, "/greet Ada \"the dev\"").await;
    let body = chat_bodies(&mock).await.pop().unwrap();
    assert_eq!(body["model"], "other-model");
    let sent = last_user_text(&body);
    assert!(sent.starts_with("Hello Ada, the dev. All: Ada \"the dev\". See @notes.txt"), "{sent}");
    assert!(sent.contains("<file path=\"notes.txt\">\nthe notes body\n</file>"), "{sent}");
    turn(&mut c, "plain message").await;
    assert_eq!(chat_bodies(&mock).await.pop().unwrap()["model"], "m", "the model change lasts one turn");
}

#[tokio::test]
async fn bang_substitution_asks_like_bash_unless_allowed_tools_cover_it() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    command(&env, "made", "Result: !`touch made.txt && echo created`\n");
    command(
        &env,
        "made-ok",
        "---\nallowed-tools: bash(touch made-ok.txt:*), bash(echo created)\n---\nResult: [!`touch made-ok.txt`] [!`echo created`]\n",
    );
    let mut c = env.controller();
    c.type_line("/made");
    wait_for(&mut c, Duration::from_secs(5), |c| c.app().pending.is_some()).await;
    let pending = c.app().pending.as_ref().expect("the ! command asks first");
    assert_eq!(pending.tool, "bash");
    c.key(key('n'));
    wait_for(&mut c, Duration::from_secs(5), |c| notices(c).iter().any(|n| n.contains("was not run"))).await;
    assert!(!env.project().join("made.txt").exists());
    assert!(chat_bodies(&mock).await.is_empty(), "nothing goes to the model");

    turn(&mut c, "/made-ok").await;
    c.settle(Duration::from_secs(5)).await;
    assert!(c.app().pending.is_none(), "allowed-tools pre-approves it: {:?} {:?}", c.app().pending.as_ref().map(|p| (&p.tool, &p.args)), notices(&c));
    assert!(env.project().join("made-ok.txt").exists());
    let sent = last_user_text(&chat_bodies(&mock).await.pop().unwrap());
    assert_eq!(sent.trim(), "Result: [] [created]");
}

fn rich_mcp() -> String {
    format!(
        "[mcp.docs]\ncommand = \"sh\"\nargs = [\"{}/../zarn-tools/tests/fixtures/mcp_rich.sh\"]\n",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn tool_names(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .map(|t| t.iter().filter_map(|t| t["function"]["name"].as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn mcp_resources_prompts_and_tool_list_changes_work_in_the_chat() {
    let grow = tool_sse("g1", "docs__grow", &json!({}));
    let mock = server(two_agents(vec![sse("seen"), sse("summarized"), grow, sse("grew"), sse("after")], vec![], 0)).await;
    let env = Env::new(&format!("{}\n{}", provider_config(&mock.uri(), ""), rich_mcp())).await;
    let mut c = env.controller();
    wait_for(&mut c, Duration::from_secs(10), |c| c.app().entries.iter().any(|e| e.name == "docs:summarize")).await;
    let entry = c.app().entries.iter().find(|e| e.name == "docs:summarize").unwrap().clone();
    assert_eq!(entry.usage, "<topic> [style]");
    assert!(entry.description.contains("summarize a topic"));
    assert!(c.app().files.contains(&"docs:docs://readme".to_string()));

    turn(&mut c, "read @docs:docs://readme please").await;
    let sent = last_user_text(&chat_bodies(&mock).await.pop().unwrap());
    assert!(sent.contains("<resource server=\"docs\" uri=\"docs://readme\">\n# Readme from MCP\n</resource>"), "{sent}");
    assert!(sent.ends_with("read @docs:docs://readme please"));

    c.type_line("/docs:summarize");
    assert!(notices(&c).last().unwrap().contains("usage: /docs:summarize <topic> [style]"));
    turn(&mut c, "/docs:summarize rust terse").await;
    c.settle(Duration::from_secs(5)).await;
    let sent = last_user_text(&chat_bodies(&mock).await.pop().unwrap());
    assert_eq!(sent, "Summarize rust in a terse style.");

    c.type_line("/mode bypass");
    c.key(key('y'));
    turn(&mut c, "grow the server").await;
    wait_for(&mut c, Duration::from_secs(5), |c| notices(c).iter().any(|n| n.contains("mcp docs changed its tools; now 2"))).await;
    turn(&mut c, "what tools now?").await;
    let names = tool_names(&chat_bodies(&mock).await.pop().unwrap());
    assert!(names.contains(&"docs__fresh".to_string()), "{names:?}");
}

#[tokio::test]
async fn above_the_threshold_mcp_tools_load_through_tool_search() {
    let search = tool_sse("s1", "tool_search", &json!({ "query": "adds tool" }));
    let mock = server(two_agents(vec![search, sse("found it")], vec![], 0)).await;
    let env = Env::new(&format!("[general]\nlazy_tools_threshold = 5\n\n{}\n{}", provider_config(&mock.uri(), ""), rich_mcp())).await;
    let mut c = env.controller();
    wait_for(&mut c, Duration::from_secs(10), |c| c.app().entries.iter().any(|e| e.name == "docs:summarize")).await;
    turn(&mut c, "find a tool").await;
    let bodies = chat_bodies(&mock).await;
    let first = tool_names(&bodies[0]);
    assert!(first.contains(&"tool_search".to_string()), "{first:?}");
    assert!(!first.contains(&"docs__grow".to_string()), "{first:?}");
    assert!(first.contains(&"read".to_string()));
    let second = tool_names(&bodies[1]);
    assert!(second.contains(&"docs__grow".to_string()), "{second:?}");

    drop(c);
    std::fs::write(env.user_dir().join("config.toml"), format!("{}\n{}", provider_config(&mock.uri(), ""), rich_mcp())).unwrap();
    let mut c = env.controller();
    wait_for(&mut c, Duration::from_secs(10), |c| c.app().entries.iter().any(|e| e.name == "docs:summarize")).await;
    turn(&mut c, "below the default threshold").await;
    let names = tool_names(&chat_bodies(&mock).await.pop().unwrap());
    assert!(names.contains(&"docs__grow".to_string()));
    assert!(!names.contains(&"tool_search".to_string()));
}
