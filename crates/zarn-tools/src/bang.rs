use serde_json::json;
use zarn_core::agent::{GateDecision, ToolGate};
use zarn_core::tools::{Ctx, Risk, Tool};

pub const OUTPUT_CHARS: usize = 20_000;

pub async fn expand(text: &str, ctx: &Ctx, gate: &dyn ToolGate) -> Result<String, String> {
    let found = zarn_core::commands::bang_commands(text);
    if found.is_empty() {
        return Ok(text.to_string());
    }
    let bash = crate::bash::Bash::new();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (range, command) in found {
        out.push_str(&text[last..range.start]);
        let args = json!({ "command": command });
        if let GateDecision::Deny(reason) = gate.check("bash", Risk::Execute, &args).await {
            return Err(format!("!`{command}` was not run: {reason}"));
        }
        let result = bash.run(args, ctx).await.map_err(|e| format!("!`{command}` failed: {e:#}"))?;
        let mut output = if !result.is_error && result.content == "command succeeded with no output" {
            String::new()
        } else {
            result.content.trim_end().to_string()
        };
        if output.chars().count() > OUTPUT_CHARS {
            output = output.chars().take(OUTPUT_CHARS).collect::<String>() + "\n[output cut]";
        }
        out.push_str(&output);
        last = range.end;
    }
    out.push_str(&text[last..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{Permissions, Policy};
    use std::sync::Arc;

    struct Count(std::sync::atomic::AtomicUsize, GateDecision);

    #[async_trait::async_trait]
    impl ToolGate for Count {
        async fn check(&self, name: &str, risk: Risk, _args: &serde_json::Value) -> GateDecision {
            assert_eq!((name, risk), ("bash", Risk::Execute));
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.1.clone()
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bang_commands_run_through_the_gate_and_insert_their_output() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Ctx::new(dir.path());
        let gate = Count(Default::default(), GateDecision::Allow);
        let out = expand("Files: !`echo one; echo two` end", &ctx, &gate).await.unwrap();
        assert_eq!(out, "Files: one\ntwo end");
        assert_eq!(gate.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_denied_bang_command_stops_the_expansion_and_never_runs() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Ctx::new(dir.path());
        let gate = Count(Default::default(), GateDecision::Deny("bash = deny".into()));
        let err = expand("x !`touch made` y", &ctx, &gate).await.unwrap_err();
        assert!(err.contains("was not run: bash = deny"));
        assert!(!dir.path().join("made").exists());
        let perms = Arc::new(Permissions::headless().with_tool("bash", Policy::Deny));
        assert!(expand("!`touch made`", &ctx, perms.as_ref()).await.is_err());
        assert!(!dir.path().join("made").exists());
        assert_eq!(expand("no bangs `here`", &ctx, perms.as_ref()).await.unwrap(), "no bangs `here`");
    }
}
