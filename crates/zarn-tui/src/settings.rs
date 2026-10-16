use crate::brand;
use crate::widgets::{Choice, Edit, MaskedInput, Pick, Picker, TextField};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use zarn_config::agents::{self, AgentFile};
use zarn_config::modes::{self, ModeDef, ModeFile};
use zarn_config::auth::KeyStore;
use zarn_config::catalog;
use zarn_config::layers::{GeneralField, Layers, Scope};
use zarn_config::ops::{self, ProviderDraft};
use zarn_core::subagent::AgentDef;
use zarn_tools::permissions::Policy;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Providers,
    Models,
    Skills,
    Mcp,
    Agents,
    Modes,
    Quota,
    Permissions,
    Hooks,
    General,
}

pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Clone, Debug, PartialEq)]
pub enum QuotaState {
    Loading,
    Loaded(Box<zarn_config::quota::Report>),
    Failed(String),
}

pub const TABS: [Tab; 10] = [
    Tab::Providers,
    Tab::Models,
    Tab::Skills,
    Tab::Mcp,
    Tab::Agents,
    Tab::Modes,
    Tab::Quota,
    Tab::Permissions,
    Tab::Hooks,
    Tab::General,
];

impl Tab {
    pub fn label(&self) -> &'static str {
        match self {
            Tab::Providers => "Providers",
            Tab::Models => "Models",
            Tab::Skills => "Skills",
            Tab::Mcp => "MCP",
            Tab::Agents => "Agents",
            Tab::Modes => "Modes",
            Tab::Quota => "Quota",
            Tab::Permissions => "Permissions",
            Tab::Hooks => "Hooks",
            Tab::General => "General",
        }
    }

    pub fn index(&self) -> usize {
        TABS.iter().position(|t| t == self).unwrap_or(0)
    }

    pub fn hints(&self) -> &'static str {
        match self {
            Tab::Providers => "enter edit · a add · k key · t test · d delete",
            Tab::Models => "enter change · [ ] reorder · d remove fallback or clear role",
            Tab::Skills => "enter or space toggles",
            Tab::Mcp => "enter edit · a add · r restart · d delete",
            Tab::Agents => "enter edit · a create · d delete",
            Tab::Modes => "enter edit or switch · a create · d delete",
            Tab::Quota => "r refresh this provider · R refresh all",
            Tab::Permissions => "enter cycles · a allow · s ask · d deny, or delete a rule",
            Tab::Hooks => "hooks are edited under [hooks] in config.toml",
            Tab::General => "enter change",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillRow {
    pub name: String,
    pub description: String,
    pub source: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PanelData {
    pub skills: Vec<SkillRow>,
    pub mcp_status: BTreeMap<String, String>,
    pub tools: Vec<(String, Policy)>,
    pub quota: BTreeMap<String, QuotaState>,
    pub tick: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestState {
    Running,
    Ok(String),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Reload,
    SetMode(String),
    RefreshQuota(Option<String>),
    Test(String),
    RestartMcp(String),
    ListModels { provider: String, profile: String },
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKind {
    Provider(String),
    AddProvider,
    DefaultProfile,
    ProfileProvider(String),
    ProfileModel(String),
    Fallback(String, usize),
    AddFallback(String),
    AddProfile,
    Skill(String),
    Mcp(String),
    AddMcp,
    Agent(usize),
    AddAgent,
    Routing,
    Role(String),
    ActiveMode,
    ModeEntry(usize),
    AddMode,
    QuotaProvider(String),
    Info,
    Permission(String),
    Rule(Policy, String),
    Accent,
    Banner,
    ShowCost,
    ContextBudget,
    AutoCompact,
    SessionCap,
    DayCap,
    ReadOnly,
    Sandbox,
    Vim,
    Theme,
    Notifications,
    StatusLine,
    Mouse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    pub value: String,
    pub note: String,
    pub kind: RowKind,
}

impl Row {
    fn new(label: impl Into<String>, value: impl Into<String>, kind: RowKind) -> Self {
        Row {
            label: label.into(),
            value: value.into(),
            note: String::new(),
            kind,
        }
    }

    fn note(mut self, note: impl Into<String>) -> Self {
        self.note = note.into();
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickFor {
    NewProviderKind,
    DefaultProfile,
    ProfileProvider(String),
    ProfileModel(String),
    AddFallback(String),
    Accent,
    Theme,
    RoleProvider(String),
    RoleModel(String, String),
    ActiveMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormFor {
    NewProvider(String),
    EditProvider(String),
    NewMcp,
    EditMcp(String),
    NewAgent,
    EditAgent(usize),
    ContextBudget,
    NewProfile,
    NewMode,
    EditMode(usize),
    SpendCap(bool),
    StatusLine,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfirmFor {
    DeleteProvider(String),
    DeleteMcp(String),
    DeleteAgent(usize),
    DeleteMode(usize),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FormField {
    pub label: String,
    pub input: TextField,
    pub hint: String,
    pub multiline: bool,
}

impl FormField {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        FormField {
            label: label.into(),
            input: TextField::new(value),
            ..FormField::default()
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    pub fn multiline(mut self) -> Self {
        self.multiline = true;
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Form {
    pub title: String,
    pub fields: Vec<FormField>,
    pub focus: usize,
    pub preview: Option<crate::status_line::Values>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormEdit {
    Pending,
    Submit,
    Cancel,
}

impl Form {
    pub fn new(title: impl Into<String>, fields: Vec<FormField>) -> Self {
        Form {
            title: title.into(),
            fields,
            focus: 0,
            preview: None,
        }
    }

    pub fn value(&self, label: &str) -> String {
        self.fields
            .iter()
            .find(|f| f.label == label)
            .map(|f| f.input.value.trim().to_string())
            .unwrap_or_default()
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(field) = self.fields.get_mut(self.focus) {
            field.input.paste(text);
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> FormEdit {
        let last = self.fields.len().saturating_sub(1);
        let multiline = self.fields.get(self.focus).is_some_and(|f| f.multiline);
        match key.code {
            KeyCode::Esc => return FormEdit::Cancel,
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return FormEdit::Submit;
            }
            KeyCode::Up | KeyCode::BackTab => self.focus = self.focus.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => self.focus = (self.focus + 1).min(last),
            KeyCode::Enter
                if multiline
                    && (key.modifiers.contains(KeyModifiers::SHIFT)
                        || key.modifiers.contains(KeyModifiers::ALT)) =>
            {
                if let Some(field) = self.fields.get_mut(self.focus) {
                    field.input.paste_raw("\n");
                }
            }
            KeyCode::Enter => {
                if self.focus >= last {
                    return FormEdit::Submit;
                }
                self.focus += 1;
            }
            _ => {
                if let Some(field) = self.fields.get_mut(self.focus) {
                    field.input.on_key(key);
                }
            }
        }
        FormEdit::Pending
    }
}

#[derive(Debug)]
pub enum Editor {
    Pick(Picker, PickFor),
    Form(Form, FormFor),
    Secret {
        input: MaskedInput,
        provider: String,
        optional: bool,
    },
    Confirm(String, ConfirmFor),
}

pub struct Panel {
    pub tab: Tab,
    pub layers: Layers,
    pub data: PanelData,
    pub agents: Vec<AgentFile>,
    pub modes: Vec<ModeFile>,
    pub active_mode: String,
    pub status_values: crate::status_line::Values,
    pub cursor: Vec<usize>,
    pub editor: Option<Editor>,
    pub message: Option<(String, bool)>,
    pub tests: BTreeMap<String, TestState>,
    store: Arc<KeyStore>,
    agent_dirs: (Option<PathBuf>, PathBuf),
    mode_dirs: (Option<PathBuf>, PathBuf),
}

fn policy_label(policy: Policy) -> &'static str {
    match policy {
        Policy::Allow => "allow",
        Policy::Ask => "ask",
        Policy::Deny => "deny",
    }
}

fn next_policy(policy: Policy) -> Policy {
    match policy {
        Policy::Allow => Policy::Ask,
        Policy::Ask => Policy::Deny,
        Policy::Deny => Policy::Allow,
    }
}

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

pub fn split_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                    has = false;
                }
            }
            (None, c) => current.push(c),
        }
    }
    if has || !current.is_empty() {
        words.push(current);
    }
    words
}

fn join_words(words: &[String]) -> String {
    words
        .iter()
        .map(|w| {
            if w.contains(char::is_whitespace) || w.is_empty() {
                format!("\"{w}\"")
            } else {
                w.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl Panel {
    pub fn new(layers: Layers, store: Arc<KeyStore>, data: PanelData) -> Self {
        let project = agents::dir(Scope::Project).unwrap_or_default();
        Self::with_agent_dirs(layers, store, data, agents::dir(Scope::User), project)
    }

    pub fn with_agent_dirs(
        layers: Layers,
        store: Arc<KeyStore>,
        data: PanelData,
        user_agents: Option<PathBuf>,
        project_agents: PathBuf,
    ) -> Self {
        let agents = agents::load_from(user_agents.as_deref(), &project_agents);
        let mode_dirs = (
            modes::dir(Scope::User),
            modes::dir(Scope::Project).unwrap_or_default(),
        );
        let loaded = modes::load_from(mode_dirs.0.as_deref(), &mode_dirs.1);
        let active_mode = layers.merged().general.mode();
        Panel {
            tab: Tab::Providers,
            layers,
            data,
            agents,
            modes: loaded.modes,
            active_mode,
            status_values: crate::status_line::Values::default(),
            cursor: vec![0; TABS.len()],
            editor: None,
            message: None,
            tests: BTreeMap::new(),
            store,
            agent_dirs: (user_agents, project_agents),
            mode_dirs,
        }
    }

    pub fn with_mode_dirs(mut self, user: Option<PathBuf>, project: PathBuf) -> Self {
        self.mode_dirs = (user, project);
        self.reload_modes();
        self
    }

    pub fn with_status_values(mut self, values: crate::status_line::Values) -> Self {
        self.status_values = values;
        self
    }

    pub fn with_active_mode(mut self, mode: impl Into<String>) -> Self {
        self.active_mode = mode.into();
        self
    }

    fn reload_modes(&mut self) {
        self.modes = modes::load_from(self.mode_dirs.0.as_deref(), &self.mode_dirs.1).modes;
    }

    pub fn open_at(mut self, tab: Tab) -> Self {
        self.tab = tab;
        self
    }

    pub fn store(&self) -> &Arc<KeyStore> {
        &self.store
    }

    fn say(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), false));
    }

    fn fail(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), true));
    }

    fn reload_agents(&mut self) {
        self.agents = agents::load_from(self.agent_dirs.0.as_deref(), &self.agent_dirs.1);
    }

    pub fn set_test(&mut self, provider: &str, state: TestState) {
        self.tests.insert(provider.to_string(), state);
    }

    pub fn set_models(&mut self, key: &str, models: Vec<(String, Option<u32>)>) {
        let matches = match &self.editor {
            Some(Editor::Pick(_, PickFor::ProfileModel(p))) => p == key,
            Some(Editor::Pick(_, PickFor::RoleModel(role, _))) => format!("role:{role}") == key,
            _ => false,
        };
        if !matches {
            return;
        }
        if let Some(Editor::Pick(picker, _)) = &mut self.editor {
            picker.set_choices(merge_models(picker.choices.clone(), models));
        }
    }

    fn quota_owner(&self, index: usize) -> Option<String> {
        self.rows()
            .iter()
            .take(index + 1)
            .rev()
            .find_map(|r| match &r.kind {
                RowKind::QuotaProvider(name) => Some(name.clone()),
                _ => None,
            })
    }

    pub fn set_quota(&mut self, name: &str, state: QuotaState) {
        self.data.quota.insert(name.to_string(), state);
    }

    pub fn loading_quota(&self) -> bool {
        self.data.quota.values().any(|q| *q == QuotaState::Loading)
    }

    pub fn set_mcp_status(&mut self, name: &str, status: impl Into<String>) {
        self.data.mcp_status.insert(name.to_string(), status.into());
    }

    pub fn rows(&self) -> Vec<Row> {
        self.rows_for(self.tab)
    }

    pub fn rows_for(&self, tab: Tab) -> Vec<Row> {
        let cfg = self.layers.merged();
        match tab {
            Tab::Providers => {
                let mut rows: Vec<Row> = ops::providers(&self.layers, &self.store)
                    .into_iter()
                    .map(|p| {
                        let test = match self.tests.get(&p.name) {
                            None => zarn_config::health::summary(&p.name),
                            Some(TestState::Running) => "testing…".into(),
                            Some(TestState::Ok(msg)) => msg.clone(),
                            Some(TestState::Failed(msg)) => format!("FAILED: {msg}"),
                        };
                        let mark = if p.is_default { " (default)" } else { "" };
                        Row::new(
                            format!("{}{mark}", p.name),
                            format!("{} · {} · {}", p.kind, p.key_label(), p.scope.label()),
                            RowKind::Provider(p.name.clone()),
                        )
                        .note(test)
                    })
                    .collect();
                rows.push(Row::new("+ add provider", "", RowKind::AddProvider));
                rows
            }
            Tab::Models => {
                let default = cfg.default_profile();
                let mut rows = vec![Row::new("default profile", default.clone(), RowKind::DefaultProfile)];
                for (name, profile) in &cfg.profiles {
                    rows.push(Row::new(
                        format!("{name} · provider"),
                        profile.provider.clone().unwrap_or_else(|| "none".into()),
                        RowKind::ProfileProvider(name.clone()),
                    ));
                    rows.push(Row::new(
                        format!("{name} · model"),
                        profile.model.clone().unwrap_or_else(|| "none".into()),
                        RowKind::ProfileModel(name.clone()),
                    ));
                    if *name == default {
                        for (i, fallback) in profile.fallback.iter().flatten().enumerate() {
                            rows.push(Row::new(
                                format!("{name} · fallback {}", i + 1),
                                fallback.clone(),
                                RowKind::Fallback(name.clone(), i),
                            ));
                        }
                        rows.push(Row::new(
                            format!("{name} · + add fallback"),
                            "",
                            RowKind::AddFallback(name.clone()),
                        ));
                    }
                }
                rows.push(Row::new("+ new profile", "", RowKind::AddProfile));
                rows.push(
                    Row::new("routing", on_off(cfg.routing.enabled()), RowKind::Routing)
                        .note("send each role below to its own provider and model"),
                );
                for role in zarn_config::ROLES {
                    rows.push(Row::new(
                        format!("role · {role}"),
                        ops::role_label(&cfg, role),
                        RowKind::Role(role.to_string()),
                    ));
                }
                rows
            }
            Tab::Quota => {
                let mut rows = Vec::new();
                for (name, state) in &self.data.quota {
                    let kind = cfg.providers.get(name).map(|p| p.kind.clone()).unwrap_or_default();
                    let header = Row::new(name.clone(), kind, RowKind::QuotaProvider(name.clone()));
                    match state {
                        QuotaState::Loading => rows.push(header.note(format!(
                            "loading {}",
                            SPINNER[self.data.tick % SPINNER.len()]
                        ))),
                        QuotaState::Failed(err) => rows.push(header.note(format!("FAILED: {err}"))),
                        QuotaState::Loaded(report) => {
                            match report.failed() {
                                Some(err) => rows.push(header.note(format!("FAILED: {err}"))),
                                None => {
                                    rows.push(header.note(format!(
                                        "fetched {}",
                                        zarn_config::quota::age(report.fetched_at)
                                    )));
                                    rows.push(Row::new(
                                        "  models",
                                        zarn_config::quota::models_line(report, 8),
                                        RowKind::Info,
                                    ));
                                    rows.push(Row::new(
                                        "  limits",
                                        zarn_config::quota::limits_line(&report.limits),
                                        RowKind::Info,
                                    ));
                                    rows.push(Row::new(
                                        "  credits",
                                        zarn_config::quota::credits_line(&report.credits),
                                        RowKind::Info,
                                    ));
                                }
                            }
                        }
                    }
                }
                rows
            }
            Tab::Hooks => {
                let mut rows: Vec<Row> = cfg
                    .hooks
                    .all()
                    .into_iter()
                    .map(|(event, hook)| {
                        let matcher = if hook.matcher.is_empty() { "*" } else { hook.matcher.as_str() };
                        Row::new(format!("{event} · {matcher}"), hook.command.clone(), RowKind::Info)
                    })
                    .collect();
                if !rows.is_empty() {
                    rows.push(Row::new(
                        "timeout",
                        format!("{} seconds", cfg.hooks.timeout().as_secs()),
                        RowKind::Info,
                    ));
                }
                rows
            }
            Tab::Modes => {
                let mut rows = vec![Row::new("active mode", self.active_mode.clone(), RowKind::ActiveMode)];
                for (i, m) in self.modes.iter().enumerate() {
                    rows.push(
                        Row::new(m.def.name.clone(), m.source.label(), RowKind::ModeEntry(i))
                            .note(m.def.description.clone()),
                    );
                }
                rows.push(Row::new("+ create mode", "", RowKind::AddMode));
                rows
            }
            Tab::Skills => self
                .data
                .skills
                .iter()
                .map(|s| {
                    Row::new(
                        s.name.clone(),
                        format!("[{}] {}", on_off(cfg.skill_enabled(&s.name)), s.source),
                        RowKind::Skill(s.name.clone()),
                    )
                    .note(s.description.clone())
                })
                .collect(),
            Tab::Mcp => {
                let mut rows: Vec<Row> = cfg
                    .mcp
                    .iter()
                    .map(|(name, server)| {
                        let target = match (&server.command, &server.url) {
                            (Some(cmd), _) => {
                                let mut words = vec![cmd.clone()];
                                words.extend(server.args.iter().cloned());
                                format!("stdio · {}", join_words(&words))
                            }
                            (None, Some(url)) => format!("http · {url}"),
                            (None, None) => "invalid".into(),
                        };
                        let status = self
                            .data
                            .mcp_status
                            .get(name)
                            .cloned()
                            .unwrap_or_else(|| "not started".into());
                        Row::new(name.clone(), target, RowKind::Mcp(name.clone())).note(status)
                    })
                    .collect();
                rows.push(Row::new("+ add server", "", RowKind::AddMcp));
                rows
            }
            Tab::Agents => {
                let mut rows: Vec<Row> = self
                    .agents
                    .iter()
                    .enumerate()
                    .map(|(i, a)| {
                        Row::new(a.def.name.clone(), a.scope.label(), RowKind::Agent(i))
                            .note(a.def.description.clone())
                    })
                    .collect();
                rows.push(Row::new("+ create agent", "", RowKind::AddAgent));
                rows
            }
            Tab::Permissions => {
                let mut tools: Vec<(String, Policy)> = self.data.tools.clone();
                for name in cfg.permissions.keys() {
                    if !tools.iter().any(|(t, _)| t == name) {
                        tools.push((name.clone(), Policy::Ask));
                    }
                }
                tools.sort_by(|a, b| a.0.cmp(&b.0));
                tools
                    .into_iter()
                    .map(|(name, default)| {
                        let (value, note) = match cfg.permissions.get(&name) {
                            Some(policy) => (
                                policy_label(*policy).to_string(),
                                self.layers.permission_scope(&name).label().to_string(),
                            ),
                            None => (policy_label(default).to_string(), "default".to_string()),
                        };
                        Row::new(name.clone(), value, RowKind::Permission(name)).note(note)
                    })
                    .chain(cfg.permissions.rules.list().into_iter().map(|(policy, rule)| {
                        let scope = ops::rule_scope(&self.layers, &rule).label();
                        Row::new(
                            format!("rule · {}", policy_label(policy)),
                            rule.clone(),
                            RowKind::Rule(policy, rule),
                        )
                        .note(scope)
                    }))
                    .collect()
            }
            Tab::General => vec![
                Row::new(
                    "accent color",
                    cfg.general.accent.clone().unwrap_or_else(|| "violet".into()),
                    RowKind::Accent,
                ),
                Row::new("banner", on_off(cfg.general.banner()), RowKind::Banner),
                Row::new("cost display", on_off(cfg.general.show_cost()), RowKind::ShowCost),
                Row::new("auto-compact", on_off(cfg.general.auto_compact()), RowKind::AutoCompact)
                    .note("compact when the context is 80% full"),
                Row::new(
                    "read-only commands",
                    on_off(cfg.general.safe_commands().is_some()),
                    RowKind::ReadOnly,
                )
                .note("ls, git status and the like run without asking"),
                Row::new("vim input", on_off(cfg.general.vim()), RowKind::Vim)
                    .note("normal and insert modes in the input; /help lists the keys"),
                Row::new("bash sandbox", on_off(cfg.general.sandbox()), RowKind::Sandbox)
                    .note("scrubbed environment only; not OS isolation, see /doctor"),
                Row::new(
                    "session spend cap",
                    cfg.limits.session_usd.map(|v| format!("${v:.2}")).unwrap_or_else(|| "off".into()),
                    RowKind::SessionCap,
                ),
                Row::new(
                    "daily spend cap",
                    cfg.limits.day_usd.map(|v| format!("${v:.2}")).unwrap_or_else(|| "off".into()),
                    RowKind::DayCap,
                ),
                Row::new(
                    "ZARN.md token budget",
                    cfg.general
                        .context_budget
                        .unwrap_or(zarn_config::context::DEFAULT_BUDGET)
                        .to_string(),
                    RowKind::ContextBudget,
                ),
                Row::new(
                    "theme",
                    cfg.general.theme.clone().unwrap_or_else(|| "auto".into()),
                    RowKind::Theme,
                )
                .note("auto follows COLORFGBG and falls back to dark"),
                Row::new("notifications", on_off(cfg.general.notifications()), RowKind::Notifications)
                    .note("bell and title after turns over 10s and on permission prompts"),
                Row::new(
                    "status line",
                    cfg.general.status_line.clone().unwrap_or_else(|| "built-in".into()),
                    RowKind::StatusLine,
                )
                .note("template with {model} {mode} {profile} {context} {cost} {cwd} {branch}"),
                Row::new("mouse scrolling", on_off(cfg.general.mouse()), RowKind::Mouse)
                    .note("captures the mouse, so native text selection needs shift; see /doctor"),
            ],
        }
    }

    pub fn selected(&self) -> usize {
        let len = self.rows().len();
        self.cursor[self.tab.index()].min(len.saturating_sub(1))
    }

    fn selected_kind(&self) -> Option<RowKind> {
        self.rows().get(self.selected()).map(|r| r.kind.clone())
    }

    fn move_cursor(&mut self, delta: isize) {
        let len = self.rows().len();
        if len == 0 {
            return;
        }
        let i = self.tab.index();
        let current = self.cursor[i].min(len - 1) as isize;
        self.cursor[i] = (current + delta).clamp(0, len as isize - 1) as usize;
    }

    fn switch_tab(&mut self, delta: isize) {
        let i = (self.tab.index() as isize + delta).rem_euclid(TABS.len() as isize) as usize;
        self.tab = TABS[i];
        self.message = None;
    }

    pub fn paste(&mut self, text: &str) {
        match &mut self.editor {
            Some(Editor::Secret { input, .. }) => input.paste(text),
            Some(Editor::Form(form, _)) => form.paste(text),
            Some(Editor::Pick(picker, _)) => picker.filter.paste(text),
            _ => {}
        }
    }

    fn saved(&mut self, what: String, result: anyhow::Result<()>) -> Option<Effect> {
        match result {
            Ok(()) => {
                self.say(what);
                Some(Effect::Reload)
            }
            Err(err) => {
                self.fail(format!("{err:#}"));
                None
            }
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Effect> {
        if let Some(editor) = self.editor.take() {
            return self.on_editor_key(editor, key);
        }

        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Esc => return Some(Effect::Close),
            KeyCode::Char('q') => return Some(Effect::Close),
            KeyCode::Tab | KeyCode::Right => self.switch_tab(1),
            KeyCode::BackTab | KeyCode::Left => self.switch_tab(-1),
            KeyCode::Up if shift => return self.reorder(-1),
            KeyCode::Down if shift => return self.reorder(1),
            KeyCode::Char('[') => return self.reorder(-1),
            KeyCode::Char(']') => return self.reorder(1),
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Home => self.cursor[self.tab.index()] = 0,
            KeyCode::End => self.move_cursor(isize::MAX / 2),
            KeyCode::Enter | KeyCode::Char(' ') => return self.activate(),
            KeyCode::Char(c) => return self.shortcut(c),
            _ => {}
        }
        None
    }

    fn reorder(&mut self, delta: isize) -> Option<Effect> {
        let Some(RowKind::Fallback(profile, i)) = self.selected_kind() else {
            return None;
        };
        let cfg = self.layers.merged();
        let mut order = cfg
            .profiles
            .get(&profile)
            .and_then(|p| p.fallback.clone())
            .unwrap_or_default();
        let j = i as isize + delta;
        if j < 0 || j as usize >= order.len() {
            return None;
        }
        order.swap(i, j as usize);
        let result = ops::set_fallback(&mut self.layers, &profile, order);
        let effect = self.saved("fallback order saved".into(), result);
        if effect.is_some() {
            self.move_cursor(delta);
        }
        effect
    }

    fn shortcut(&mut self, c: char) -> Option<Effect> {
        let kind = self.selected_kind();
        match (self.tab, c, kind) {
            (Tab::Providers, 'a', _) | (Tab::Mcp, 'a', _) | (Tab::Agents, 'a', _) | (Tab::Modes, 'a', _) => {
                let add = match self.tab {
                    Tab::Providers => RowKind::AddProvider,
                    Tab::Mcp => RowKind::AddMcp,
                    Tab::Modes => RowKind::AddMode,
                    _ => RowKind::AddAgent,
                };
                self.activate_kind(add)
            }
            (Tab::Quota, 'R', _) => {
                let names: Vec<String> = self.data.quota.keys().cloned().collect();
                for name in names {
                    self.data.quota.insert(name, QuotaState::Loading);
                }
                Some(Effect::RefreshQuota(None))
            }
            (Tab::Quota, 'r', kind) => {
                let name = match kind {
                    Some(RowKind::QuotaProvider(name)) => Some(name),
                    _ => self.quota_owner(self.selected()),
                }?;
                self.data.quota.insert(name.clone(), QuotaState::Loading);
                Some(Effect::RefreshQuota(Some(name)))
            }
            (Tab::Models, 'd', Some(RowKind::Role(role))) => {
                let result = ops::clear_role(&mut self.layers, &role);
                self.saved(format!("role {role} now uses the main model"), result)
            }
            (Tab::Modes, 'd', Some(RowKind::ModeEntry(i))) => {
                let file = self.modes.get(i)?.clone();
                if file.path.is_none() {
                    self.fail(format!("{} is built in and cannot be deleted", file.def.name));
                    return None;
                }
                self.editor = Some(Editor::Confirm(
                    format!("Delete mode {}? y/n", file.def.name),
                    ConfirmFor::DeleteMode(i),
                ));
                None
            }
            (Tab::Providers, 'k', Some(RowKind::Provider(name))) => {
                self.ask_key(&name, false);
                None
            }
            (Tab::Providers, 't', Some(RowKind::Provider(name))) => {
                self.tests.insert(name.clone(), TestState::Running);
                Some(Effect::Test(name))
            }
            (Tab::Providers, 'd', Some(RowKind::Provider(name))) => {
                self.editor = Some(Editor::Confirm(
                    format!("Delete provider {name} and its stored key? y/n"),
                    ConfirmFor::DeleteProvider(name),
                ));
                None
            }
            (Tab::Models, 'd', Some(RowKind::Fallback(profile, i))) => {
                let cfg = self.layers.merged();
                let mut order = cfg
                    .profiles
                    .get(&profile)
                    .and_then(|p| p.fallback.clone())
                    .unwrap_or_default();
                if i < order.len() {
                    order.remove(i);
                }
                let result = ops::set_fallback(&mut self.layers, &profile, order);
                self.saved("fallback removed".into(), result)
            }
            (Tab::Mcp, 'r', Some(RowKind::Mcp(name))) => {
                self.set_mcp_status(&name, "restarting…");
                Some(Effect::RestartMcp(name))
            }
            (Tab::Mcp, 'd', Some(RowKind::Mcp(name))) => {
                self.editor = Some(Editor::Confirm(
                    format!("Remove MCP server {name}? y/n"),
                    ConfirmFor::DeleteMcp(name),
                ));
                None
            }
            (Tab::Agents, 'd', Some(RowKind::Agent(i))) => {
                let name = self.agents.get(i).map(|a| a.def.name.clone()).unwrap_or_default();
                self.editor = Some(Editor::Confirm(
                    format!("Delete agent {name}? y/n"),
                    ConfirmFor::DeleteAgent(i),
                ));
                None
            }
            (Tab::Permissions, 'a', Some(RowKind::Permission(tool))) => {
                self.set_policy(&tool, Policy::Allow)
            }
            (Tab::Permissions, 's', Some(RowKind::Permission(tool))) => {
                self.set_policy(&tool, Policy::Ask)
            }
            (Tab::Permissions, 'd', Some(RowKind::Rule(policy, rule))) => {
                let result = ops::remove_permission_rule(&mut self.layers, policy, &rule);
                self.saved(format!("rule {rule} deleted"), result)
            }
            (Tab::Permissions, 'd', Some(RowKind::Permission(tool))) => {
                self.set_policy(&tool, Policy::Deny)
            }
            _ => None,
        }
    }

    fn set_policy(&mut self, tool: &str, policy: Policy) -> Option<Effect> {
        let result = ops::set_permission(&mut self.layers, tool, policy);
        match result {
            Ok(scope) => {
                self.say(format!(
                    "{tool}: {} (saved to {} config)",
                    policy_label(policy),
                    scope.label()
                ));
                Some(Effect::Reload)
            }
            Err(err) => {
                self.fail(format!("{err:#}"));
                None
            }
        }
    }

    pub fn ask_key(&mut self, provider: &str, optional: bool) {
        self.editor = Some(Editor::Secret {
            input: MaskedInput::new(),
            provider: provider.to_string(),
            optional,
        });
    }

    fn activate(&mut self) -> Option<Effect> {
        let kind = self.selected_kind()?;
        self.activate_kind(kind)
    }

    fn provider_choices(&self) -> Vec<Choice> {
        self.layers
            .merged()
            .providers
            .iter()
            .map(|(name, spec)| Choice::new(name.clone(), name.clone()).with_detail(spec.kind.clone()))
            .collect()
    }

    pub fn activate_kind(&mut self, kind: RowKind) -> Option<Effect> {
        let cfg = self.layers.merged();
        match kind {
            RowKind::AddProvider => {
                let choices = catalog::KINDS
                    .iter()
                    .map(|k| Choice::new(k.id, k.label))
                    .collect();
                self.editor = Some(Editor::Pick(
                    Picker::new("Provider kind", choices),
                    PickFor::NewProviderKind,
                ));
                None
            }
            RowKind::Provider(name) => {
                let spec = cfg.providers.get(&name)?;
                self.editor = Some(Editor::Form(
                    Form::new(
                        format!("Edit {name} ({})", spec.kind),
                        vec![
                            FormField::new("base_url", spec.base_url.clone().unwrap_or_default())
                                .hint("leave empty for the default endpoint"),
                            FormField::new(
                                "api_key_env",
                                spec.api_key_env.clone().unwrap_or_default(),
                            )
                            .hint("optional: an environment variable that overrides the stored key"),
                        ],
                    ),
                    FormFor::EditProvider(name),
                ));
                None
            }
            RowKind::DefaultProfile => {
                let choices = cfg.profiles.keys().map(Choice::plain).collect();
                self.editor = Some(Editor::Pick(
                    Picker::new("Default profile", choices).select_value(&cfg.default_profile()),
                    PickFor::DefaultProfile,
                ));
                None
            }
            RowKind::ProfileProvider(profile) => {
                let current = cfg
                    .profiles
                    .get(&profile)
                    .and_then(|p| p.provider.clone())
                    .unwrap_or_default();
                self.editor = Some(Editor::Pick(
                    Picker::new(format!("Provider for {profile}"), self.provider_choices())
                        .select_value(&current),
                    PickFor::ProfileProvider(profile),
                ));
                None
            }
            RowKind::ProfileModel(profile) => {
                let p = cfg.profiles.get(&profile).cloned().unwrap_or_default();
                let provider = p.provider.clone().unwrap_or_default();
                let kind = cfg
                    .providers
                    .get(&provider)
                    .map(|s| s.kind.clone())
                    .unwrap_or_default();
                let mut choices: Vec<Choice> = catalog::suggested_models(&kind)
                    .iter()
                    .map(|m| Choice::plain(*m))
                    .collect();
                if let Some(model) = &p.model
                    && !choices.iter().any(|c| &c.value == model)
                {
                    choices.insert(0, Choice::plain(model.clone()));
                }
                self.editor = Some(Editor::Pick(
                    Picker::new(format!("Model for {profile}"), choices)
                        .custom("type any model name, then enter")
                        .select_value(p.model.as_deref().unwrap_or_default()),
                    PickFor::ProfileModel(profile.clone()),
                ));
                Some(Effect::ListModels { provider, profile })
            }
            RowKind::AddFallback(profile) => {
                let p = cfg.profiles.get(&profile).cloned().unwrap_or_default();
                let taken: Vec<String> = p
                    .provider
                    .iter()
                    .chain(p.fallback.iter().flatten())
                    .cloned()
                    .collect();
                let choices: Vec<Choice> = self
                    .provider_choices()
                    .into_iter()
                    .filter(|c| !taken.contains(&c.value))
                    .collect();
                if choices.is_empty() {
                    self.fail("every configured provider is already in this chain");
                    return None;
                }
                self.editor = Some(Editor::Pick(
                    Picker::new(format!("Add a fallback to {profile}"), choices),
                    PickFor::AddFallback(profile),
                ));
                None
            }
            RowKind::Fallback(..) => {
                self.say("use [ and ] (or shift+↑↓) to reorder, d to remove");
                None
            }
            RowKind::AddProfile => {
                self.editor = Some(Editor::Form(
                    Form::new("New profile", vec![FormField::new("name", "")]),
                    FormFor::NewProfile,
                ));
                None
            }
            RowKind::Skill(name) => {
                let enable = !cfg.skill_enabled(&name);
                let result = ops::set_skill_enabled(&mut self.layers, &name, enable).map(|_| ());
                self.saved(
                    format!("skill {name} turned {}", on_off(enable)),
                    result,
                )
            }
            RowKind::AddMcp => {
                self.editor = Some(Editor::Form(
                    Form::new(
                        "Add MCP server",
                        vec![
                            FormField::new("name", ""),
                            FormField::new("command or url", "")
                                .hint("e.g. npx -y @modelcontextprotocol/server-filesystem . or https://host/mcp"),
                            FormField::new("scope", "user").hint("user or project"),
                        ],
                    ),
                    FormFor::NewMcp,
                ));
                None
            }
            RowKind::Mcp(name) => {
                let server = cfg.mcp.get(&name)?;
                let line = match (&server.command, &server.url) {
                    (Some(cmd), _) => {
                        let mut words = vec![cmd.clone()];
                        words.extend(server.args.iter().cloned());
                        join_words(&words)
                    }
                    (None, Some(url)) => url.clone(),
                    _ => String::new(),
                };
                self.editor = Some(Editor::Form(
                    Form::new(
                        format!("Edit MCP server {name}"),
                        vec![FormField::new("command or url", line)],
                    ),
                    FormFor::EditMcp(name),
                ));
                None
            }
            RowKind::AddAgent => {
                self.editor = Some(Editor::Form(agent_form("Create agent", None), FormFor::NewAgent));
                None
            }
            RowKind::Agent(i) => {
                let file = self.agents.get(i)?.clone();
                self.editor = Some(Editor::Form(
                    agent_form(&format!("Edit agent {}", file.def.name), Some(&file)),
                    FormFor::EditAgent(i),
                ));
                None
            }
            RowKind::Rule(_, rule) => {
                self.say(format!("{rule}: press d to delete it"));
                None
            }
            RowKind::QuotaProvider(name) => {
                self.data.quota.insert(name.clone(), QuotaState::Loading);
                Some(Effect::RefreshQuota(Some(name)))
            }
            RowKind::Info => None,
            RowKind::Routing => {
                let value = !cfg.routing.enabled();
                let result = ops::set_routing_enabled(&mut self.layers, value).map(|_| ());
                self.saved(format!("routing {}", on_off(value)), result)
            }
            RowKind::Role(role) => {
                let mut choices = vec![Choice::new("", "main model").with_detail("clear this role")];
                choices.extend(self.provider_choices());
                let current = cfg
                    .routing
                    .roles
                    .get(&role)
                    .and_then(|r| r.provider.clone())
                    .unwrap_or_default();
                self.editor = Some(Editor::Pick(
                    Picker::new(format!("Provider for the {role} role"), choices).select_value(&current),
                    PickFor::RoleProvider(role),
                ));
                None
            }
            RowKind::ActiveMode => {
                let choices = self
                    .modes
                    .iter()
                    .map(|m| {
                        Choice::new(m.def.name.clone(), m.def.name.clone())
                            .with_detail(format!("{} · {}", m.source.label(), m.def.description))
                    })
                    .collect();
                self.editor = Some(Editor::Pick(
                    Picker::new("Active mode", choices).select_value(&self.active_mode),
                    PickFor::ActiveMode,
                ));
                None
            }
            RowKind::ModeEntry(i) => {
                let file = self.modes.get(i)?.clone();
                if file.path.is_none() {
                    self.say(format!(
                        "{} is built in: {}. Create a custom mode to change behaviour.",
                        file.def.name, file.def.description
                    ));
                    return None;
                }
                self.editor = Some(Editor::Form(
                    mode_form(&format!("Edit mode {}", file.def.name), Some(&file)),
                    FormFor::EditMode(i),
                ));
                None
            }
            RowKind::AddMode => {
                self.editor = Some(Editor::Form(mode_form("Create mode", None), FormFor::NewMode));
                None
            }
            RowKind::Permission(tool) => {
                let current = cfg
                    .permissions
                    .get(&tool)
                    .copied()
                    .or_else(|| {
                        self.data
                            .tools
                            .iter()
                            .find(|(t, _)| *t == tool)
                            .map(|(_, p)| *p)
                    })
                    .unwrap_or(Policy::Ask);
                self.set_policy(&tool, next_policy(current))
            }
            RowKind::Accent => {
                let choices = brand::ACCENTS
                    .iter()
                    .map(|(name, _)| Choice::plain(*name))
                    .collect();
                self.editor = Some(Editor::Pick(
                    Picker::new("Accent color", choices)
                        .custom("or type a hex color like #ff8800")
                        .select_value(cfg.general.accent.as_deref().unwrap_or("violet")),
                    PickFor::Accent,
                ));
                None
            }
            RowKind::Theme => {
                let choices = crate::theme::NAMES.iter().map(|n| Choice::plain(*n)).collect();
                self.editor = Some(Editor::Pick(
                    Picker::new("Theme", choices).select_value(cfg.general.theme.as_deref().unwrap_or("auto")),
                    PickFor::Theme,
                ));
                None
            }
            RowKind::Notifications => {
                let value = !cfg.general.notifications();
                let result = ops::set_general(&mut self.layers, GeneralField::Notifications, |g| {
                    g.notifications = Some(value)
                });
                self.saved(format!("notifications {}", on_off(value)), result)
            }
            RowKind::Mouse => {
                let value = !cfg.general.mouse();
                let result = ops::set_general(&mut self.layers, GeneralField::Mouse, |g| g.mouse = Some(value));
                self.saved(format!("mouse scrolling {}", on_off(value)), result)
            }
            RowKind::StatusLine => {
                let mut form = Form::new(
                    "Status line",
                    vec![FormField::new("template", cfg.general.status_line.clone().unwrap_or_default())
                        .hint("{model} {mode} {profile} {context} {cost} {cwd} {branch}; empty resets")],
                );
                form.preview = Some(self.status_values.clone());
                self.editor = Some(Editor::Form(form, FormFor::StatusLine));
                None
            }
            RowKind::Banner => {
                let value = !cfg.general.banner();
                let result = ops::set_general(&mut self.layers, GeneralField::Banner, |g| {
                    g.banner = Some(value)
                });
                self.saved(format!("banner {}", on_off(value)), result)
            }
            RowKind::ShowCost => {
                let value = !cfg.general.show_cost();
                let result = ops::set_general(&mut self.layers, GeneralField::ShowCost, |g| {
                    g.show_cost = Some(value)
                });
                self.saved(format!("cost display {}", on_off(value)), result)
            }
            RowKind::AutoCompact => {
                let value = !cfg.general.auto_compact();
                let result = ops::set_general(&mut self.layers, GeneralField::AutoCompact, |g| {
                    g.auto_compact = Some(value)
                });
                self.saved(format!("auto-compact {}", on_off(value)), result)
            }
            RowKind::ReadOnly => {
                let value = cfg.general.safe_commands().is_none();
                let result = ops::set_general(&mut self.layers, GeneralField::ReadOnly, |g| {
                    g.auto_allow_read_only = Some(value)
                });
                self.saved(format!("read-only commands without asking: {}", on_off(value)), result)
            }
            RowKind::Vim => {
                let value = !cfg.general.vim();
                let result = ops::set_general(&mut self.layers, GeneralField::Vim, |g| g.vim = Some(value));
                self.saved(format!("vim input {}", on_off(value)), result)
            }
            RowKind::Sandbox => {
                let value = !cfg.general.sandbox();
                let result = ops::set_general(&mut self.layers, GeneralField::Sandbox, |g| {
                    g.sandbox = Some(value)
                });
                self.saved(format!("bash sandbox {}", on_off(value)), result)
            }
            RowKind::SessionCap | RowKind::DayCap => {
                let day = kind == RowKind::DayCap;
                let current = if day { cfg.limits.day_usd } else { cfg.limits.session_usd };
                self.editor = Some(Editor::Form(
                    Form::new(
                        if day { "Daily spend cap" } else { "Session spend cap" },
                        vec![FormField::new("usd", current.map(|v| v.to_string()).unwrap_or_default())
                            .hint("dollars, or off; only priced models can be capped")],
                    ),
                    FormFor::SpendCap(day),
                ));
                None
            }
            RowKind::ContextBudget => {
                let current = cfg
                    .general
                    .context_budget
                    .unwrap_or(zarn_config::context::DEFAULT_BUDGET);
                self.editor = Some(Editor::Form(
                    Form::new(
                        "ZARN.md token budget",
                        vec![FormField::new("tokens", current.to_string())],
                    ),
                    FormFor::ContextBudget,
                ));
                None
            }
        }
    }

    fn on_editor_key(&mut self, editor: Editor, key: KeyEvent) -> Option<Effect> {
        match editor {
            Editor::Confirm(text, purpose) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.confirm(purpose),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => None,
                _ => {
                    self.editor = Some(Editor::Confirm(text, purpose));
                    None
                }
            },
            Editor::Secret {
                mut input,
                provider,
                optional,
            } => match input.on_key(key) {
                Edit::Cancel => {
                    self.say("key unchanged");
                    None
                }
                Edit::Submit => {
                    let key = input.take();
                    if key.is_empty() {
                        if optional {
                            self.say(format!("{provider} saved without a key"));
                            return Some(Effect::Reload);
                        }
                        self.fail("the key is empty; paste it, or press esc to cancel");
                        self.editor = Some(Editor::Secret {
                            input,
                            provider,
                            optional,
                        });
                        return None;
                    }
                    match self.store.save(&provider, &key) {
                        Ok(source) => {
                            let place = source.describe(&self.store);
                            let mut text = format!("key for {provider} saved to {place}; press t to test");
                            if let Some(notice) = self.store.fallback_notice() {
                                text = format!("{text}. {notice}");
                            }
                            self.say(text);
                            self.tests.remove(&provider);
                            Some(Effect::Reload)
                        }
                        Err(err) => {
                            self.fail(err.to_string());
                            None
                        }
                    }
                }
                _ => {
                    self.editor = Some(Editor::Secret {
                        input,
                        provider,
                        optional,
                    });
                    None
                }
            },
            Editor::Pick(mut picker, purpose) => match picker.on_key(key) {
                Pick::Cancel => None,
                Pick::Pending => {
                    self.editor = Some(Editor::Pick(picker, purpose));
                    None
                }
                Pick::Chosen(value) | Pick::Custom(value) => self.picked(purpose, value),
            },
            Editor::Form(mut form, purpose) => match form.on_key(key) {
                FormEdit::Cancel => None,
                FormEdit::Pending => {
                    self.editor = Some(Editor::Form(form, purpose));
                    None
                }
                FormEdit::Submit => self.submit(form, purpose),
            },
        }
    }

    fn confirm(&mut self, purpose: ConfirmFor) -> Option<Effect> {
        match purpose {
            ConfirmFor::DeleteProvider(name) => {
                let result = ops::remove_provider(&mut self.layers, &self.store, &name).map(|_| ());
                self.tests.remove(&name);
                self.saved(format!("provider {name} deleted"), result)
            }
            ConfirmFor::DeleteMcp(name) => {
                let result = ops::mcp_remove(&mut self.layers, &name);
                self.data.mcp_status.remove(&name);
                self.saved(format!("MCP server {name} removed"), result)
            }
            ConfirmFor::DeleteMode(i) => {
                let file = self.modes.get(i).cloned()?;
                let result = modes::delete(&file);
                self.reload_modes();
                self.saved(format!("mode {} deleted", file.def.name), result)
            }
            ConfirmFor::DeleteAgent(i) => {
                let file = self.agents.get(i).cloned()?;
                let result = agents::delete(&file);
                self.reload_agents();
                self.saved(format!("agent {} deleted", file.def.name), result)
            }
        }
    }

    fn picked(&mut self, purpose: PickFor, value: String) -> Option<Effect> {
        match purpose {
            PickFor::NewProviderKind => {
                let kind = catalog::kind(&value)?;
                let cfg = self.layers.merged();
                let mut name = kind.id.to_string();
                let mut n = 2;
                while cfg.providers.contains_key(&name) {
                    name = format!("{}-{n}", kind.id);
                    n += 1;
                }
                let mut fields = vec![FormField::new("name", name)];
                if kind.asks_base_url {
                    fields.push(
                        FormField::new("base_url", kind.default_base_url.unwrap_or_default())
                            .hint("the API endpoint, e.g. https://api.groq.com/openai/v1"),
                    );
                }
                fields.push(
                    FormField::new("model", kind.models.first().copied().unwrap_or_default())
                        .hint("used when this becomes the default provider"),
                );
                self.editor = Some(Editor::Form(
                    Form::new(format!("Add {}", kind.label), fields),
                    FormFor::NewProvider(kind.id.to_string()),
                ));
                None
            }
            PickFor::DefaultProfile => {
                let result = ops::set_default_profile(&mut self.layers, &value);
                self.saved(format!("default profile is now {value}"), result)
            }
            PickFor::ProfileProvider(profile) => {
                let result = ops::set_profile_provider(&mut self.layers, &profile, &value);
                self.saved(format!("{profile} now uses {value}"), result)
            }
            PickFor::ProfileModel(profile) => {
                let result = ops::set_profile_model(&mut self.layers, &profile, &value);
                self.saved(format!("{profile} now uses model {value}"), result)
            }
            PickFor::AddFallback(profile) => {
                let mut order = self
                    .layers
                    .merged()
                    .profiles
                    .get(&profile)
                    .and_then(|p| p.fallback.clone())
                    .unwrap_or_default();
                order.push(value.clone());
                let result = ops::set_fallback(&mut self.layers, &profile, order);
                self.saved(format!("{value} added to the fallback chain"), result)
            }
            PickFor::RoleProvider(role) => {
                if value.is_empty() {
                    let result = ops::clear_role(&mut self.layers, &role);
                    return self.saved(format!("role {role} now uses the main model"), result);
                }
                let cfg = self.layers.merged();
                let kind = cfg.providers.get(&value).map(|s| s.kind.clone()).unwrap_or_default();
                let choices: Vec<Choice> = catalog::suggested_models(&kind)
                    .iter()
                    .map(|m| Choice::plain(*m))
                    .collect();
                self.editor = Some(Editor::Pick(
                    Picker::new(format!("Model for the {role} role on {value}"), choices)
                        .custom("type any model name, then enter"),
                    PickFor::RoleModel(role.clone(), value.clone()),
                ));
                Some(Effect::ListModels {
                    provider: value,
                    profile: format!("role:{role}"),
                })
            }
            PickFor::RoleModel(role, provider) => {
                let result = ops::set_role(&mut self.layers, &role, Some(&provider), Some(&value)).map(|_| ());
                self.saved(format!("role {role} now uses {provider} · {value}"), result)
            }
            PickFor::ActiveMode => {
                if value != "bypass" {
                    let result = ops::set_general(&mut self.layers, GeneralField::Mode, |g| {
                        g.mode = Some(value.clone())
                    });
                    if let Err(err) = result {
                        self.fail(format!("{err:#}"));
                        return None;
                    }
                }
                self.active_mode = value.clone();
                self.say(format!("mode is now {value}"));
                Some(Effect::SetMode(value))
            }
            PickFor::Theme => {
                let result = ops::set_general(&mut self.layers, GeneralField::Theme, |g| {
                    g.theme = Some(value.clone())
                });
                self.saved(format!("theme is now {value}"), result)
            }
            PickFor::Accent => {
                if !brand::ACCENTS.iter().any(|(n, _)| *n == value) && brand::parse_hex(&value).is_none() {
                    self.fail(format!("'{value}' is not a color name or #rrggbb"));
                    return None;
                }
                let result = ops::set_general(&mut self.layers, GeneralField::Accent, |g| {
                    g.accent = Some(value.clone())
                });
                self.saved(format!("accent color is now {value}"), result)
            }
        }
    }

    fn submit(&mut self, form: Form, purpose: FormFor) -> Option<Effect> {
        let retry = |panel: &mut Panel, form: Form, purpose: FormFor, err: String| {
            panel.fail(err);
            panel.editor = Some(Editor::Form(form, purpose));
            None
        };
        match purpose {
            FormFor::NewProvider(kind_id) => {
                let draft = ProviderDraft {
                    name: form.value("name"),
                    kind: kind_id.clone(),
                    base_url: Some(form.value("base_url")).filter(|v| !v.is_empty()),
                    api_key_env: None,
                    model: Some(form.value("model")).filter(|v| !v.is_empty()),
                };
                if self.layers.merged().providers.contains_key(&draft.name) {
                    return retry(self, form, FormFor::NewProvider(kind_id), format!("a provider named {} already exists", draft.name));
                }
                if catalog::kind(&kind_id).is_some_and(|k| k.asks_base_url)
                    && draft.base_url.is_none()
                {
                    return retry(self, form, FormFor::NewProvider(kind_id), "this kind needs a base_url".into());
                }
                match ops::add_provider(&mut self.layers, &draft) {
                    Ok(scope) => {
                        let needs = catalog::needs_key(&draft.spec());
                        let optional = !needs;
                        let wants_key = needs || kind_id == "openai-compat";
                        if wants_key {
                            self.ask_key(&draft.name, optional);
                            self.say(format!(
                                "{} added to the {} config; paste its API key{}",
                                draft.name,
                                scope.label(),
                                if optional { " (or press enter to skip)" } else { "" }
                            ));
                        } else {
                            self.say(format!("{} added; press t to test", draft.name));
                        }
                        let i = self.tab.index();
                        let rows = self.rows();
                        if let Some(pos) = rows
                            .iter()
                            .position(|r| r.kind == RowKind::Provider(draft.name.clone()))
                        {
                            self.cursor[i] = pos;
                        }
                        Some(Effect::Reload)
                    }
                    Err(err) => retry(self, form, FormFor::NewProvider(kind_id), format!("{err:#}")),
                }
            }
            FormFor::EditProvider(name) => {
                let base_url = Some(form.value("base_url")).filter(|v| !v.is_empty());
                let env = Some(form.value("api_key_env")).filter(|v| !v.is_empty());
                let result = ops::update_provider(&mut self.layers, &name, |spec| {
                    spec.base_url = base_url;
                    spec.api_key_env = env;
                })
                .map(|_| ());
                self.tests.remove(&name);
                self.saved(format!("{name} saved"), result)
            }
            FormFor::NewMcp | FormFor::EditMcp(_) => {
                let name = match &purpose {
                    FormFor::EditMcp(name) => name.clone(),
                    _ => form.value("name"),
                };
                let scope = match form.value("scope").as_str() {
                    "project" => Some(Scope::Project),
                    "user" => Some(Scope::User),
                    "" => None,
                    other => {
                        return retry(self, form, purpose, format!("scope must be user or project, not {other}"));
                    }
                };
                let words = split_words(&form.value("command or url"));
                let server = match ops::parse_server(&words) {
                    Ok(server) => server,
                    Err(err) => return retry(self, form, purpose, format!("{err:#}")),
                };
                match ops::mcp_add(&mut self.layers, &name, server, scope) {
                    Ok(_) => {
                        self.say(format!("MCP server {name} saved; starting it"));
                        self.set_mcp_status(&name, "starting…");
                        Some(Effect::RestartMcp(name))
                    }
                    Err(err) => retry(self, form, purpose, format!("{err:#}")),
                }
            }
            FormFor::NewAgent | FormFor::EditAgent(_) => {
                let def = AgentDef {
                    name: form.value("name"),
                    description: form.value("description"),
                    tools: agents::split_tools(&form.value("tools")),
                    model: Some(form.value("model")).filter(|m| !m.is_empty()),
                    instructions: form.value("instructions"),
                };
                if def.description.is_empty() {
                    return retry(self, form, purpose, "an agent needs a description so the model knows when to use it".into());
                }
                let previous = match &purpose {
                    FormFor::EditAgent(i) => self.agents.get(*i).cloned(),
                    _ => None,
                };
                let scope = match form.value("scope").as_str() {
                    "project" => Scope::Project,
                    "user" | "" => Scope::User,
                    other => {
                        return retry(self, form, purpose, format!("scope must be user or project, not {other}"));
                    }
                };
                let dir = match scope {
                    Scope::Project => Some(self.agent_dirs.1.clone()),
                    Scope::User => self.agent_dirs.0.clone(),
                };
                let Some(dir) = dir else {
                    return retry(self, form, purpose, "no home directory for user agents".into());
                };
                match agents::save_in(&dir, &def) {
                    Ok(path) => {
                        if let Some(old) = previous
                            && old.path != path
                        {
                            let _ = agents::delete(&old);
                        }
                        self.reload_agents();
                        self.say(format!("agent {} saved", def.name));
                        Some(Effect::Reload)
                    }
                    Err(err) => retry(self, form, purpose, format!("{err:#}")),
                }
            }
            FormFor::SpendCap(day) => {
                let result = ops::parse_cap(&form.value("usd"))
                    .and_then(|usd| ops::set_spend_cap(&mut self.layers, day, usd).map(|_| usd));
                match result {
                    Ok(usd) => {
                        let label = if day { "daily" } else { "session" };
                        self.say(match usd {
                            Some(v) => format!("{label} spend cap is now ${v:.2}"),
                            None => format!("{label} spend cap removed"),
                        });
                        Some(Effect::Reload)
                    }
                    Err(err) => retry(self, form, purpose, format!("{err:#}")),
                }
            }
            FormFor::StatusLine => {
                let source = form.value("template");
                if source.is_empty() {
                    let result = ops::set_general(&mut self.layers, GeneralField::StatusLine, |g| g.status_line = None);
                    return self.saved("status line reset to the built-in one".to_string(), result);
                }
                match crate::status_line::Template::parse(&source) {
                    Ok(_) => {
                        let result = ops::set_general(&mut self.layers, GeneralField::StatusLine, |g| {
                            g.status_line = Some(source.clone())
                        });
                        self.saved(format!("status line is now {source}"), result)
                    }
                    Err(err) => retry(self, form, purpose, format!("{err}; the previous status line is kept")),
                }
            }
            FormFor::ContextBudget => match form.value("tokens").parse::<u32>() {
                Ok(n) if n > 0 => {
                    let result = ops::set_general(&mut self.layers, GeneralField::ContextBudget, |g| {
                        g.context_budget = Some(n)
                    });
                    self.saved(format!("ZARN.md budget is now {n} tokens"), result)
                }
                _ => retry(self, form, purpose, "enter a positive number of tokens".into()),
            },
            FormFor::NewMode | FormFor::EditMode(_) => {
                let def = match mode_from_form(&form) {
                    Ok(def) => def,
                    Err(err) => return retry(self, form, purpose, err),
                };
                let previous = match &purpose {
                    FormFor::EditMode(i) => self.modes.get(*i).cloned(),
                    _ => None,
                };
                let dir = match form.value("scope").as_str() {
                    "project" => Some(self.mode_dirs.1.clone()),
                    "user" | "" => self.mode_dirs.0.clone(),
                    other => {
                        return retry(self, form, purpose, format!("scope must be user or project, not {other}"));
                    }
                };
                let Some(dir) = dir else {
                    return retry(self, form, purpose, "no home directory for user modes".into());
                };
                match modes::save_in(&dir, &def) {
                    Ok(path) => {
                        if let Some(old) = previous
                            && old.path.as_ref().is_some_and(|p| *p != path)
                        {
                            let _ = modes::delete(&old);
                        }
                        self.reload_modes();
                        self.say(format!("mode {} saved to {}", def.name, path.display()));
                        Some(Effect::Reload)
                    }
                    Err(err) => retry(self, form, purpose, format!("{err:#}")),
                }
            }
            FormFor::NewProfile => {
                let name = form.value("name");
                match ops::add_profile(&mut self.layers, &name) {
                    Ok(()) => {
                        self.say(format!("profile {name} created from the default profile"));
                        Some(Effect::Reload)
                    }
                    Err(err) => retry(self, form, purpose, format!("{err:#}")),
                }
            }
        }
    }
}

pub fn merge_models(mut choices: Vec<Choice>, models: Vec<(String, Option<u32>)>) -> Vec<Choice> {
    for (model, window) in models {
        let detail = window.map(|w| zarn_config::quota::context_label(Some(w)));
        match choices.iter_mut().find(|c| c.value == model) {
            Some(existing) => {
                if let Some(detail) = detail {
                    existing.detail = detail;
                }
            }
            None => {
                let mut choice = Choice::plain(model);
                if let Some(detail) = detail {
                    choice = choice.with_detail(detail);
                }
                choices.push(choice);
            }
        }
    }
    choices
}

fn mode_form(title: &str, file: Option<&ModeFile>) -> Form {
    let def = file.map(|f| f.def.clone()).unwrap_or_default();
    let scope = file.map(|f| f.source.label()).unwrap_or("user");
    let rendered = modes::render(&def);
    let permissions = rendered
        .lines()
        .find_map(|l| l.strip_prefix("permissions: ").map(str::to_string))
        .unwrap_or_default();
    Form::new(
        title,
        vec![
            FormField::new("name", def.name),
            FormField::new("description", def.description),
            FormField::new("tools", def.tools.join(", ")).hint("comma separated; empty means every tool"),
            FormField::new("permissions", permissions).hint("e.g. edit=deny, write=deny, bash=ask"),
            FormField::new("profile", def.profile.unwrap_or_default()).hint("optional profile to switch to"),
            FormField::new("model", def.model.unwrap_or_default()).hint("optional model to switch to"),
            FormField::new("scope", scope).hint("user or project"),
            FormField::new("prompt", def.prompt)
                .hint("added to the system prompt; shift+enter for a new line, ctrl+s saves")
                .multiline(),
        ],
    )
}

fn mode_from_form(form: &Form) -> Result<ModeDef, String> {
    let mut text = String::from("---\n");
    for key in ["name", "description", "tools", "permissions", "profile", "model"] {
        let value = form.value(key).replace('\n', " ");
        if !value.is_empty() {
            text.push_str(&format!("{key}: {value}\n"));
        }
    }
    text.push_str("---\n");
    text.push_str(&form.value("prompt"));
    let def = modes::parse(&text)?;
    if modes::is_builtin(&def.name) {
        return Err(format!("{} is a built-in mode; pick another name", def.name));
    }
    Ok(def)
}

fn agent_form(title: &str, file: Option<&AgentFile>) -> Form {
    let def = file.map(|f| f.def.clone()).unwrap_or_default();
    let scope = file.map(|f| f.scope.label()).unwrap_or("user");
    Form::new(
        title,
        vec![
            FormField::new("name", def.name),
            FormField::new("description", def.description)
                .hint("when the model should delegate to this agent"),
            FormField::new("tools", def.tools.join(", ")).hint("comma separated; empty means all builtins"),
            FormField::new("model", def.model.unwrap_or_default()).hint("empty uses the session model"),
            FormField::new("scope", scope).hint("user or project"),
            FormField::new("instructions", def.instructions)
                .hint("shift+enter for a new line, enter or ctrl+s to save")
                .multiline(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_split_on_spaces_and_respect_quotes() {
        assert_eq!(
            split_words(r#"npx -y "my server" 'a b' plain"#),
            vec!["npx", "-y", "my server", "a b", "plain"]
        );
        assert_eq!(split_words(""), Vec::<String>::new());
    }

    #[test]
    fn joining_quotes_words_with_spaces() {
        assert_eq!(join_words(&["a".into(), "b c".into()]), "a \"b c\"");
    }
}
