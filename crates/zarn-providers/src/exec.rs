use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use zarn_core::provider::{Caps, Event, Provider, ProviderError};
use zarn_core::types::{Message, Request};

pub struct ExecProvider {
    command: String,
    args: Vec<String>,
    caps: Caps,
}

impl ExecProvider {
    pub fn new(command: impl Into<String>, args: Vec<String>) -> Self {
        ExecProvider {
            command: command.into(),
            args,
            caps: Caps::default(),
        }
    }

    pub fn with_caps(mut self, caps: Caps) -> Self {
        self.caps = caps;
        self
    }
}

#[async_trait]
impl Provider for ExecProvider {
    async fn stream(&self, req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let mut cmd = Command::new(&self.command);
        cmd.args(&self.args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| {
            ProviderError::Other(format!("failed to spawn command: {}", e))
        })?;

        let Some(mut stdin) = child.stdin.take() else {
            return Err(ProviderError::Other("child has no stdin".into()));
        };
        let Some(stdout) = child.stdout.take() else {
            return Err(ProviderError::Other("child has no stdout".into()));
        };
        let stderr = child.stderr.take();

        let req_json = serde_json::to_string(&req).map_err(|e| {
            ProviderError::BadRequest(format!("failed to serialize request: {}", e))
        })?;

        stdin.write_all(req_json.as_bytes()).await.map_err(|e| {
            ProviderError::Network(format!("failed to write to stdin: {}", e))
        })?;
        stdin.write_all(b"\n").await.map_err(|e| {
            ProviderError::Network(format!("failed to write newline: {}", e))
        })?;
        drop(stdin);

        let (tx, rx) = mpsc::channel(64);
        let command = self.command.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            let mut saw_done = false;

            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }

                match serde_json::from_str::<Event>(&line) {
                    Ok(event) => {
                        if matches!(event, Event::Done(_)) {
                            saw_done = true;
                        }
                        if tx.send(event).await.is_err() {
                            let _ = child.start_kill();
                            return;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(adapter = %command, %e, line = %line, "skipping unparseable adapter line");
                    }
                }
            }

            if saw_done {
                let _ = child.wait().await;
                return;
            }

            let mut complaint = format!("adapter {command} ended without sending Done");
            if let Ok(status) = child.wait().await
                && !status.success()
            {
                match status.code() {
                    Some(code) => complaint.push_str(&format!(" (exit code {code})")),
                    None => complaint.push_str(" (killed by signal)"),
                }
            }
            if let Some(stderr) = stderr {
                let mut text = String::new();
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    text.push_str(&line);
                    text.push('\n');
                }
                if !text.trim().is_empty() {
                    complaint.push_str(&format!("\nstderr:\n{}", text.trim_end()));
                }
            }
            let _ = tx.send(Event::Error(ProviderError::Other(complaint))).await;
        });

        Ok(ReceiverStream::new(rx).boxed())
    }

    fn caps(&self) -> Caps {
        self.caps
    }

    fn count_tokens(&self, msgs: &[Message]) -> Result<u32, ProviderError> {
        let bpe = tiktoken_rs::cl100k_base()
            .map_err(|e| ProviderError::Other(format!("tokenizer unavailable: {}", e)))?;
        let text = msgs.iter().map(Message::text).collect::<Vec<_>>().join("\n");
        Ok(bpe.encode_with_special_tokens(&text).len() as u32)
    }
}
