use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zarn_core::agent::{Agent, AgentConfig};
use zarn_core::provider::{Caps, Event, Provider, ProviderError};
use zarn_core::tools::{Ctx, ToolRegistry};
use zarn_core::types::{Message, Request, StopReason};
use zarn_tools::exit_plan::{ExitPlan, PlanApprover, PlanVerdict};
use zarn_tools::permissions::{Overlay, Permissions, Policy};

struct Scripted {
    turns: Vec<Vec<Event>>,
    next: AtomicUsize,
}

#[async_trait]
impl Provider for Scripted {
    async fn stream(&self, _req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let i = self.next.fetch_add(1, Ordering::SeqCst);
        let events = self.turns.get(i).cloned().unwrap_or_else(|| {
            vec![Event::TextDelta("done".into()), Event::Done(StopReason::EndTurn)]
        });
        Ok(futures::stream::iter(events).boxed())
    }

    fn caps(&self) -> Caps {
        Caps::default()
    }

    fn count_tokens(&self, _msgs: &[Message]) -> Result<u32, ProviderError> {
        Ok(0)
    }
}

fn call(id: &str, name: &str, args: Value) -> Vec<Event> {
    vec![
        Event::ToolCallStart {
            id: id.into(),
            name: name.into(),
        },
        Event::ToolCallDelta(args.to_string()),
        Event::ToolCallEnd,
        Event::Done(StopReason::ToolUse),
    ]
}

fn plan_overlay() -> Overlay {
    Overlay {
        name: "plan".into(),
        safe_only: true,
        ..Overlay::default()
    }
}

fn accept_edits() -> Overlay {
    Overlay {
        name: "accept-edits".into(),
        policies: HashMap::from([
            ("edit".to_string(), Policy::Allow),
            ("write".to_string(), Policy::Allow),
        ]),
        ..Overlay::default()
    }
}

struct Approver {
    gate: Arc<Permissions>,
    verdict: PlanVerdict,
    seen: Mutex<Vec<String>>,
}

#[async_trait]
impl PlanApprover for Approver {
    async fn review(&self, plan: &str) -> PlanVerdict {
        self.seen.lock().unwrap().push(plan.to_string());
        if let PlanVerdict::Approve(_) = &self.verdict {
            self.gate.set_overlay(accept_edits());
        }
        self.verdict.clone()
    }
}

async fn run(verdict: PlanVerdict) -> (tempfile::TempDir, Vec<Message>, Arc<Approver>, Arc<Permissions>) {
    let dir = tempfile::tempdir().unwrap();
    let gate = Arc::new(Permissions::headless().with_overlay(plan_overlay()));
    let approver = Arc::new(Approver {
        gate: gate.clone(),
        verdict,
        seen: Mutex::new(Vec::new()),
    });
    let mut registry = ToolRegistry::new();
    for tool in zarn_tools::builtin_list() {
        registry.register(tool);
    }
    registry.register(Box::new(ExitPlan::new(approver.clone())));
    let provider = Arc::new(Scripted {
        turns: vec![
            call("1", "write", json!({ "path": "out.txt", "content": "early" })),
            call("2", "exit_plan", json!({ "plan": "1. write out.txt" })),
            call("3", "write", json!({ "path": "out.txt", "content": "after approval" })),
        ],
        next: AtomicUsize::new(0),
    });
    let agent = Agent::new(
        provider,
        Arc::new(registry),
        Arc::new(Ctx::new(dir.path())),
        AgentConfig::default(),
    )
    .with_gate(gate.clone());
    let (tx, mut rx) = mpsc::channel(256);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let mut messages = vec![Message::user("add out.txt")];
    agent
        .run(&mut messages, &tx, &CancellationToken::new())
        .await
        .unwrap();
    (dir, messages, approver, gate)
}

fn results(messages: &[Message]) -> Vec<(String, bool)> {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::ToolResult {
                content, is_error, ..
            } => Some((content.clone(), *is_error)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn plan_mode_denies_write_then_an_approved_plan_switches_mode_and_continues() {
    let (dir, messages, approver, gate) = run(PlanVerdict::Approve("accept-edits".into())).await;
    let results = results(&messages);
    assert!(results[0].1);
    assert!(results[0].0.contains("plan mode"));
    assert!(!results[1].1);
    assert!(results[1].0.contains("accept-edits"));
    assert!(!results[2].1, "{}", results[2].0);
    assert_eq!(approver.seen.lock().unwrap().as_slice(), ["1. write out.txt"]);
    assert_eq!(gate.overlay().name, "accept-edits");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
        "after approval"
    );
}

#[tokio::test]
async fn a_rejected_plan_stays_in_plan_mode_and_passes_the_feedback_back() {
    let (dir, messages, _approver, gate) = run(PlanVerdict::Reject("add tests first".into())).await;
    let results = results(&messages);
    assert!(results[1].1);
    assert!(results[1].0.contains("add tests first"));
    assert!(results[2].1);
    assert!(results[2].0.contains("plan mode"));
    assert_eq!(gate.overlay().name, "plan");
    assert!(!dir.path().join("out.txt").exists());
}
