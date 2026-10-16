use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use zarn_core::tools::{Ctx, Risk, Tool, ToolOutput};
use zarn_core::types::ToolSpec;

pub const KEEP_BYTES: usize = 1024 * 1024;

#[derive(Default)]
struct Job {
    command: String,
    output: String,
    dropped: usize,
    read: usize,
    status: Option<String>,
    pid: Option<u32>,
}

#[derive(Default)]
pub struct Shells {
    jobs: Mutex<BTreeMap<String, Arc<Mutex<Job>>>>,
    next: Mutex<usize>,
}

impl Shells {
    pub fn new() -> Arc<Self> {
        Arc::new(Shells::default())
    }

    pub fn start(self: &Arc<Self>, command: &str, mut builder: tokio::process::Command) -> std::result::Result<String, String> {
        builder
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(false);
        let mut child = builder.spawn().map_err(|e| e.to_string())?;
        let id = {
            let mut n = self.next.lock().map_err(|_| "lock poisoned".to_string())?;
            *n += 1;
            format!("shell-{n}")
        };
        let job = Arc::new(Mutex::new(Job {
            command: command.to_string(),
            pid: child.id(),
            ..Job::default()
        }));
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.insert(id.clone(), job.clone());
        }
        let mut readers = Vec::new();
        let out = child.stdout.take().map(|o| Box::pin(o) as Pinned);
        let err = child.stderr.take().map(|e| Box::pin(e) as Pinned);
        for pipe in [out, err].into_iter().flatten() {
            let job = job.clone();
            readers.push(tokio::spawn(async move {
                let mut lines = BufReader::new(pipe).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(mut j) = job.lock() {
                        j.output.push_str(&line);
                        j.output.push('\n');
                        if j.output.len() > KEEP_BYTES {
                            let mut cut = j.output.len() - KEEP_BYTES;
                            while !j.output.is_char_boundary(cut) {
                                cut += 1;
                            }
                            j.output.drain(..cut);
                            j.dropped += cut;
                            j.read = j.read.saturating_sub(cut);
                        }
                    }
                }
            }));
        }
        tokio::spawn(async move {
            let status = child.wait().await;
            for r in readers {
                let _ = r.await;
            }
            if let Ok(mut j) = job.lock() {
                if j.status.is_none() {
                    j.status = Some(match status.ok().and_then(|s| s.code()) {
                        Some(0) => "exited with code 0".into(),
                        Some(code) => format!("exited with code {code}"),
                        None => "killed by a signal".into(),
                    });
                }
                j.pid = None;
            }
        });
        Ok(id)
    }

    fn job(&self, id: &str) -> Option<Arc<Mutex<Job>>> {
        self.jobs.lock().ok()?.get(id).cloned()
    }

    pub fn ids(&self) -> Vec<String> {
        self.jobs.lock().map(|j| j.keys().cloned().collect()).unwrap_or_default()
    }

    pub fn output(&self, id: &str, filter: Option<&regex::Regex>) -> Option<(String, Option<String>)> {
        let job = self.job(id)?;
        let mut j = job.lock().ok()?;
        let fresh = j.output[j.read.min(j.output.len())..].to_string();
        j.read = j.output.len();
        let text = match filter {
            Some(re) => fresh.lines().filter(|l| re.is_match(l)).map(|l| format!("{l}\n")).collect(),
            None => fresh,
        };
        Some((text, j.status.clone()))
    }

    pub fn kill(&self, id: &str) -> Option<String> {
        let job = self.job(id)?;
        let mut j = job.lock().ok()?;
        if let Some(status) = &j.status {
            return Some(format!("{id} already finished: {status}"));
        }
        if let Some(pid) = j.pid {
            kill_pid(pid);
        }
        j.status = Some("killed by bash_kill".into());
        Some(format!("killed {id} ({})", j.command))
    }

    pub fn kill_all(&self) {
        for id in self.ids() {
            self.kill(&id);
        }
    }
}

type Pinned = std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>;

#[cfg(unix)]
fn kill_pid(pid: u32) {
    unsafe {
        libc::killpg(pid as i32, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn kill_pid(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

pub struct BashOutput {
    shells: Arc<Shells>,
}

impl BashOutput {
    pub fn new(shells: Arc<Shells>) -> Self {
        BashOutput { shells }
    }
}

#[async_trait]
impl Tool for BashOutput {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash_output".to_string(),
            description: "Read the output a background bash command (started with background = true) produced since the last read, and whether it is still running.".to_string(),
            schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "The id bash returned, like shell-1" },
                    "filter": { "type": "string", "description": "Only return lines matching this regex" }
                },
                "required": ["id"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Safe
    }

    async fn run(&self, args: Value, _ctx: &Ctx) -> Result<ToolOutput> {
        let id = args.get("id").and_then(Value::as_str).ok_or_else(|| anyhow!("id is required"))?;
        let filter = match args.get("filter").and_then(Value::as_str) {
            Some(f) => Some(regex::Regex::new(f).map_err(|e| anyhow!("bad filter: {e}"))?),
            None => None,
        };
        let Some((text, status)) = self.shells.output(id, filter.as_ref()) else {
            return Ok(ToolOutput::error(format!("no background shell {id}; running: {}", self.shells.ids().join(", "))));
        };
        let state = status.unwrap_or_else(|| "still running".into());
        let text = crate::budget::truncate(&text, crate::budget::DEFAULT_TOKEN_BUDGET);
        Ok(ToolOutput::ok(if text.is_empty() {
            format!("[{id}: {state}; no new output]")
        } else {
            format!("{text}[{id}: {state}]")
        }))
    }
}

pub struct BashKill {
    shells: Arc<Shells>,
}

impl BashKill {
    pub fn new(shells: Arc<Shells>) -> Self {
        BashKill { shells }
    }
}

#[async_trait]
impl Tool for BashKill {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash_kill".to_string(),
            description: "Stop a background bash command started with background = true.".to_string(),
            schema: json!({
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Safe
    }

    async fn run(&self, args: Value, _ctx: &Ctx) -> Result<ToolOutput> {
        let id = args.get("id").and_then(Value::as_str).ok_or_else(|| anyhow!("id is required"))?;
        Ok(match self.shells.kill(id) {
            Some(text) => ToolOutput::ok(text),
            None => ToolOutput::error(format!("no background shell {id}")),
        })
    }
}
