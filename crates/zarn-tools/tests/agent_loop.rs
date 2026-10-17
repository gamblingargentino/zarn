use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zarn_core::agent::{Agent, AgentConfig};
use zarn_core::provider::{Caps, Event, Provider, ProviderError};
use zarn_core::tools::Ctx;
use zarn_core::types::{Message, Request, StopReason};
use zarn_tools::permissions::Permissions;

struct ScriptedProvider {
    turns: Vec<Vec<Event>>,
    counter: Arc<std::sync::atomic::AtomicUsize>,
}

impl ScriptedProvider {
    #[allow(clippy::new_ret_no_self)]
    fn new(turns: Vec<Vec<Event>>) -> Arc<dyn Provider> {
        Arc::new(ScriptedProvider {
            turns,
            counter: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn stream(&self, _req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let turn = self.counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        if turn >= self.turns.len() {
            return Err(ProviderError::Other("unexpected turn".into()));
        }

        let events = self.turns[turn].clone();
        Ok(futures::stream::iter(events).boxed())
    }

    fn caps(&self) -> Caps {
        Caps::default()
    }

    fn count_tokens(&self, _msgs: &[Message]) -> Result<u32, ProviderError> {
        Ok(0)
    }
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zarn-agent-test-{tag}-{}",
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn no_tool_calls_ends_loop_and_appends_assistant_message() {
    let provider = ScriptedProvider::new(vec![vec![
        Event::TextDelta("hello world".to_string()),
        Event::Done(StopReason::EndTurn),
    ]]);

    let tools = Arc::new(zarn_tools::builtins());
    let ctx = Arc::new(Ctx::new("."));
    let config = AgentConfig::default();
    let agent = Agent::new(provider, tools, ctx, config);

    let (tx, _rx) = mpsc::channel(100);
    let mut messages = vec![Message::user("test")];
    let cancel = CancellationToken::new();

    let reason = agent.run(&mut messages, &tx, &cancel).await.unwrap();

    assert_eq!(reason, StopReason::EndTurn);
    assert_eq!(messages.len(), 2);
    match &messages[1] {
        Message::Assistant {
            content,
            tool_calls,
        } => {
            assert!(!content.is_empty());
            assert_eq!(content[0].as_text().unwrap(), "hello world");
            assert!(tool_calls.is_empty());
        }
        _ => panic!("expected assistant message"),
    }
}

#[tokio::test]
async fn read_tool_call_actually_reads_file() {
    let dir = temp_dir("read");
    let file_path = dir.join("test.txt");
    std::fs::write(&file_path, "hello from file").unwrap();

    let provider = ScriptedProvider::new(vec![
        vec![
            Event::ToolCallStart {
                id: "call-1".to_string(),
                name: "read".to_string(),
            },
            Event::ToolCallDelta(format!(
                r#"{{"path":"{}"}}"#,
                file_path.display().to_string().replace('\\', "/")
            )),
            Event::ToolCallEnd,
            Event::Done(StopReason::ToolUse),
        ],
        vec![Event::Done(StopReason::EndTurn)],
    ]);

    let tools = Arc::new(zarn_tools::builtins());
    let ctx = Arc::new(Ctx::new("."));
    let config = AgentConfig::default();
    let agent = Agent::new(provider, tools, ctx, config);

    let (tx, _rx) = mpsc::channel(100);
    let mut messages = vec![Message::user("read the file")];
    let cancel = CancellationToken::new();

    let reason = agent.run(&mut messages, &tx, &cancel).await.unwrap();

    assert_eq!(reason, StopReason::EndTurn);
    assert_eq!(messages.len(), 4);

    match &messages[1] {
        Message::Assistant { tool_calls, .. } => {
            assert_eq!(tool_calls.len(), 1);
            assert_eq!(tool_calls[0].name, "read");
        }
        _ => panic!("expected assistant message with tool calls"),
    }

    match &messages[2] {
        Message::ToolResult {
            content,
            is_error,
            ..
        } => {
            if *is_error {
                panic!("tool error: {}", content);
            }
            assert!(content.contains("hello from file"), "content: {}", content);
        }
        _ => panic!("expected tool result message"),
    }
}

#[tokio::test]
async fn second_turn_receives_tool_results() {
    let dir = temp_dir("second-turn");
    let file_path = dir.join("test.txt");
    std::fs::write(&file_path, "turn 2 file").unwrap();

    let provider = ScriptedProvider::new(vec![
        vec![
            Event::ToolCallStart {
                id: "call-1".to_string(),
                name: "read".to_string(),
            },
            Event::ToolCallDelta(format!(
                r#"{{"path":"{}"}}"#,
                file_path.display().to_string().replace('\\', "/")
            )),
            Event::ToolCallEnd,
            Event::Done(StopReason::ToolUse),
        ],
        vec![
            Event::TextDelta("processed the file".to_string()),
            Event::Done(StopReason::EndTurn),
        ],
    ]);

    let tools = Arc::new(zarn_tools::builtins());
    let ctx = Arc::new(Ctx::new("."));
    let config = AgentConfig::default();
    let agent = Agent::new(provider, tools, ctx, config);

    let (tx, _rx) = mpsc::channel(100);
    let mut messages = vec![Message::user("read then respond")];
    let cancel = CancellationToken::new();

    let reason = agent.run(&mut messages, &tx, &cancel).await.unwrap();

    assert_eq!(reason, StopReason::EndTurn);
    assert_eq!(messages.len(), 4);

    assert!(matches!(&messages[1], Message::Assistant { .. }));
    assert!(matches!(&messages[2], Message::ToolResult { .. }));
    assert!(matches!(&messages[3], Message::Assistant { .. }));

    match &messages[3] {
        Message::Assistant { content, .. } => {
            assert_eq!(content[0].as_text().unwrap(), "processed the file");
        }
        _ => panic!("expected final assistant message"),
    }
}

#[tokio::test]
async fn unknown_tool_name_is_error_not_fatal() {
    let provider = ScriptedProvider::new(vec![
        vec![
            Event::ToolCallStart {
                id: "call-1".to_string(),
                name: "nonexistent_tool".to_string(),
            },
            Event::ToolCallDelta("{}".to_string()),
            Event::ToolCallEnd,
            Event::Done(StopReason::ToolUse),
        ],
        vec![Event::Done(StopReason::EndTurn)],
    ]);

    let tools = Arc::new(zarn_tools::builtins());
    let ctx = Arc::new(Ctx::new("."));
    let config = AgentConfig::default();
    let agent = Agent::new(provider, tools, ctx, config);

    let (tx, _rx) = mpsc::channel(100);
    let mut messages = vec![Message::user("call unknown tool")];
    let cancel = CancellationToken::new();

    let reason = agent.run(&mut messages, &tx, &cancel).await.unwrap();

    assert_eq!(reason, StopReason::EndTurn);

    match &messages[2] {
        Message::ToolResult {
            is_error,
            content,
            ..
        } => {
            assert!(is_error);
            assert!(content.contains("unknown tool"));
        }
        _ => panic!("expected tool result"),
    }
}

#[tokio::test]
async fn max_iterations_guard_stops_infinite_loop() {
    let provider = ScriptedProvider::new(vec![
        vec![
            Event::ToolCallStart {
                id: "call-1".to_string(),
                name: "read".to_string(),
            },
            Event::ToolCallDelta("{}".to_string()),
            Event::ToolCallEnd,
            Event::Done(StopReason::ToolUse),
        ];
        33
    ]);

    let tools = Arc::new(zarn_tools::builtins());
    let ctx = Arc::new(Ctx::new("."));
    let config = AgentConfig {
        max_iterations: 3,
        ..AgentConfig::default()
    };
    let agent = Agent::new(provider, tools, ctx, config);

    let (tx, _rx) = mpsc::channel(100);
    let mut messages = vec![Message::user("test")];
    let cancel = CancellationToken::new();

    let reason = agent.run(&mut messages, &tx, &cancel).await.unwrap();

    assert_eq!(reason, StopReason::MaxTokens);
}

#[tokio::test]
async fn write_denied_when_headless_permissions() {
    let dir = temp_dir("write-denied");
    let file_path = dir.join("protected.txt");

    let provider = ScriptedProvider::new(vec![
        vec![
            Event::ToolCallStart {
                id: "call-1".to_string(),
                name: "write".to_string(),
            },
            Event::ToolCallDelta(format!(
                r#"{{"path": "{}", "content": "new content"}}"#,
                file_path.to_string_lossy()
            )),
            Event::ToolCallEnd,
            Event::Done(StopReason::ToolUse),
        ],
        vec![Event::Done(StopReason::EndTurn)],
    ]);

    let tools = Arc::new(zarn_tools::builtins());
    let ctx = Arc::new(Ctx::new("."));
    let config = AgentConfig::default();

    let gate = Arc::new(Permissions::headless());
    let agent = Agent::new(provider, tools, ctx, config).with_gate(gate);

    let (tx, _rx) = mpsc::channel(100);
    let mut messages = vec![Message::user("write to file")];
    let cancel = CancellationToken::new();

    agent.run(&mut messages, &tx, &cancel).await.unwrap();

    match &messages[2] {
        Message::ToolResult {
            is_error,
            content,
            ..
        } => {
            assert!(is_error);
            assert!(content.contains("denied"));
        }
        _ => panic!("expected tool result"),
    }

    assert!(!file_path.exists(), "file should not be created");
}

#[tokio::test]
async fn edit_denied_when_headless_permissions() {
    let dir = temp_dir("edit-denied");
    let file_path = dir.join("protected.txt");
    std::fs::write(&file_path, "original content").unwrap();

    let provider = ScriptedProvider::new(vec![
        vec![
            Event::ToolCallStart {
                id: "call-1".to_string(),
                name: "edit".to_string(),
            },
            Event::ToolCallDelta(format!(
                r#"{{"path": "{}", "old_string": "original", "new_string": "modified"}}"#,
                file_path.to_string_lossy()
            )),
            Event::ToolCallEnd,
            Event::Done(StopReason::ToolUse),
        ],
        vec![Event::Done(StopReason::EndTurn)],
    ]);

    let tools = Arc::new(zarn_tools::builtins());
    let ctx = Arc::new(Ctx::new("."));
    let config = AgentConfig::default();

    let gate = Arc::new(Permissions::headless());
    let agent = Agent::new(provider, tools, ctx, config).with_gate(gate);

    let (tx, _rx) = mpsc::channel(100);
    let mut messages = vec![Message::user("edit file")];
    let cancel = CancellationToken::new();

    agent.run(&mut messages, &tx, &cancel).await.unwrap();

    match &messages[2] {
        Message::ToolResult { is_error, .. } => {
            assert!(is_error);
        }
        _ => panic!("expected tool result"),
    }

    let contents = std::fs::read_to_string(&file_path).unwrap();
    assert_eq!(contents, "original content", "file should not be modified");
}

struct StallingProvider;

#[async_trait]
impl Provider for StallingProvider {
    async fn stream(&self, _req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let started = futures::stream::iter(vec![
            Event::TextDelta("thinking ".into()),
            Event::TextDelta("out loud".into()),
        ]);
        Ok(started.chain(futures::stream::pending()).boxed())
    }

    fn caps(&self) -> Caps {
        Caps::default()
    }

    fn count_tokens(&self, _msgs: &[Message]) -> Result<u32, ProviderError> {
        Ok(0)
    }
}

#[tokio::test]
async fn cancelling_mid_stream_keeps_the_text_that_already_arrived() {
    let dir = temp_dir("cancel");
    let agent = Agent::new(
        Arc::new(StallingProvider),
        Arc::new(zarn_tools::builtins()),
        Arc::new(Ctx::new(&dir)),
        AgentConfig {
            model: "stub".into(),
            ..AgentConfig::default()
        },
    );

    let (tx, mut rx) = mpsc::channel(256);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });

    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        trigger.cancel();
    });

    let mut messages = vec![Message::user("go")];
    let stop = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        agent.run(&mut messages, &tx, &cancel),
    )
    .await
    .expect("cancellation must end the run, not hang")
    .unwrap();

    drop(tx);
    let _ = drain.await;

    assert_eq!(stop, StopReason::Cancelled);

    let partial = messages
        .iter()
        .rev()
        .find(|m| matches!(m, Message::Assistant { .. }))
        .expect("the partial assistant turn must be kept so the transcript is not lost");
    assert_eq!(partial.text(), "thinking out loud");

    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn a_tool_call_cut_off_by_the_output_limit_is_not_run() {
    let dir = temp_dir("cut");
    let target = dir.join("made.txt").display().to_string().replace('\\', "/");
    let first_part = format!(r#"{{"path":"{target}","content":"half of a big fi"#);
    let provider = ScriptedProvider::new(vec![
        vec![
            Event::ToolCallStart { id: "whole".into(), name: "write".into() },
            Event::ToolCallDelta(format!(r#"{{"path":"{target}.first","content":"done"}}"#)),
            Event::ToolCallEnd,
            Event::ToolCallStart { id: "cut".into(), name: "write".into() },
            Event::ToolCallDelta(first_part),
            Event::Done(StopReason::MaxTokens),
        ],
        vec![
            Event::ToolCallStart { id: "closed-but-cut".into(), name: "write".into() },
            Event::ToolCallDelta(format!(r#"{{"path":"{target}","content":"also cut"}}"#)),
            Event::ToolCallEnd,
            Event::Done(StopReason::MaxTokens),
        ],
        vec![Event::TextDelta("I will split it".into()), Event::Done(StopReason::EndTurn)],
    ]);
    let tools = Arc::new(zarn_tools::builtins());
    let ctx = Arc::new(Ctx::new(&dir));
    let config = AgentConfig { max_tokens: 1000, ..AgentConfig::default() };
    let agent = Agent::new(provider, tools, ctx, config).with_gate(Arc::new(Permissions::headless().with_tool("write", zarn_tools::permissions::Policy::Allow)));
    let (tx, mut rx) = mpsc::channel(100);
    let mut messages = vec![Message::user("write a big file")];
    let reason = agent.run(&mut messages, &tx, &CancellationToken::new()).await.unwrap();
    assert_eq!(reason, StopReason::EndTurn);
    assert!(std::path::Path::new(&format!("{target}.first")).exists(), "a complete call before the cut still runs");
    assert!(!std::path::Path::new(&target).exists(), "a cut-off call never runs");

    let results: Vec<(&str, &str, bool)> = messages
        .iter()
        .filter_map(|m| match m {
            Message::ToolResult { id, content, is_error } => Some((id.as_str(), content.as_str(), *is_error)),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 3, "every call gets an answer so the conversation stays valid");
    for (id, content, is_error) in &results[1..] {
        assert!(is_error, "{id}");
        assert!(content.contains("cut off") && content.contains("1000 tokens"), "{id}: {content}");
    }
    drop(tx);
    let mut finished_cut = 0;
    while let Some(event) = rx.recv().await {
        if let zarn_core::agent::AgentEvent::ToolFinished { id, output, .. } = event
            && id != "whole"
        {
            assert!(output.is_error);
            finished_cut += 1;
        }
    }
    assert_eq!(finished_cut, 2, "the UI shows each cut-off call as failed");
}
