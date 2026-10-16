use crate::budget;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use zarn_core::tools::{Ctx, Risk, Tool, ToolOutput};
use zarn_core::types::ToolSpec;

const DEFAULT_TIMEOUT_SECS: u64 = 120;

pub struct Bash {
    sandbox: bool,
    shells: Option<Arc<crate::shells::Shells>>,
}

impl Bash {
    pub fn new() -> Self {
        Bash { sandbox: false, shells: None }
    }

    pub fn sandboxed() -> Self {
        Bash { sandbox: true, shells: None }
    }

    pub fn with_shells(mut self, shells: Arc<crate::shells::Shells>) -> Self {
        self.shells = Some(shells);
        self
    }
}

const SECRET_NAMES: &[&str] = &[
    "KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL", "AUTH", "COOKIE", "SESSION",
    "PRIVATE",
];

pub fn looks_secret(name: &str, value: &str) -> bool {
    let upper = name.to_uppercase();
    if SECRET_NAMES.iter().any(|s| upper.contains(s)) {
        return true;
    }
    let v = value.trim();
    ["sk-", "sk_", "gsk_", "AIza", "ghp_", "gho_", "github_pat_", "xox", "glpat-"]
        .iter()
        .any(|prefix| v.starts_with(prefix) && v.len() >= 20)
}

pub fn scrubbed_names() -> Vec<String> {
    std::env::vars()
        .filter(|(name, value)| looks_secret(name, value))
        .map(|(name, _)| name)
        .collect()
}

impl Default for Bash {
    fn default() -> Self {
        Self::new()
    }
}

fn shell() -> (&'static str, &'static str) {
    if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("bash", "-c")
    }
}

#[cfg(unix)]
fn kill_group(child: &mut tokio::process::Child) {
    match child.id() {
        Some(pid) => unsafe {
            libc::killpg(pid as i32, libc::SIGKILL);
        },
        None => {
            let _ = child.start_kill();
        }
    }
}

#[cfg(windows)]
fn kill_group(child: &mut tokio::process::Child) {
    match child.id() {
        Some(pid) => {
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        None => {
            let _ = child.start_kill();
        }
    }
}

#[async_trait]
impl Tool for Bash {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".to_string(),
            description:
                "Run a shell command in the working directory and return its merged stdout and stderr."
                    .to_string(),
            schema: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command to run"
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (default 120)"
                    },
                    "background": {
                        "type": "boolean",
                        "description": "Start the command in the background and return an id at once; read it with bash_output and stop it with bash_kill"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Execute
    }

    async fn run(&self, args: Value, ctx: &Ctx) -> Result<ToolOutput> {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("command is required"))?;
        let seconds = args
            .get("timeout")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        let (program, flag) = shell();
        let mut builder = Command::new(program);
        builder
            .arg(flag)
            .arg(command)
            .current_dir(ctx.cwd())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        #[cfg(unix)]
        builder.process_group(0);

        if self.sandbox {
            for name in scrubbed_names() {
                builder.env_remove(name);
            }
            builder.env("ZARN_SANDBOX", "1");
        }

        if args.get("background").and_then(Value::as_bool).unwrap_or(false) {
            let Some(shells) = &self.shells else {
                return Ok(ToolOutput::error("background commands are not available here"));
            };
            return Ok(match shells.start(command, builder) {
                Ok(id) => ToolOutput::ok(format!(
                    "started {id} in the background: {command}\nread its output with bash_output (id {id}) and stop it with bash_kill"
                )),
                Err(err) => ToolOutput::error(format!("cannot run {program}: {err}")),
            });
        }

        let mut child = match builder.spawn() {
            Ok(child) => child,
            Err(err) => return Ok(ToolOutput::error(format!("cannot run {program}: {err}"))),
        };

        let merged = Arc::new(Mutex::new(String::new()));
        let mut readers = Vec::new();
        let progress_owner = crate::progress::owner(ctx);

        for pipe in [
            child.stdout.take().map(PipeKind::Out),
            child.stderr.take().map(PipeKind::Err),
        ]
        .into_iter()
        .flatten()
        {
            let sink = merged.clone();
            let command_text = command.to_string();
            readers.push(tokio::spawn(async move {
                let mut lines = match pipe {
                    PipeKind::Out(out) => BufReader::new(Box::pin(out) as PinnedRead).lines(),
                    PipeKind::Err(err) => BufReader::new(Box::pin(err) as PinnedRead).lines(),
                };
                while let Ok(Some(line)) = lines.next_line().await {
                    crate::progress::emit(progress_owner, &command_text, &line);
                    if let Ok(mut sink) = sink.lock() {
                        sink.push_str(&line);
                        sink.push('\n');
                    }
                }
            }));
        }

        let timeout = tokio::time::Duration::from_secs(seconds);
        let status = match tokio::time::timeout(timeout, child.wait()).await {
            Ok(status) => status,
            Err(_) => {
                kill_group(&mut child);
                for reader in readers {
                    let _ = reader.await;
                }
                let partial = merged.lock().map(|s| s.clone()).unwrap_or_default();
                let mut text =
                    format!("command timed out after {seconds}s and was killed\n\n{partial}");
                text = budget::truncate(&text, budget::DEFAULT_TOKEN_BUDGET);
                return Ok(ToolOutput::error(text));
            }
        };

        for reader in readers {
            let _ = reader.await;
        }

        let output = merged.lock().map(|s| s.clone()).unwrap_or_default();
        let code = status.as_ref().ok().and_then(|s| s.code());
        let failed = status.as_ref().map(|s| !s.success()).unwrap_or(true);

        let mut text = output;
        if failed {
            let label = match code {
                Some(code) => format!("exit code {code}"),
                None => "killed by signal".to_string(),
            };
            text = if text.trim().is_empty() {
                format!("command failed: {label}")
            } else {
                format!("{text}\ncommand failed: {label}")
            };
        } else if text.trim().is_empty() {
            text = "command succeeded with no output".to_string();
        }

        let text = budget::truncate(&text, budget::DEFAULT_TOKEN_BUDGET);
        Ok(if failed {
            ToolOutput::error(text)
        } else {
            ToolOutput::ok(text)
        })
    }
}

type PinnedRead = std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>;

enum PipeKind {
    Out(tokio::process::ChildStdout),
    Err(tokio::process::ChildStderr),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx {
        Ctx::new(std::env::temp_dir())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn output_streams_through_the_progress_sink_as_it_arrives() {
        let ctx = ctx();
        let seen: Arc<Mutex<Vec<(std::time::Instant, String)>>> = Arc::default();
        let log = seen.clone();
        crate::progress::register(
            &ctx,
            Arc::new(move |_cmd, line| log.lock().unwrap().push((std::time::Instant::now(), line.to_string()))),
        );
        let started = std::time::Instant::now();
        Bash::new()
            .run(json!({ "command": "echo first; sleep 0.6; echo second" }), &ctx)
            .await
            .unwrap();
        let finished = std::time::Instant::now();
        crate::progress::unregister(&ctx);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>(), vec!["first", "second"]);
        assert!(seen[0].0.duration_since(started) < std::time::Duration::from_millis(400));
        assert!(finished.duration_since(seen[0].0) >= std::time::Duration::from_millis(400));
    }

    #[tokio::test]
    async fn it_returns_stdout() {
        let out = Bash::new()
            .run(json!({ "command": "echo hello" }), &ctx())
            .await
            .unwrap();
        assert!(out.content.contains("hello"));
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn stderr_is_merged_into_the_same_output() {
        let command = if cfg!(windows) {
            "echo oops 1>&2"
        } else {
            "echo oops >&2"
        };
        let out = Bash::new().run(json!({ "command": command }), &ctx()).await.unwrap();
        assert!(out.content.contains("oops"));
    }

    #[tokio::test]
    async fn a_failing_command_reports_its_exit_code() {
        let out = Bash::new().run(json!({ "command": "exit 3" }), &ctx()).await.unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("exit code 3"));
    }

    #[tokio::test]
    async fn a_hanging_command_is_killed_at_the_timeout() {
        let command = if cfg!(windows) {
            "ping -n 30 127.0.0.1 > nul"
        } else {
            "sleep 30"
        };
        let started = std::time::Instant::now();
        let out = Bash::new()
            .run(json!({ "command": command, "timeout": 1 }), &ctx())
            .await
            .unwrap();

        assert!(out.is_error);
        assert!(out.content.contains("timed out"));
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
    }

    #[tokio::test]
    async fn a_missing_command_argument_is_an_error() {
        assert!(Bash::new().run(json!({}), &ctx()).await.is_err());
    }
}
