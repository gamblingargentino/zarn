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

pub const SHELL_ENV: &str = "ZARN_SHELL";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shell {
    pub program: String,
    pub args: Vec<&'static str>,
    pub label: String,
}

impl Shell {
    fn of(program: impl Into<String>) -> Shell {
        let program = program.into();
        let name = std::path::Path::new(&program)
            .file_stem()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let (args, label) = match name.as_str() {
            "pwsh" => (vec!["-NoProfile", "-NonInteractive", "-Command"], "PowerShell 7".to_string()),
            "powershell" => (vec!["-NoProfile", "-NonInteractive", "-Command"], "Windows PowerShell".to_string()),
            "cmd" => (vec!["/C"], "cmd.exe".to_string()),
            "sh" | "zsh" | "dash" => (vec!["-c"], name.clone()),
            _ if cfg!(windows) => (vec!["-c"], "bash (Git for Windows)".to_string()),
            _ => (vec!["-c"], "bash".to_string()),
        };
        Shell { program, args, label }
    }
}

pub fn pick_windows_shell(
    chosen: Option<String>,
    path: &str,
    program_files: &[String],
    exists: &dyn Fn(&std::path::Path) -> bool,
) -> Shell {
    if let Some(chosen) = chosen.filter(|c| !c.trim().is_empty()) {
        return Shell::of(chosen.trim());
    }
    let installed = program_files
        .iter()
        .map(|root| std::path::Path::new(root).join("Git").join("bin").join("bash.exe"));
    let on_path = path
        .split(';')
        .filter(|dir| {
            let lower = dir.to_lowercase();
            !lower.contains("system32") && !lower.contains("windowsapps")
        })
        .map(|dir| std::path::Path::new(dir).join("bash.exe"));
    if let Some(bash) = installed.chain(on_path).find(|p| exists(p)) {
        return Shell::of(bash.to_string_lossy().to_string());
    }
    for name in ["pwsh.exe", "powershell.exe"] {
        if path.split(';').any(|dir| exists(&std::path::Path::new(dir).join(name))) {
            return Shell::of(name);
        }
    }
    Shell::of("cmd")
}

pub fn shell() -> &'static Shell {
    static SHELL: std::sync::OnceLock<Shell> = std::sync::OnceLock::new();
    SHELL.get_or_init(|| {
        let chosen = std::env::var(SHELL_ENV).ok();
        if !cfg!(windows) {
            return Shell::of(chosen.filter(|c| !c.trim().is_empty()).unwrap_or_else(|| "bash".into()));
        }
        let program_files: Vec<String> = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
            .iter()
            .filter_map(|v| std::env::var(v).ok())
            .chain(std::env::var("LOCALAPPDATA").ok().map(|d| format!("{d}\\Programs")))
            .collect();
        pick_windows_shell(chosen, &std::env::var("PATH").unwrap_or_default(), &program_files, &|p| p.is_file())
    })
}

pub fn environment_note() -> String {
    let os = if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    };
    let shell = shell();
    let hint = match shell.label.as_str() {
        "cmd.exe" => " Write cmd.exe commands (dir, type, findstr, where), not Unix ones; ls, cat and grep do not exist here.",
        "Windows PowerShell" | "PowerShell 7" => " Write PowerShell commands (Get-ChildItem, Get-Content, Select-String); Unix flags such as ls -la do not work.",
        _ if cfg!(windows) => " It is a Unix-style shell on Windows: paths may be written C:/like/this or /c/like/this.",
        _ => "",
    };
    format!("The operating system is {os}. The bash tool runs commands with {}.{hint}", shell.label)
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
            description: format!(
                "Run a shell command in the working directory and return its merged stdout and stderr. Commands run with {}; write them for that shell.",
                shell().label
            ),
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

        let shell = shell();
        let mut builder = Command::new(&shell.program);
        builder
            .args(&shell.args)
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
                Err(err) => ToolOutput::error(format!("cannot run {}: {err}", shell.program)),
            });
        }

        let mut child = match builder.spawn() {
            Ok(child) => child,
            Err(err) => return Ok(ToolOutput::error(format!("cannot run {}: {err}", shell.program))),
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

    #[test]
    fn windows_prefers_git_bash_then_powershell_then_cmd() {
        let files = |present: &'static [&'static str]| {
            move |p: &std::path::Path| present.iter().any(|f| p.to_string_lossy().replace('\\', "/") == *f)
        };
        let roots = vec!["C:/Program Files".to_string()];
        let git = pick_windows_shell(None, "C:/Windows/System32", &roots, &files(&["C:/Program Files/Git/bin/bash.exe"]));
        assert_eq!(git.label, if cfg!(windows) { "bash (Git for Windows)" } else { "bash" });
        assert_eq!(git.args, vec!["-c"]);
        let wsl = pick_windows_shell(
            None,
            "C:/Windows/System32;C:/Windows/System32/WindowsPowerShell/v1.0",
            &roots,
            &files(&["C:/Windows/System32/bash.exe", "C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe"]),
        );
        assert_eq!(wsl.label, "Windows PowerShell", "the WSL bash in System32 runs in another file system");
        assert_eq!(wsl.args, vec!["-NoProfile", "-NonInteractive", "-Command"]);
        let bare = pick_windows_shell(None, "", &roots, &files(&[]));
        assert_eq!(bare.label, "cmd.exe");
        let chosen = pick_windows_shell(Some("pwsh".into()), "", &roots, &files(&[]));
        assert_eq!(chosen.label, "PowerShell 7");
        assert!(environment_note().contains("The bash tool runs commands with"));
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
