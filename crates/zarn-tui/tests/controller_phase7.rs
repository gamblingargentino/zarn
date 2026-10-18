#![cfg(unix)]

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

fn scripted(steps: Vec<String>) -> impl Fn(&Value) -> (String, u64) + Send + Sync + 'static {
    let n = Arc::new(AtomicUsize::new(0));
    move |body| {
        if system_text(body).starts_with("Write a title") {
            return (sse("Title"), 0);
        }
        let i = n.fetch_add(1, Ordering::SeqCst);
        (steps.get(i).cloned().unwrap_or_else(|| sse("done")), 0)
    }
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.test")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.test")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn repo(env: &Env) {
    let p = env.project();
    git(&p, &["init", "-q", "-b", "main"]);
    git(&p, &["config", "user.name", "t"]);
    git(&p, &["config", "user.email", "t@example.test"]);
    std::fs::write(p.join("lib.rs"), "fn a() {}\n").unwrap();
    git(&p, &["add", "lib.rs"]);
    git(&p, &["commit", "-q", "-m", "first"]);
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
async fn commit_drafts_from_the_staged_diff_and_only_commits_after_yes() {
    let draft = "Add the b function\n\nIt returns nothing yet.\n\nCo-Authored-By: Some Bot <bot@example.test>";
    let mock = server(scripted(vec![sse(draft), sse(draft)])).await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    repo(&env);
    let mut c = env.controller();
    c.type_line("/commit");
    c.settle(Duration::from_secs(5)).await;
    assert!(notices(&c).last().unwrap().contains("nothing is staged"), "{:?}", notices(&c));

    std::fs::write(env.project().join("lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    git(&env.project(), &["add", "lib.rs"]);
    c.type_line("/commit");
    wait_for(&mut c, Duration::from_secs(10), |c| matches!(c.app().overlay, Some(Overlay::Confirm(..)))).await;
    let Some(Overlay::Confirm(text, _)) = &c.app().overlay else { panic!("expected the confirmation") };
    assert!(text.contains("Add the b function\n\nIt returns nothing yet."), "{text}");
    assert!(!text.contains("Co-Authored-By"), "no attribution unless configured");
    let sent = last_user_text(&chat_bodies(&mock).await.pop().unwrap());
    assert!(sent.contains("+fn b() {}"), "the staged diff went to the model: {sent}");
    c.key(key('n'));
    assert_eq!(git(&env.project(), &["rev-list", "--count", "HEAD"]).trim(), "1", "no commit without yes");

    c.type_line("/commit");
    wait_for(&mut c, Duration::from_secs(10), |c| matches!(c.app().overlay, Some(Overlay::Confirm(..)))).await;
    c.key(key('y'));
    wait_for(&mut c, Duration::from_secs(10), |c| notices(c).iter().any(|n| n.starts_with("committed"))).await;
    let log = git(&env.project(), &["log", "-1", "--format=%B"]);
    assert_eq!(log.trim(), "Add the b function\n\nIt returns nothing yet.");
}

#[tokio::test]
async fn pr_drafts_a_title_and_body_without_calling_any_api_and_review_uses_the_tools() {
    let mock = server(scripted(vec![sse("Title: Add parsing\n\n## Summary\nAdds a parser."), sse("looks fine")])).await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    repo(&env);
    git(&env.project(), &["checkout", "-q", "-b", "feature"]);
    std::fs::write(env.project().join("parse.rs"), "fn parse() {}\n").unwrap();
    git(&env.project(), &["add", "."]);
    git(&env.project(), &["commit", "-q", "-m", "parser"]);
    let mut c = env.controller();
    c.type_line("/pr");
    wait_for(&mut c, Duration::from_secs(10), |c| notices(c).iter().any(|n| n.starts_with("pull request draft"))).await;
    let note = notices(&c).last().unwrap().clone();
    assert!(note.contains("Title: Add parsing") && note.contains("## Summary"), "{note}");
    assert!(std::fs::read_to_string(env.project().join(".zarn/pr.md")).unwrap().starts_with("# Add parsing"));
    let material = last_user_text(&chat_bodies(&mock).await.pop().unwrap());
    assert!(material.contains("parser") && material.contains("+fn parse() {}"), "{material}");
    assert_eq!(mock.received_requests().await.unwrap().len(), 1, "one model call and nothing else");

    turn(&mut c, "/review security").await;
    let sent = last_user_text(&chat_bodies(&mock).await.pop().unwrap());
    assert!(sent.starts_with("Review the uncommitted changes"), "{sent}");
    assert!(sent.contains("Pay particular attention to: security"));
}

#[tokio::test]
async fn fork_starts_a_new_session_and_leaves_the_original_alone() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    turn(&mut c, "first").await;
    let root = zarn_core::session::data_root();
    let before = zarn_core::session::list(&root, &env.project());
    assert_eq!(before.len(), 1);
    let original = std::fs::read_to_string(&before[0]).unwrap();
    c.type_line("/fork");
    assert!(notices(&c).last().unwrap().contains("is unchanged"));
    turn(&mut c, "after the fork").await;
    let after = zarn_core::session::list(&root, &env.project());
    assert_eq!(after.len(), 2);
    assert_eq!(std::fs::read_to_string(&before[0]).unwrap(), original);
    let fresh = after.iter().find(|p| **p != before[0]).unwrap();
    let (_, messages) = zarn_core::session::read(fresh).unwrap();
    let text: Vec<String> = messages.iter().map(|m| m.text()).collect();
    assert!(text.iter().any(|t| t == "first") && text.iter().any(|t| t == "after the fork"));
    drop(c);

    let mut c = env.controller_with(zarn_tui::Options { resume: Some(before[0].clone()), fork: true, ..Default::default() });
    assert!(notices(&c).iter().any(|n| n.contains("the original is unchanged")));
    turn(&mut c, "third").await;
    assert_eq!(std::fs::read_to_string(&before[0]).unwrap(), original);
    assert_eq!(zarn_core::session::list(&root, &env.project()).len(), 3);
}

#[tokio::test]
async fn loop_reruns_a_prompt_until_stopped() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    c.type_line("/loop 1s ping the build");
    wait_for(&mut c, Duration::from_secs(6), |_| false).await;
    let pings = chat_bodies(&mock).await.iter().filter(|b| last_user_text(b) == "ping the build").count();
    assert!(pings >= 2, "ran {pings} times");
    c.type_line("/loop");
    assert!(notices(&c).last().unwrap().contains("every 1s  ping the build"));
    c.type_line("/loop stop all");
    c.settle(Duration::from_secs(3)).await;
    let stopped = chat_bodies(&mock).await.len();
    wait_for(&mut c, Duration::from_secs(3), |_| false).await;
    assert_eq!(chat_bodies(&mock).await.len(), stopped);
    c.type_line("/loop 5y nope");
    assert!(notices(&c).last().unwrap().contains("not a time unit"));
}

#[tokio::test]
async fn just_for_me_rules_go_to_config_local_toml() {
    let write = tool_sse("w", "write", &json!({ "path": "mine.txt", "content": "x" }));
    let mock = server(scripted(vec![write, sse("ok")])).await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    c.type_line("write it");
    wait_for(&mut c, Duration::from_secs(5), |c| c.app().pending.is_some()).await;
    c.key(key('l'));
    c.settle(Duration::from_secs(5)).await;
    let local = std::fs::read_to_string(env.project().join(".zarn/config.local.toml")).unwrap();
    assert!(local.contains("allow = [\"write(mine.txt)\"]"), "{local}");
    assert!(!env.project().join(".zarn/config.toml").exists());
    let ignore = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.gitignore")).unwrap();
    assert!(ignore.lines().any(|l| l.trim() == ".zarn/"), "covered by the .zarn/ ignore rule");
}

#[tokio::test]
async fn an_isolated_subagent_works_in_its_own_worktree() {
    let main = tool_sse("t", "task", &json!({ "agent": "builder", "prompt": "make a file" }));
    let n = Arc::new(AtomicUsize::new(0));
    let m = Arc::new(AtomicUsize::new(0));
    let script = move |body: &Value| {
        let system = system_text(body);
        if system.starts_with("Write a title") {
            return (sse("Title"), 0);
        }
        if system.starts_with("You build things") {
            let i = n.fetch_add(1, Ordering::SeqCst);
            return (if i == 0 { tool_sse("w", "write", &json!({ "path": "built.txt", "content": "x" })) } else { sse("built it") }, 0);
        }
        let i = m.fetch_add(1, Ordering::SeqCst);
        (if i == 0 { main.clone() } else { sse("done") }, 0)
    };
    let mock = server(script).await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    repo(&env);
    let agents = env.project().join(".zarn/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("builder.md"), "---\nname: builder\ndescription: builds\nisolation: worktree\n---\nYou build things.\n").unwrap();
    let mut c = env.controller();
    c.type_line("/mode bypass");
    c.key(key('y'));
    turn(&mut c, "delegate").await;
    assert!(!env.project().join("built.txt").exists(), "the main checkout is untouched");
    let trees = env.project().join(".zarn/worktrees/zarn");
    let kept: Vec<_> = std::fs::read_dir(&trees).unwrap().flatten().map(|e| e.path()).collect();
    assert_eq!(kept.len(), 1, "a worktree with changes is kept");
    assert!(kept[0].join("built.txt").exists());
    let result = chat_bodies(&mock).await.into_iter().rev().find(|b| !system_text(b).starts_with("You build")).unwrap().to_string();
    assert!(result.contains("it has changes, so it was kept"), "{result}");
}
