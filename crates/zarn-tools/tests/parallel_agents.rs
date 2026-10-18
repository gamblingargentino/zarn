use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use zarn_core::agent::{Agent, AgentConfig};
use zarn_core::provider::{Caps, Event, Provider, ProviderError};
use zarn_core::subagent::{AgentDef, Background, JobStatus, TaskState, TaskTool, TaskUpdate};
use zarn_core::tools::{Ctx, ToolRegistry};
use zarn_core::types::{Message, Request, StopReason};

struct Slow {
    delay: Duration,
    reply: String,
    fail: bool,
}

#[async_trait]
impl Provider for Slow {
    async fn stream(&self, _req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        tokio::time::sleep(self.delay).await;
        if self.fail {
            return Err(ProviderError::Other("this agent always fails".into()));
        }
        Ok(futures::stream::iter(vec![
            Event::TextDelta(self.reply.clone()),
            Event::Usage { input: 5, output: 7, cached: 0 },
            Event::Done(StopReason::EndTurn),
        ])
        .boxed())
    }

    fn caps(&self) -> Caps {
        Caps::default()
    }

    fn count_tokens(&self, _msgs: &[Message]) -> Result<u32, ProviderError> {
        Ok(0)
    }
}

struct Lead {
    calls: Vec<(String, serde_json::Value)>,
    turn: Mutex<usize>,
}

#[async_trait]
impl Provider for Lead {
    async fn stream(&self, _req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let turn = {
            let mut t = self.turn.lock().unwrap();
            *t += 1;
            *t
        };
        let mut events = Vec::new();
        if turn == 1 {
            for (id, args) in &self.calls {
                events.push(Event::ToolCallStart { id: id.clone(), name: "task".into() });
                events.push(Event::ToolCallDelta(args.to_string()));
                events.push(Event::ToolCallEnd);
            }
            events.push(Event::Done(StopReason::ToolUse));
        } else {
            events.push(Event::TextDelta("all done".into()));
            events.push(Event::Done(StopReason::EndTurn));
        }
        Ok(futures::stream::iter(events).boxed())
    }

    fn caps(&self) -> Caps {
        Caps::default()
    }

    fn count_tokens(&self, _msgs: &[Message]) -> Result<u32, ProviderError> {
        Ok(0)
    }
}

fn def(name: &str) -> AgentDef {
    AgentDef {
        name: name.into(),
        description: format!("the {name} agent"),
        ..AgentDef::default()
    }
}

fn task_tool(updates: Arc<Mutex<Vec<TaskUpdate>>>) -> TaskTool {
    TaskTool::new(
        vec![def("alpha"), def("beta"), def("broken"), def("sleepy")],
        Arc::new(|d: &AgentDef| -> Arc<dyn Provider> {
            Arc::new(Slow {
                delay: Duration::from_millis(if d.name == "sleepy" { 10_000 } else { 400 }),
                reply: format!("report from {}", d.name),
                fail: d.name == "broken",
            })
        }),
        Arc::new(|_: &[String]| ToolRegistry::new()),
        Arc::new(Ctx::new(".")),
        AgentConfig::default(),
    )
    .with_monitor(Arc::new(move |u| updates.lock().unwrap().push(u)))
}

async fn run_lead(parallel: usize) -> (Vec<Message>, Duration, Vec<TaskUpdate>) {
    let updates = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(task_tool(updates.clone())));
    let lead = Lead {
        calls: vec![
            ("c1".into(), json!({ "agent": "alpha", "prompt": "one" })),
            ("c2".into(), json!({ "agent": "broken", "prompt": "two" })),
            ("c3".into(), json!({ "agent": "beta", "prompt": "three" })),
        ],
        turn: Mutex::new(0),
    };
    let agent = Agent::new(
        Arc::new(lead),
        Arc::new(registry),
        Arc::new(Ctx::new(".")),
        AgentConfig { parallel, ..AgentConfig::default() },
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let mut messages = vec![Message::user("go")];
    let started = Instant::now();
    agent.run(&mut messages, &tx, &CancellationToken::new()).await.unwrap();
    let elapsed = started.elapsed();
    drop(tx);
    drain.await.unwrap();
    let updates = updates.lock().unwrap().clone();
    (messages, elapsed, updates)
}

fn results(messages: &[Message]) -> Vec<(String, String, bool)> {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::ToolResult { id, content, is_error } => Some((id.clone(), content.clone(), *is_error)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn task_calls_in_one_turn_run_concurrently_and_return_in_order() {
    let (messages, elapsed, updates) = run_lead(4).await;
    assert!(elapsed < Duration::from_millis(1000), "three 400ms agents took {elapsed:?}");
    let got = results(&messages);
    assert_eq!(got.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), vec!["c1", "c2", "c3"]);
    assert_eq!(got[0].1, "report from alpha");
    assert!(got[1].2 && got[1].1.contains("broken failed"), "{:?}", got[1]);
    assert!(!got[2].2 && got[2].1 == "report from beta", "a failure does not cancel the others");
    let started = updates.iter().filter(|u| matches!(u.state, TaskState::Started { .. })).count();
    let tokens = updates.iter().any(|u| u.agent == "alpha" && u.state == TaskState::Tokens(7));
    let finished: Vec<_> = updates.iter().filter_map(|u| match u.state {
        TaskState::Finished { ok } => Some((u.agent.clone(), ok)),
        _ => None,
    }).collect();
    assert_eq!(started, 3);
    assert!(tokens, "{updates:?}");
    assert!(finished.contains(&("broken".to_string(), false)));
    assert!(finished.contains(&("beta".to_string(), true)));
}

#[tokio::test]
async fn the_parallel_limit_bounds_concurrency() {
    let (messages, elapsed, _) = run_lead(1).await;
    assert!(elapsed >= Duration::from_millis(1100), "with a limit of 1 they run one by one: {elapsed:?}");
    assert_eq!(results(&messages).len(), 3);
}

#[tokio::test]
async fn background_agents_return_at_once_deliver_later_and_can_be_cancelled() {
    let updates = Arc::new(Mutex::new(Vec::new()));
    let background = Background::new();
    let tool = task_tool(updates.clone()).with_background(background.clone());
    let ctx = Ctx::new(".");
    let started = Instant::now();
    let out = zarn_core::tools::Tool::run(&tool, json!({ "agent": "alpha", "prompt": "later", "background": true }), &ctx)
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_millis(200));
    assert!(out.content.starts_with("started background agent bg-1 (alpha)"), "{}", out.content);
    assert_eq!(background.running(), 1);
    assert!(background.take_finished().is_empty());

    let slow = zarn_core::tools::Tool::run(&tool, json!({ "agent": "sleepy", "prompt": "x", "background": true }), &ctx)
        .await
        .unwrap();
    assert!(slow.content.contains("bg-2"));
    tokio::time::sleep(Duration::from_millis(700)).await;
    let done = background.take_finished();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].id, "bg-1");
    assert_eq!(done[0].status, JobStatus::Done);
    assert_eq!(done[0].result, "report from alpha");
    let text = zarn_core::subagent::deliver(&done).unwrap();
    assert!(text.contains("<background_result id=\"bg-1\" agent=\"alpha\" status=\"done\">\nreport from alpha\n</background_result>"));
    assert!(background.take_finished().is_empty(), "delivered once");

    assert_eq!(background.cancel("bg-2").unwrap(), "cancelled bg-2 (sleepy)");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let list = background.list();
    assert_eq!(list[1].status, JobStatus::Cancelled);
    assert!(background.cancel("bg-2").unwrap_err().contains("already cancelled"));
    assert!(background.cancel("bg-9").is_err());
    assert_eq!(background.running(), 0);
}
