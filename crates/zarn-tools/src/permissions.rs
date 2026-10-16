use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overlay {
    pub name: String,
    pub policies: HashMap<String, Policy>,
    pub allow_all: bool,
    pub safe_only: bool,
}

impl Overlay {
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty() && !self.allow_all && !self.safe_only
    }
}

pub const PLAN_DENIAL: &str = "plan mode allows only read-only tools until the plan is approved; call exit_plan with your plan";

pub struct Permissions {
    overlay: RwLock<Overlay>,
    configured: RwLock<HashMap<String, Policy>>,
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
            overlay: RwLock::new(Overlay::default()),
            configured: RwLock::new(HashMap::new()),
            by_risk,
            session: Mutex::new(HashMap::new()),
            prompt,
        }
    }

    pub fn headless() -> Self {
        Self::new(Arc::new(DenyAll))
    }

    pub fn with_tool(self, name: impl Into<String>, policy: Policy) -> Self {
        if let Ok(mut configured) = self.configured.write() {
            configured.insert(name.into(), policy);
        }
        self
    }

    pub fn with_tools(self, policies: impl IntoIterator<Item = (String, Policy)>) -> Self {
        if let Ok(mut configured) = self.configured.write() {
            configured.extend(policies);
        }
        self
    }

    pub fn set_policies(&self, policies: impl IntoIterator<Item = (String, Policy)>) {
        if let Ok(mut configured) = self.configured.write() {
            *configured = policies.into_iter().collect();
        }
    }

    pub fn set_overlay(&self, overlay: Overlay) {
        if let Ok(mut current) = self.overlay.write() {
            *current = overlay;
        }
    }

    pub fn overlay(&self) -> Overlay {
        self.overlay.read().map(|o| o.clone()).unwrap_or_default()
    }

    pub fn with_overlay(self, overlay: Overlay) -> Self {
        self.set_overlay(overlay);
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

    fn plan_blocks(&self, name: &str, risk: Risk) -> bool {
        self.overlay.read().is_ok_and(|o| o.safe_only) && risk != Risk::Safe && name != "exit_plan"
    }

    pub fn policy_for(&self, name: &str, risk: Risk) -> Policy {
        let configured = self
            .configured
            .read()
            .ok()
            .and_then(|c| c.get(name).copied());
        if configured == Some(Policy::Deny) || self.plan_blocks(name, risk) {
            return Policy::Deny;
        }
        let overlay = self.overlay();
        match overlay.policies.get(name) {
            Some(Policy::Deny) => return Policy::Deny,
            Some(policy) => return *policy,
            None if overlay.allow_all => return Policy::Allow,
            None => {}
        }
        if let Ok(session) = self.session.lock()
            && let Some(policy) = session.get(name)
        {
            return *policy;
        }
        if let Some(policy) = configured {
            return policy;
        }
        self.by_risk.get(&risk).copied().unwrap_or(Policy::Ask)
    }
}

#[async_trait]
impl ToolGate for Permissions {
    async fn check(&self, name: &str, risk: Risk, args: &Value) -> GateDecision {
        match self.policy_for(name, risk) {
            Policy::Allow => GateDecision::Allow,
            Policy::Deny if self.plan_blocks(name, risk) => GateDecision::Deny(PLAN_DENIAL.into()),
            Policy::Deny => {
                let overlay = self.overlay();
                if overlay.policies.get(name) == Some(&Policy::Deny) {
                    GateDecision::Deny(format!("tool {name} is denied in {} mode", overlay.name))
                } else {
                    GateDecision::Deny(format!("tool {name} is denied by the permission policy"))
                }
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
    fn policies_can_be_replaced_while_running() {
        let perms = Permissions::headless().with_tool("bash", Policy::Deny);
        perms.set_policies([("bash".to_string(), Policy::Allow)]);
        assert_eq!(perms.policy_for("bash", Risk::Execute), Policy::Allow);
    }

    #[tokio::test]
    async fn a_later_deny_beats_an_earlier_always() {
        let perms = Permissions::new(Arc::new(AlwaysYes));
        perms.check("bash", Risk::Execute, &Value::Null).await;
        perms.set_policies([("bash".to_string(), Policy::Deny)]);
        assert_eq!(perms.policy_for("bash", Risk::Execute), Policy::Deny);
    }

    fn overlay(name: &str, policies: &[(&str, Policy)]) -> Overlay {
        Overlay {
            name: name.into(),
            policies: policies.iter().map(|(t, p)| (t.to_string(), *p)).collect(),
            ..Overlay::default()
        }
    }

    #[test]
    fn an_overlay_sits_on_top_of_config_and_comes_off_cleanly() {
        let perms = Permissions::headless().with_tool("edit", Policy::Allow);
        perms.set_overlay(overlay("review", &[("edit", Policy::Deny), ("bash", Policy::Ask)]));
        assert_eq!(perms.policy_for("edit", Risk::Write), Policy::Deny);
        perms.set_overlay(Overlay::default());
        assert_eq!(perms.policy_for("edit", Risk::Write), Policy::Allow);
    }

    #[test]
    fn an_overlay_cannot_lift_a_configured_deny() {
        let perms = Permissions::headless().with_tool("bash", Policy::Deny);
        perms.set_overlay(Overlay {
            allow_all: true,
            ..overlay("bypass", &[("bash", Policy::Allow)])
        });
        assert_eq!(perms.policy_for("bash", Risk::Execute), Policy::Deny);
    }

    #[tokio::test]
    async fn plan_mode_denies_anything_that_is_not_read_only() {
        let perms = Permissions::new(Arc::new(AlwaysYes));
        perms.set_overlay(Overlay {
            safe_only: true,
            ..overlay("plan", &[])
        });
        let denied = perms.check("write", Risk::Write, &Value::Null).await;
        assert_eq!(denied, GateDecision::Deny(PLAN_DENIAL.into()));
        assert_eq!(perms.check("read", Risk::Safe, &Value::Null).await, GateDecision::Allow);
        assert_eq!(perms.check("exit_plan", Risk::Safe, &Value::Null).await, GateDecision::Allow);
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
