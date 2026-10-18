#![cfg(unix)]

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use zarn_config::hooks::{HookSpec, HookedGate, Hooks};
use zarn_core::agent::{Agent, AgentConfig, GateDecision, ToolGate};
use zarn_core::provider::{Caps, Event, Provider, ProviderError};
use zarn_core::tools::{Ctx, Risk, ToolRegistry};
use zarn_core::types::{Message, Request, StopReason};
use zarn_tools::permissions::{Overlay, Permissions};

fn script(dir: &Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    format!("sh {}", path.display())
}

fn allow_all() -> Arc<dyn ToolGate> {
    Arc::new(Permissions::headless().with_overlay(Overlay {
        name: "bypass".into(),
        allow_all: true,
        ..Overlay::default()
    }))
}

fn gate(dir: &Path, hooks: Hooks) -> HookedGate {
    HookedGate::new(allow_all(), Arc::new(RwLock::new(hooks)), dir)
}

fn pre(matcher: &str, command: String) -> Hooks {
    Hooks {
        pre_tool: vec![HookSpec {
            matcher: matcher.into(),
            command,
        }],
        ..Hooks::default()
    }
}

#[tokio::test]
async fn a_failing_pre_tool_hook_blocks_with_its_stderr_and_sees_the_event_as_json() {
    let dir = tempfile::tempdir().unwrap();
    let seen = dir.path().join("seen.json");
    let command = script(
        dir.path(),
        "guard.sh",
        &format!("cat > {}\necho 'no writes to lockfiles' >&2\nexit 2\n", seen.display()),
    );
    let gate = gate(dir.path(), pre("edit|write", command));
    let decision = gate
        .check("write", Risk::Write, &json!({ "path": "Cargo.lock", "content": "x" }))
        .await;
    let GateDecision::Deny(reason) = decision else {
        panic!("expected a block");
    };
    assert!(reason.contains("no writes to lockfiles"), "{reason}");
    assert!(reason.contains("exited with 2"));
    let event: Value = serde_json::from_str(&std::fs::read_to_string(&seen).unwrap()).unwrap();
    assert_eq!(event["event"], "pre_tool");
    assert_eq!(event["tool"], "write");
    assert_eq!(event["args"]["path"], "Cargo.lock");
    assert_eq!(event["cwd"], dir.path().display().to_string());
}

#[tokio::test]
async fn a_passing_hook_lets_the_permission_policy_decide_and_unmatched_tools_skip_it() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let command = script(dir.path(), "ok.sh", &format!("touch {}\nexit 0\n", marker.display()));
    let gate = gate(dir.path(), pre("edit|write", command));
    assert_eq!(gate.check("bash", Risk::Execute, &json!({})).await, GateDecision::Allow);
    assert!(!marker.exists(), "bash does not match edit|write");
    assert_eq!(gate.check("edit", Risk::Write, &json!({})).await, GateDecision::Allow);
    assert!(marker.exists());
}

#[tokio::test]
async fn a_hook_that_hangs_times_out_and_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let mut hooks = pre("", script(dir.path(), "slow.sh", "sleep 30\n"));
    hooks.timeout_secs = Some(1);
    let started = Instant::now();
    let decision = gate(dir.path(), hooks).check("bash", Risk::Execute, &json!({})).await;
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(matches!(decision, GateDecision::Deny(r) if r.contains("timed out")));
}

#[tokio::test]
async fn hook_output_is_redacted() {
    zarn_config::auth::register_secret("test-key-not-real-hook-output");
    let dir = tempfile::tempdir().unwrap();
    let command = script(dir.path(), "leak.sh", "echo 'key test-key-not-real-hook-output' >&2\nexit 1\n");
    let GateDecision::Deny(reason) = gate(dir.path(), pre("", command))
        .check("bash", Risk::Execute, &json!({}))
        .await
    else {
        panic!("expected a block");
    };
    assert!(!reason.contains("test-key-not-real-hook-output"));
    assert!(reason.contains("[redacted]"));
}

struct Scripted {
    next: AtomicUsize,
}

#[async_trait]
impl Provider for Scripted {
    async fn stream(&self, _req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let events = if self.next.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                Event::ToolCallStart {
                    id: "w".into(),
                    name: "write".into(),
                },
                Event::ToolCallDelta(json!({ "path": "out.txt", "content": "x" }).to_string()),
                Event::ToolCallEnd,
                Event::Done(StopReason::ToolUse),
            ]
        } else {
            vec![Event::TextDelta("ok".into()), Event::Done(StopReason::EndTurn)]
        };
        Ok(futures::stream::iter(events).boxed())
    }

    fn caps(&self) -> Caps {
        Caps::default()
    }

    fn count_tokens(&self, _msgs: &[Message]) -> Result<u32, ProviderError> {
        Ok(0)
    }
}

#[tokio::test]
async fn the_model_receives_the_hooks_reason_as_the_tool_result() {
    let dir = tempfile::tempdir().unwrap();
    let command = script(dir.path(), "deny.sh", "echo 'use the formatter instead' >&2\nexit 1\n");
    let mut registry = ToolRegistry::new();
    for tool in zarn_tools::builtin_list() {
        registry.register(tool);
    }
    let agent = Agent::new(
        Arc::new(Scripted {
            next: AtomicUsize::new(0),
        }),
        Arc::new(registry),
        Arc::new(Ctx::new(dir.path())),
        AgentConfig::default(),
    )
    .with_gate(Arc::new(gate(dir.path(), pre("write", command))));
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let mut messages = vec![Message::user("write it")];
    agent.run(&mut messages, &tx, &CancellationToken::new()).await.unwrap();
    let result = messages
        .iter()
        .find_map(|m| match m {
            Message::ToolResult { content, is_error, .. } => Some((content.clone(), *is_error)),
            _ => None,
        })
        .unwrap();
    assert!(result.1);
    assert!(result.0.contains("use the formatter instead"));
    assert!(!PathBuf::from(dir.path()).join("out.txt").exists());
}

#[tokio::test]
async fn other_events_fire_with_their_payload() {
    let dir = tempfile::tempdir().unwrap();
    let seen = dir.path().join("post.json");
    let hooks = Hooks {
        post_tool: vec![HookSpec {
            matcher: "read".into(),
            command: script(dir.path(), "post.sh", &format!("cat > {}\n", seen.display())),
        }],
        ..Hooks::default()
    };
    let outcomes = zarn_config::hooks::fire(
        &hooks,
        "post_tool",
        Some("read"),
        json!({ "output": "file body", "is_error": false }),
        dir.path(),
    )
    .await;
    assert!(outcomes[0].ok());
    let event: Value = serde_json::from_str(&std::fs::read_to_string(&seen).unwrap()).unwrap();
    assert_eq!(event["event"], "post_tool");
    assert_eq!(event["output"], "file body");
}
