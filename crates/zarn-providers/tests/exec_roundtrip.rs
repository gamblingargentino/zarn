use futures::stream::StreamExt;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;
use zarn_core::provider::{Event, Provider, ProviderError};
use zarn_core::types::{Message, Request, StopReason};
use zarn_providers::ExecProvider;

fn bash_available() -> bool {
    std::process::Command::new("bash")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn write_script(dir: &TempDir, name: &str, content: &str) -> PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, content).expect("failed to write script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .expect("failed to chmod script");
    }
    path
}

fn test_request() -> Request {
    Request::new("test-model", vec![Message::user("test message")])
}

#[tokio::test]
async fn test_happy_path() {
    if !bash_available() {
        return;
    }

    let dir = TempDir::new().expect("failed to create temp dir");
    let script = write_script(
        &dir,
        "happy.sh",
        r#"#!/bin/bash
read
echo '{"TextDelta":"hello"}'
echo '{"TextDelta":" "}'
echo '{"TextDelta":"world"}'
echo '{"Done":"EndTurn"}'
"#,
    );

    let provider = ExecProvider::new(
        "bash",
        vec![script.to_string_lossy().to_string()],
    );
    let mut stream = provider
        .stream(test_request())
        .await
        .expect("failed to create stream");

    let event1 = stream.next().await.expect("expected first event");
    assert_eq!(event1, Event::TextDelta("hello".to_string()));

    let event2 = stream.next().await.expect("expected second event");
    assert_eq!(event2, Event::TextDelta(" ".to_string()));

    let event3 = stream.next().await.expect("expected third event");
    assert_eq!(event3, Event::TextDelta("world".to_string()));

    let event4 = stream.next().await.expect("expected done event");
    assert_eq!(event4, Event::Done(StopReason::EndTurn));

    assert!(stream.next().await.is_none(), "stream should end after Done");
}

#[tokio::test]
async fn test_adapter_reads_request() {
    if !bash_available() {
        return;
    }

    let dir = TempDir::new().expect("failed to create temp dir");
    let script = write_script(
        &dir,
        "echo_model.sh",
        r#"#!/bin/bash
read -r line
echo "$line" | grep -q '"test-model"'
if [ $? -eq 0 ]; then
    echo '{"TextDelta":"received test-model"}'
else
    echo '{"TextDelta":"model not found"}'
fi
echo '{"Done":"EndTurn"}'
"#,
    );

    let provider = ExecProvider::new(
        "bash",
        vec![script.to_string_lossy().to_string()],
    );
    let mut stream = provider
        .stream(test_request())
        .await
        .expect("failed to create stream");

    let event1 = stream.next().await.expect("expected first event");
    assert_eq!(event1, Event::TextDelta("received test-model".to_string()));

    let event2 = stream.next().await.expect("expected done event");
    assert_eq!(event2, Event::Done(StopReason::EndTurn));
}

#[tokio::test]
async fn test_no_done_sends_error() {
    if !bash_available() {
        return;
    }

    let dir = TempDir::new().expect("failed to create temp dir");
    let script = write_script(
        &dir,
        "no_done.sh",
        r#"#!/bin/bash
read
echo '{"TextDelta":"hello"}'
exit 0
"#,
    );

    let provider = ExecProvider::new(
        "bash",
        vec![script.to_string_lossy().to_string()],
    );
    let mut stream = provider
        .stream(test_request())
        .await
        .expect("failed to create stream");

    let event1 = stream.next().await.expect("expected text delta event");
    assert_eq!(event1, Event::TextDelta("hello".to_string()));

    let event2 = stream.next().await.expect("expected error event");
    match event2 {
        Event::Error(ProviderError::Other(msg)) => {
            assert!(
                msg.contains("ended without sending Done"),
                "error should mention missing Done: {}",
                msg
            );
        }
        _ => panic!("expected Error event, got {:?}", event2),
    }
}

#[tokio::test]
async fn test_malformed_line_skipped() {
    if !bash_available() {
        return;
    }

    let dir = TempDir::new().expect("failed to create temp dir");
    let script = write_script(
        &dir,
        "malformed.sh",
        r#"#!/bin/bash
read
echo '{"TextDelta":"first"}'
echo 'this is not valid json at all'
echo '{"TextDelta":"second"}'
echo '{"Done":"EndTurn"}'
"#,
    );

    let provider = ExecProvider::new(
        "bash",
        vec![script.to_string_lossy().to_string()],
    );
    let mut stream = provider
        .stream(test_request())
        .await
        .expect("failed to create stream");

    let event1 = stream.next().await.expect("expected first event");
    assert_eq!(event1, Event::TextDelta("first".to_string()));

    let event2 = stream.next().await.expect("expected second event after skipped malformed");
    assert_eq!(event2, Event::TextDelta("second".to_string()));

    let event3 = stream.next().await.expect("expected done event");
    assert_eq!(event3, Event::Done(StopReason::EndTurn));

    assert!(stream.next().await.is_none(), "stream should end after Done");
}

#[tokio::test]
async fn test_stderr_and_exit_code() {
    if !bash_available() {
        return;
    }

    let dir = TempDir::new().expect("failed to create temp dir");
    let script = write_script(
        &dir,
        "stderr_exit.sh",
        r#"#!/bin/bash
read
echo '{"TextDelta":"output"}'
echo "error message" >&2
exit 42
"#,
    );

    let provider = ExecProvider::new(
        "bash",
        vec![script.to_string_lossy().to_string()],
    );
    let mut stream = provider
        .stream(test_request())
        .await
        .expect("failed to create stream");

    let event1 = stream.next().await.expect("expected text event");
    assert_eq!(event1, Event::TextDelta("output".to_string()));

    let event2 = stream.next().await.expect("expected error event");
    match event2 {
        Event::Error(ProviderError::Other(msg)) => {
            assert!(
                msg.contains("exit code 42"),
                "error should include exit code: {}",
                msg
            );
            assert!(
                msg.contains("error message"),
                "error should include stderr: {}",
                msg
            );
            assert!(
                msg.contains("ended without sending Done"),
                "error should mention missing Done: {}",
                msg
            );
        }
        _ => panic!("expected Error event, got {:?}", event2),
    }

    assert!(stream.next().await.is_none(), "stream should end after error");
}

#[tokio::test]
async fn test_tool_call_events() {
    if !bash_available() {
        return;
    }

    let dir = TempDir::new().expect("failed to create temp dir");
    let script = write_script(
        &dir,
        "tool_calls.sh",
        r#"#!/bin/bash
read
echo '{"ToolCallStart":{"id":"call_1","name":"read_file"}}'
echo '{"ToolCallDelta":"{\"path\":"}'
echo '{"ToolCallDelta":"\"/etc/passwd\"}"}'
echo '"ToolCallEnd"'
echo '{"Done":"EndTurn"}'
"#,
    );

    let provider = ExecProvider::new(
        "bash",
        vec![script.to_string_lossy().to_string()],
    );
    let mut stream = provider
        .stream(test_request())
        .await
        .expect("failed to create stream");

    let event1 = stream.next().await.expect("expected ToolCallStart");
    match event1 {
        Event::ToolCallStart { id, name } => {
            assert_eq!(id, "call_1");
            assert_eq!(name, "read_file");
        }
        _ => panic!("expected ToolCallStart, got {:?}", event1),
    }

    let event2 = stream.next().await.expect("expected first ToolCallDelta");
    assert_eq!(event2, Event::ToolCallDelta(r#"{"path":"#.to_string()));

    let event3 = stream.next().await.expect("expected second ToolCallDelta");
    assert_eq!(event3, Event::ToolCallDelta(r#""/etc/passwd"}"#.to_string()));

    let event4 = stream.next().await.expect("expected ToolCallEnd");
    assert_eq!(event4, Event::ToolCallEnd);

    let event5 = stream.next().await.expect("expected Done");
    assert_eq!(event5, Event::Done(StopReason::EndTurn));
}

#[tokio::test]
async fn test_usage_event() {
    if !bash_available() {
        return;
    }

    let dir = TempDir::new().expect("failed to create temp dir");
    let script = write_script(
        &dir,
        "usage.sh",
        r#"#!/bin/bash
read
echo '{"Usage":{"input":100,"output":50,"cached":10}}'
echo '{"Done":"EndTurn"}'
"#,
    );

    let provider = ExecProvider::new(
        "bash",
        vec![script.to_string_lossy().to_string()],
    );
    let mut stream = provider
        .stream(test_request())
        .await
        .expect("failed to create stream");

    let event1 = stream.next().await.expect("expected Usage event");
    match event1 {
        Event::Usage {
            input,
            output,
            cached,
        } => {
            assert_eq!(input, 100);
            assert_eq!(output, 50);
            assert_eq!(cached, 10);
        }
        _ => panic!("expected Usage event, got {:?}", event1),
    }

    let event2 = stream.next().await.expect("expected Done");
    assert_eq!(event2, Event::Done(StopReason::EndTurn));
}
