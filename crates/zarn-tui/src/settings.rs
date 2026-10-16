use crate::brand;
use crate::widgets::{Choice, Edit, MaskedInput, Pick, Picker, TextField};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use zarn_config::agents::{self, AgentFile};
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
    Permissions,
    General,
}

pub const TABS: [Tab; 7] = [
    Tab::Providers,
    Tab::Models,
    Tab::Skills,
    Tab::Mcp,
    Tab::Agents,
    Tab::Permissions,
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
            Tab::Permissions => "Permissions",
            Tab::General => "General",
        }
    }

    pub fn index(&self) -> usize {
        TABS.iter().position(|t| t == self).unwrap_or(0)
    }

    pub fn hints(&self) -> &'static str {
        match self {
            Tab::Providers => "enter edit · a add · k key · t test · d delete",
            Tab::Models => "enter change · [ ] or shift+↑↓ reorder fallback · d remove fallback",
            Tab::Skills => "enter or space toggles",
            Tab::Mcp => "enter edit · a add · r restart · d delete",
            Tab::Agents => "enter edit · a create · d delete",
            Tab::Permissions => "enter cycles · a allow · s ask · d deny",
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PanelData {
    pub skills: Vec<SkillRow>,
    pub mcp_status: BTreeMap<String, String>,
    pub tools: Vec<(String, Policy)>,
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
    Permission(String),
    Accent,
    Banner,
    ShowCost,
    ContextBudget,
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfirmFor {
    DeleteProvider(String),
    DeleteMcp(String),
    DeleteAgent(usize),
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
    pub cursor: [usize; 7],
    pub editor: Option<Editor>,
    pub message: Option<(String, bool)>,
    pub tests: BTreeMap<String, TestState>,
    store: Arc<KeyStore>,
    agent_dirs: (Option<PathBuf>, PathBuf),
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
        Panel {
            tab: Tab::Providers,
            layers,
            data,
            agents,
            cursor: [0; 7],
            editor: None,
            message: None,
            tests: BTreeMap::new(),
            store,
            agent_dirs: (user_agents, project_agents),
        }
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

    pub fn set_models(&mut self, profile: &str, models: Vec<String>) {
        if let Some(Editor::Pick(picker, PickFor::ProfileModel(p))) = &mut self.editor
            && p == profile
        {
            let mut choices = picker.choices.clone();
            for model in models {
                if !choices.iter().any(|c| c.value == model) {
                    choices.push(Choice::plain(model));
                }
            }
            picker.set_choices(choices);
        }
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
                            None => String::new(),
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
                Row::new(
                    "ZARN.md token budget",
                    cfg.general
                        .context_budget
                        .unwrap_or(zarn_config::context::DEFAULT_BUDGET)
                        .to_string(),
                    RowKind::ContextBudget,
                ),
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
            (Tab::Providers, 'a', _) | (Tab::Mcp, 'a', _) | (Tab::Agents, 'a', _) => {
                let add = match self.tab {
                    Tab::Providers => RowKind::AddProvider,
                    Tab::Mcp => RowKind::AddMcp,
                    _ => RowKind::AddAgent,
                };
                self.activate_kind(add)
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
            FormFor::ContextBudget => match form.value("tokens").parse::<u32>() {
                Ok(n) if n > 0 => {
                    let result = ops::set_general(&mut self.layers, GeneralField::ContextBudget, |g| {
                        g.context_budget = Some(n)
                    });
                    self.saved(format!("ZARN.md budget is now {n} tokens"), result)
                }
                _ => retry(self, form, purpose, "enter a positive number of tokens".into()),
            },
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
