use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zarn_core::agent::{GateDecision, ToolGate};
use zarn_core::tools::Risk;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    Allow,
    #[default]
    Ask,
    Deny,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Once,
    Always,
    No,
}

#[async_trait]
pub trait PermissionPrompt: Send + Sync {
    async fn ask(&self, tool: &str, risk: Risk, args: &Value) -> Answer;
}

pub struct DenyAll;

#[async_trait]
impl PermissionPrompt for DenyAll {
    async fn ask(&self, _tool: &str, _risk: Risk, _args: &Value) -> Answer {
        Answer::No
    }
}

pub struct Permissions {
    configured: HashMap<String, Policy>,
    by_risk: HashMap<Risk, Policy>,
    session: Mutex<HashMap<String, Policy>>,
    prompt: Arc<dyn PermissionPrompt>,
}

impl Permissions {
    pub fn new(prompt: Arc<dyn PermissionPrompt>) -> Self {
        let by_risk = HashMap::from([
            (Risk::Safe, Policy::Allow),
            (Risk::Write, Policy::Ask),
            (Risk::Execute, Policy::Ask),
        ]);
        Permissions {
            configured: HashMap::new(),
            by_risk,
            session: Mutex::new(HashMap::new()),
            prompt,
        }
    }

    pub fn headless() -> Self {
        Self::new(Arc::new(DenyAll))
    }

    pub fn with_tool(mut self, name: impl Into<String>, policy: Policy) -> Self {
        self.configured.insert(name.into(), policy);
        self
    }

    pub fn with_tools(mut self, policies: HashMap<String, Policy>) -> Self {
        self.configured.extend(policies);
        self
    }

    pub fn with_risk(mut self, risk: Risk, policy: Policy) -> Self {
        self.by_risk.insert(risk, policy);
        self
    }

    pub fn allow_for_session(&self, name: &str) {
        if let Ok(mut session) = self.session.lock() {
            session.insert(name.to_string(), Policy::Allow);
        }
    }

    pub fn policy_for(&self, name: &str, risk: Risk) -> Policy {
        if let Ok(session) = self.session.lock()
            && let Some(policy) = session.get(name)
        {
            return *policy;
        }
        if let Some(policy) = self.configured.get(name) {
            return *policy;
        }
        self.by_risk.get(&risk).copied().unwrap_or(Policy::Ask)
    }
}

#[async_trait]
impl ToolGate for Permissions {
    async fn check(&self, name: &str, risk: Risk, args: &Value) -> GateDecision {
        match self.policy_for(name, risk) {
            Policy::Allow => GateDecision::Allow,
            Policy::Deny => {
                GateDecision::Deny(format!("tool {name} is denied by the permission policy"))
            }
            Policy::Ask => match self.prompt.ask(name, risk, args).await {
                Answer::Once => GateDecision::Allow,
                Answer::Always => {
                    self.allow_for_session(name);
                    GateDecision::Allow
                }
                Answer::No => GateDecision::Deny(format!("user denied {name}")),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysYes;

    #[async_trait]
    impl PermissionPrompt for AlwaysYes {
        async fn ask(&self, _tool: &str, _risk: Risk, _args: &Value) -> Answer {
            Answer::Always
        }
    }

    #[test]
    fn risk_defaults_apply_when_nothing_is_configured() {
        let perms = Permissions::headless();
        assert_eq!(perms.policy_for("read", Risk::Safe), Policy::Allow);
        assert_eq!(perms.policy_for("bash", Risk::Execute), Policy::Ask);
    }

    #[test]
    fn configured_policy_beats_risk_default() {
        let perms = Permissions::headless().with_tool("bash", Policy::Deny);
        assert_eq!(perms.policy_for("bash", Risk::Execute), Policy::Deny);
    }

    #[tokio::test]
    async fn headless_denies_anything_that_would_prompt() {
        let perms = Permissions::headless();
        let decision = perms.check("write", Risk::Write, &Value::Null).await;
        assert!(matches!(decision, GateDecision::Deny(_)));
    }

    #[tokio::test]
    async fn always_is_remembered_for_the_session() {
        let perms = Permissions::new(Arc::new(AlwaysYes));
        assert_eq!(perms.policy_for("write", Risk::Write), Policy::Ask);
        assert_eq!(
            perms.check("write", Risk::Write, &Value::Null).await,
            GateDecision::Allow
        );
        assert_eq!(perms.policy_for("write", Risk::Write), Policy::Allow);
    }

    #[tokio::test]
    async fn session_override_does_not_resurrect_a_denied_tool() {
        let perms = Permissions::new(Arc::new(AlwaysYes)).with_tool("bash", Policy::Deny);
        let decision = perms.check("bash", Risk::Execute, &Value::Null).await;
        assert!(matches!(decision, GateDecision::Deny(_)));
    }
}
