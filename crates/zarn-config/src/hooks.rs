use crate::auth::redact;
use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zarn_core::agent::{GateDecision, ToolGate};
use zarn_core::tools::Risk;

pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
pub const EVENTS: [&str; 4] = ["pre_tool", "post_tool", "prompt_submit", "turn_end"];

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookSpec {
    #[serde(rename = "match", default, skip_serializing_if = "String::is_empty")]
    pub matcher: String,
    pub command: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hooks {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_tool: Vec<HookSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_tool: Vec<HookSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt_submit: Vec<HookSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turn_end: Vec<HookSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

impl Hooks {
    pub fn is_empty(&self) -> bool {
        *self == Hooks::default()
    }

    pub fn fill_from(&mut self, other: Hooks) {
        for (into, from) in [
            (&mut self.pre_tool, other.pre_tool),
            (&mut self.post_tool, other.post_tool),
            (&mut self.prompt_submit, other.prompt_submit),
            (&mut self.turn_end, other.turn_end),
        ] {
            for hook in from {
                if !into.contains(&hook) {
                    into.push(hook);
                }
            }
        }
        self.timeout_secs = self.timeout_secs.or(other.timeout_secs);
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS))
    }

    pub fn list(&self, event: &str) -> &[HookSpec] {
        match event {
            "pre_tool" => &self.pre_tool,
            "post_tool" => &self.post_tool,
            "prompt_submit" => &self.prompt_submit,
            "turn_end" => &self.turn_end,
            _ => &[],
        }
    }

    pub fn all(&self) -> Vec<(&'static str, &HookSpec)> {
        EVENTS
            .iter()
            .flat_map(|event| self.list(event).iter().map(move |h| (*event, h)))
            .collect()
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        for (event, hook) in self.all() {
            if hook.command.trim().is_empty() {
                anyhow::bail!("a {event} hook has an empty command");
            }
            matcher(&hook.matcher)
                .map_err(|e| anyhow::anyhow!("{event} hook match '{}' is not a valid pattern: {e}", hook.matcher))?;
        }
        Ok(())
    }
}

fn matcher(pattern: &str) -> Result<Option<Regex>, regex::Error> {
    if pattern.trim().is_empty() || pattern.trim() == "*" {
        return Ok(None);
    }
    Regex::new(&format!("^(?:{pattern})$")).map(Some)
}

pub fn matches(hook: &HookSpec, tool: &str) -> bool {
    match matcher(&hook.matcher) {
        Ok(None) => true,
        Ok(Some(re)) => re.is_match(tool),
        Err(_) => false,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub command: String,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl Outcome {
    pub fn ok(&self) -> bool {
        !self.timed_out && self.code == Some(0)
    }

    pub fn reason(&self) -> String {
        if self.timed_out {
            return format!("the hook `{}` timed out", self.command);
        }
        let detail = if self.stderr.trim().is_empty() {
            self.stdout.trim()
        } else {
            self.stderr.trim()
        };
        let code = self
            .code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "a signal".into());
        if detail.is_empty() {
            format!("the hook `{}` exited with {code}", self.command)
        } else {
            format!("the hook `{}` exited with {code}: {detail}", self.command)
        }
    }
}

fn shell(command: &str) -> tokio::process::Command {
    let mut cmd = if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

pub async fn run(command: &str, input: &Value, timeout: Duration, cwd: &Path) -> Outcome {
    let mut outcome = Outcome {
        command: command.to_string(),
        code: None,
        stdout: String::new(),
        stderr: String::new(),
        timed_out: false,
    };
    let mut child = match shell(command).current_dir(cwd).spawn() {
        Ok(child) => child,
        Err(err) => {
            outcome.stderr = format!("could not start: {err}");
            return outcome;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let body = input.to_string();
        let _ = stdin.write_all(body.as_bytes()).await;
        drop(stdin);
    }
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let collect = async {
        let read_out = async {
            let mut out = String::new();
            if let Some(s) = stdout.as_mut() {
                let _ = s.read_to_string(&mut out).await;
            }
            out
        };
        let read_err = async {
            let mut err = String::new();
            if let Some(s) = stderr.as_mut() {
                let _ = s.read_to_string(&mut err).await;
            }
            err
        };
        let (out, err) = tokio::join!(read_out, read_err);
        let status = child.wait().await;
        (out, err, status)
    };
    match tokio::time::timeout(timeout, collect).await {
        Ok((out, err, status)) => {
            outcome.stdout = redact(&out);
            outcome.stderr = redact(&err);
            outcome.code = status.ok().and_then(|s| s.code());
        }
        Err(_) => outcome.timed_out = true,
    }
    outcome
}

pub async fn fire(
    hooks: &Hooks,
    event: &str,
    tool: Option<&str>,
    mut payload: Value,
    cwd: &Path,
) -> Vec<Outcome> {
    if let Value::Object(map) = &mut payload {
        map.insert("event".into(), json!(event));
        map.insert("cwd".into(), json!(cwd.display().to_string()));
        if let Some(tool) = tool {
            map.insert("tool".into(), json!(tool));
        }
    }
    let mut outcomes = Vec::new();
    for hook in hooks.list(event) {
        if let Some(tool) = tool
            && !matches(hook, tool)
        {
            continue;
        }
        outcomes.push(run(&hook.command, &payload, hooks.timeout(), cwd).await);
    }
    outcomes
}

pub struct HookedGate {
    inner: Arc<dyn ToolGate>,
    hooks: Arc<RwLock<Hooks>>,
    cwd: PathBuf,
}

impl HookedGate {
    pub fn new(inner: Arc<dyn ToolGate>, hooks: Arc<RwLock<Hooks>>, cwd: impl Into<PathBuf>) -> Self {
        HookedGate {
            inner,
            hooks,
            cwd: cwd.into(),
        }
    }
}

#[async_trait]
impl ToolGate for HookedGate {
    async fn check(&self, name: &str, risk: Risk, args: &Value) -> GateDecision {
        let hooks = self.hooks.read().map(|h| h.clone()).unwrap_or_default();
        if !hooks.pre_tool.is_empty() {
            let outcomes = fire(
                &hooks,
                "pre_tool",
                Some(name),
                json!({ "args": args }),
                &self.cwd,
            )
            .await;
            if let Some(blocked) = outcomes.iter().find(|o| !o.ok()) {
                return GateDecision::Deny(format!("blocked by a pre_tool hook: {}", blocked.reason()));
            }
        }
        self.inner.check(name, risk, args).await
    }
}

pub fn describe(hooks: &Hooks) -> String {
    if hooks.all().is_empty() {
        return "no hooks; add them under [hooks] in config.toml".into();
    }
    let mut out: Vec<String> = hooks
        .all()
        .into_iter()
        .map(|(event, hook)| {
            let matcher = if hook.matcher.is_empty() { "*" } else { hook.matcher.as_str() };
            format!("{event:<13} {matcher:<16} {}", hook.command)
        })
        .collect();
    out.push(format!("timeout {} seconds", hooks.timeout().as_secs()));
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matchers_are_anchored_alternations() {
        let hook = HookSpec {
            matcher: "edit|write".into(),
            command: "true".into(),
        };
        assert!(matches(&hook, "edit"));
        assert!(matches(&hook, "write"));
        assert!(!matches(&hook, "multiedit"));
        assert!(matches(
            &HookSpec {
                matcher: String::new(),
                command: "x".into()
            },
            "anything"
        ));
    }

    #[test]
    fn hooks_parse_from_toml_and_merge() {
        let mut project: Hooks = toml::from_str(
            "timeout_secs = 5\n[[pre_tool]]\nmatch = \"bash\"\ncommand = \"./check.sh\"\n",
        )
        .unwrap();
        let user: Hooks = toml::from_str("[[turn_end]]\ncommand = \"notify\"\n").unwrap();
        project.fill_from(user);
        assert_eq!(project.pre_tool[0].matcher, "bash");
        assert_eq!(project.turn_end[0].command, "notify");
        assert_eq!(project.timeout(), Duration::from_secs(5));
        let bad: Hooks = toml::from_str("[[pre_tool]]\nmatch = \"(\"\ncommand = \"x\"\n").unwrap();
        assert!(bad.validate().is_err());
    }
}
