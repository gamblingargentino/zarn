use crate::brand;
use crate::onboarding::{self, Onboarding};
use crate::popup::{self, Entry};
use crate::settings::{self, Panel};
use crate::widgets::{Edit, MaskedInput, Pick, Picker, Secret};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::Color;
use serde_json::Value;
use tokio::sync::oneshot;
use zarn_core::agent::AgentEvent;
use zarn_core::provider::Event;
use zarn_core::tools::Risk;
use zarn_tools::exit_plan::PlanVerdict;
use zarn_tools::permissions::Answer;

pub const COLLAPSED_LINES: usize = 3;
pub const PASTE_LINES: usize = 20;
pub const PASTE_CHARS: usize = 2_000;
pub const IMAGE_MARK: &str = "[[zarn-image:";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Vim {
    pub normal: bool,
    pub pending_d: bool,
}

pub const VIM_HELP: &str = "vim input (General → vim input): esc normal mode · i insert · a append · o open line below · h j k l move · w b word · 0 $ line start and end · x delete char · dd delete line · enter sends";

#[derive(Clone, Debug, PartialEq)]
pub struct AgentRow {
    pub id: String,
    pub agent: String,
    pub tool: Option<String>,
    pub started: std::time::Instant,
    pub tokens: u64,
    pub background: bool,
    pub done: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Search {
    pub query: String,
    pub typing: bool,
    pub hit: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paste {
    pub token: String,
    pub text: String,
}

#[derive(Debug)]
pub struct PermissionRequest {
    pub tool: String,
    pub risk: Risk,
    pub args: Value,
    pub reply: oneshot::Sender<Answer>,
    pub pattern: String,
    pub preview: Option<zarn_tools::diff::Preview>,
}

impl PermissionRequest {
    pub fn new(tool: impl Into<String>, risk: Risk, args: Value, reply: oneshot::Sender<Answer>) -> Self {
        let tool = tool.into();
        PermissionRequest {
            pattern: tool.clone(),
            tool,
            risk,
            args,
            reply,
            preview: None,
        }
    }
}

#[derive(Debug)]
pub struct PlanReview {
    pub plan: String,
    pub reply: Option<oneshot::Sender<PlanVerdict>>,
    pub feedback: Option<crate::widgets::TextField>,
    pub decided: Option<PlanVerdict>,
}

impl PlanReview {
    pub fn new(plan: impl Into<String>, reply: oneshot::Sender<PlanVerdict>) -> Self {
        PlanReview {
            plan: plan.into(),
            reply: Some(reply),
            feedback: None,
            decided: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CardStatus {
    #[default]
    Pending,
    Running,
    Done,
    Failed,
}

impl CardStatus {
    pub fn label(&self) -> &'static str {
        match self {
            CardStatus::Pending => "pending",
            CardStatus::Running => "running",
            CardStatus::Done => "done",
            CardStatus::Failed => "failed",
        }
    }
}

#[derive(Debug, Default)]
pub struct ToolCard {
    pub id: String,
    pub name: String,
    pub args: Value,
    pub status: CardStatus,
    pub output: Option<String>,
    pub expanded: bool,
    pub detail: Option<String>,
    pub live: Vec<String>,
}

pub const LIVE_LINES: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareSide {
    pub label: String,
    pub text: String,
    pub meta: String,
    pub failed: bool,
}

#[derive(Debug)]
pub enum Block {
    Compare(Vec<CompareSide>),
    User(String),
    Assistant(String),
    Tool(ToolCard),
    Notice(String),
    Queued(String),
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cost: Option<f64>,
    pub context: Option<(u32, u32)>,
    pub mode: String,
    pub profile: String,
    pub cwd: String,
    pub branch: String,
    pub turn_started: Option<std::time::Instant>,
    pub streamed_chars: usize,
}

impl Status {
    pub fn turn_tokens(&self) -> u64 {
        (self.streamed_chars as u64).div_ceil(4)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatPick {
    Model,
    Theme,
    SwitchProvider,
    Resume,
    Login,
    Logout,
    Mode,
    Profile,
    Rewind,
    Memory(String),
    History,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfirmFor {
    Bypass,
    Commit(String),
    Prompted(String),
    Rewind(usize),
}

#[derive(Debug)]
pub enum Overlay {
    Pick(Picker, ChatPick),
    Secret {
        input: MaskedInput,
        provider: String,
    },
    Confirm(String, ConfirmFor),
}

#[derive(Default)]
pub enum Mode {
    #[default]
    Chat,
    Panel(Box<Panel>),
    Onboarding(Box<Onboarding>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Submit(String),
    Command(String),
    Cancel,
    Quit,
    Panel(settings::Effect),
    Onboard(onboarding::Effect),
    Picked(ChatPick, String),
    Login(String, Secret),
    SendQueued,
    CycleMode,
    Confirmed(ConfirmFor),
    Plan(PlanVerdict),
    SaveRule(String, zarn_config::layers::Scope),
    Shell(String),
    Remember(String),
    Editor,
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub accent: Color,
    pub banner: bool,
    pub show_cost: bool,
    pub palette: crate::theme::Palette,
    pub status_line: Option<crate::status_line::Template>,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            accent: brand::ACCENT,
            banner: true,
            show_cost: true,
            palette: crate::theme::DARK,
            status_line: None,
        }
    }
}

#[derive(Default)]
pub struct App {
    pub blocks: Vec<Block>,
    pub input: String,
    pub cursor: usize,
    pub history: Vec<String>,
    pub history_pos: Option<usize>,
    pub scroll_up: u16,
    pub streaming: bool,
    pub tick: usize,
    pub pending: Option<PermissionRequest>,
    pub status: Status,
    pub mode: Mode,
    pub overlay: Option<Overlay>,
    pub entries: Vec<Entry>,
    pub args: std::collections::BTreeMap<String, Vec<(String, String)>>,
    pub popup_selected: usize,
    pub popup_hidden: bool,
    pub theme: Theme,
    pub not_ready: Option<String>,
    pub queue: Vec<String>,
    pub queue_paused: bool,
    pub plan_review: Option<PlanReview>,
    pub pending_expanded: bool,
    pub files: Vec<String>,
    pub keymap: crate::keymap::Keymap,
    pub transcript: bool,
    pub todos: Vec<zarn_core::session::Todo>,
    pub vim: Option<Vim>,
    pub transcript_scroll: u16,
    pub search: Option<Search>,
    pub pastes: Vec<Paste>,
    pub paste_count: usize,
    pub queue_display: Vec<String>,
    pub focused: Option<bool>,
    pub agents: Vec<AgentRow>,
    pub hits: crate::hits::Hits,
    pub permission_choice: usize,
    pub plan_choice: usize,
    pub confirm_choice: usize,
    ctrl_c_armed: bool,
    esc_armed: bool,
}

impl App {
    pub fn new(model: impl Into<String>) -> Self {
        App {
            status: Status {
                model: model.into(),
                ..Status::default()
            },
            entries: popup::builtin_entries(),
            ..App::default()
        }
    }

    pub fn mention(&self) -> Option<(usize, String)> {
        if self.input.starts_with('/') {
            return None;
        }
        crate::mentions::token_at(&self.input, self.cursor)
    }

    pub fn mention_entries(&self) -> Vec<Entry> {
        let Some((_, query)) = self.mention() else {
            return Vec::new();
        };
        crate::mentions::filter(&self.files, &query, 50)
            .into_iter()
            .map(|path| Entry {
                name: path.clone(),
                usage: String::new(),
                description: String::new(),
                origin: popup::Origin::Builtin,
            })
            .collect()
    }

    pub fn popup_matches(&self) -> Vec<Entry> {
        if let Some(q) = popup::query(&self.input) {
            return popup::filter(&self.entries, q).into_iter().cloned().collect();
        }
        if self.cursor < self.len() {
            return Vec::new();
        }
        let Some((key, partial)) = popup::argument_query(&self.input) else {
            return Vec::new();
        };
        let needle = partial.to_lowercase();
        let all = popup::arguments(&key, &self.args);
        let (mut starts, contains): (Vec<Entry>, Vec<Entry>) = all
            .into_iter()
            .filter(|e| e.name.to_lowercase().contains(&needle))
            .partition(|e| e.name.to_lowercase().starts_with(&needle));
        if starts.len() == 1 && contains.is_empty() && starts[0].name == partial {
            return Vec::new();
        }
        starts.extend(contains);
        starts
    }

    pub fn popup_prefix(&self) -> &'static str {
        if popup::query(&self.input).is_some() { "/" } else { "" }
    }

    pub fn usage_hint(&self) -> Option<String> {
        if self.cursor < self.len() || self.popup_visible() && popup::query(&self.input).is_some() {
            return None;
        }
        popup::usage_hint(&self.input, &self.entries)
    }

    fn complete_argument(&mut self, value: &str) {
        let Some((_, partial)) = popup::argument_query(&self.input) else {
            return;
        };
        let keep = self.input.len() - partial.len();
        let line = format!("{}{value} ", &self.input[..keep]);
        self.set_input(line);
    }

    pub fn mention_visible(&self) -> bool {
        matches!(self.mode, Mode::Chat)
            && self.overlay.is_none()
            && self.pending.is_none()
            && self.plan_review.is_none()
            && !self.popup_hidden
            && !self.mention_entries().is_empty()
    }

    fn complete_mention(&mut self, path: &str) {
        let Some((start, _)) = self.mention() else {
            return;
        };
        let chars: Vec<char> = self.input.chars().collect();
        let before: String = chars[..start].iter().collect();
        let after: String = chars[self.cursor.min(chars.len())..].iter().collect();
        self.input = format!("{before}@{path} {}", after.trim_start());
        self.cursor = before.chars().count() + path.chars().count() + 2;
        self.popup_hidden = true;
    }

    fn on_mention_key(&mut self, key: KeyEvent) -> Option<Option<Action>> {
        if !self.mention_visible() {
            return None;
        }
        let shown = self.mention_entries();
        let chosen = shown
            .get(self.popup_selected.min(shown.len().saturating_sub(1)))
            .map(|e| e.name.clone());
        match key.code {
            KeyCode::Up => {
                self.popup_selected = self.popup_selected.saturating_sub(1);
                Some(None)
            }
            KeyCode::Down => {
                self.popup_selected = (self.popup_selected + 1).min(shown.len().saturating_sub(1));
                Some(None)
            }
            KeyCode::Esc => {
                self.popup_hidden = true;
                Some(None)
            }
            KeyCode::Enter
                if self.mention().is_some_and(|(_, q)| Some(&q) == chosen.as_ref()) =>
            {
                None
            }
            KeyCode::Tab | KeyCode::Enter
                if !key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                if let Some(path) = chosen {
                    self.complete_mention(&path);
                }
                Some(None)
            }
            _ => None,
        }
    }

    pub fn popup_visible(&self) -> bool {
        matches!(self.mode, Mode::Chat)
            && self.overlay.is_none()
            && self.pending.is_none()
            && self.plan_review.is_none()
            && !self.popup_hidden
            && !self.popup_matches().is_empty()
    }

    fn input_changed(&mut self) {
        self.popup_selected = 0;
        self.popup_hidden = false;
    }

    pub fn set_input(&mut self, text: impl Into<String>) {
        self.input = text.into();
        self.cursor = self.len();
        self.input_changed();
    }

    pub fn paste(&mut self, text: &str) {
        match &mut self.mode {
            Mode::Panel(panel) => return panel.paste(text),
            Mode::Onboarding(flow) => return flow.paste(text),
            Mode::Chat => {}
        }
        match &mut self.overlay {
            Some(Overlay::Secret { input, .. }) => input.paste(text),
            Some(Overlay::Pick(picker, _)) => picker.filter.paste(text),
            Some(Overlay::Confirm(..)) => {}
            None if self.pending.is_none() => self.paste_into_input(text),
            None => {}
        }
    }

    fn insert_str(&mut self, text: &str) {
        for c in text.chars() {
            self.insert(c);
        }
    }

    fn paste_into_input(&mut self, text: &str) {
        let text = text.replace('\r', "");
        let candidate = text.trim().trim_matches(['"', '\'']);
        if !candidate.contains('\n')
            && crate::mentions::image_mime(candidate).is_some()
            && std::path::Path::new(candidate).is_file()
        {
            let full = std::path::Path::new(candidate)
                .canonicalize()
                .unwrap_or_else(|_| std::path::PathBuf::from(candidate));
            self.paste_count += 1;
            let name = full
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let token = format!("[Image #{} {name}]", self.paste_count);
            self.pastes.push(Paste {
                token: token.clone(),
                text: format!("{IMAGE_MARK}{}]]", full.display()),
            });
            self.insert_str(&token);
            return;
        }
        let lines = text.lines().count();
        if lines > PASTE_LINES || text.chars().count() > PASTE_CHARS {
            self.paste_count += 1;
            let token = format!("[Pasted text #{} +{lines} lines]", self.paste_count);
            self.pastes.push(Paste {
                token: token.clone(),
                text,
            });
            self.insert_str(&token);
            return;
        }
        self.insert_str(&text);
    }

    pub fn expand_pastes(&mut self, text: &str) -> String {
        let mut out = text.to_string();
        for paste in std::mem::take(&mut self.pastes) {
            if let Some(at) = out.find(&paste.token) {
                out.replace_range(at..at + paste.token.len(), &paste.text);
            }
        }
        out
    }

    fn delete_token_before_cursor(&mut self) -> bool {
        let before: String = self.input.chars().take(self.cursor).collect();
        let Some(index) = self.pastes.iter().position(|p| before.ends_with(&p.token)) else {
            return false;
        };
        let token = self.pastes.remove(index).token;
        let count = token.chars().count();
        let start = self.byte_at(self.cursor - count);
        let end = self.byte_at(self.cursor);
        self.input.replace_range(start..end, "");
        self.cursor -= count;
        self.input_changed();
        true
    }

    pub fn enqueue(&mut self, display: String, expanded: String) {
        self.queue.push(expanded);
        self.queue_display.push(display.clone());
        self.blocks.push(Block::Queued(display));
    }

    fn on_overlay_key(&mut self, overlay: Overlay, key: KeyEvent) -> Option<Action> {
        match overlay {
            Overlay::Pick(mut picker, purpose) => match picker.on_key(key) {
                Pick::Cancel => None,
                Pick::Pending => {
                    self.overlay = Some(Overlay::Pick(picker, purpose));
                    None
                }
                Pick::Chosen(value) | Pick::Custom(value) => Some(Action::Picked(purpose, value)),
            },
            Overlay::Confirm(text, purpose) => {
                let yes = match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => Some(true),
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(false),
                    KeyCode::Enter => Some(self.confirm_choice == 0),
                    KeyCode::Left | KeyCode::Up | KeyCode::BackTab => {
                        self.confirm_choice = 0;
                        None
                    }
                    KeyCode::Right | KeyCode::Down | KeyCode::Tab => {
                        self.confirm_choice = 1;
                        None
                    }
                    _ => None,
                };
                match yes {
                    Some(answer) => {
                        self.confirm_choice = 0;
                        if answer {
                            Some(Action::Confirmed(purpose))
                        } else {
                            self.notice("cancelled");
                            None
                        }
                    }
                    None => {
                        self.overlay = Some(Overlay::Confirm(text, purpose));
                        None
                    }
                }
            }
            Overlay::Secret {
                mut input,
                provider,
            } => match input.on_key(key) {
                Edit::Cancel => {
                    self.notice("login cancelled; no key was changed");
                    None
                }
                Edit::Submit => {
                    let value = input.take();
                    if value.is_empty() {
                        self.overlay = Some(Overlay::Secret { input, provider });
                        return None;
                    }
                    Some(Action::Login(provider, Secret::new(value)))
                }
                _ => {
                    self.overlay = Some(Overlay::Secret { input, provider });
                    None
                }
            },
        }
    }

    fn on_popup_key(&mut self, key: KeyEvent) -> Option<Option<Action>> {
        if !self.popup_visible() {
            return None;
        }
        let matches = self.popup_matches();
        let shown = matches.len();
        let chosen_entry = matches.get(self.popup_selected.min(shown.saturating_sub(1))).cloned();
        let chosen = chosen_entry.as_ref().map(|e| e.name.clone());
        let arguments = popup::query(&self.input).is_none();
        let needs_more = chosen_entry.as_ref().is_some_and(|e| popup::requires_arguments(&e.usage));
        match key.code {
            KeyCode::Up => {
                self.popup_selected = self.popup_selected.saturating_sub(1);
                Some(None)
            }
            KeyCode::Down => {
                self.popup_selected = (self.popup_selected + 1).min(shown.saturating_sub(1));
                Some(None)
            }
            KeyCode::Esc => {
                self.popup_hidden = true;
                Some(None)
            }
            KeyCode::Tab if arguments => {
                if let Some(name) = chosen {
                    self.complete_argument(&name);
                }
                Some(None)
            }
            KeyCode::Tab => {
                if let Some(name) = chosen {
                    self.set_input(format!("/{name} "));
                }
                Some(None)
            }
            KeyCode::Enter if !key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => {
                let name = chosen?;
                if arguments {
                    self.complete_argument(&name);
                    if needs_more {
                        return Some(None);
                    }
                    let line = self.input.trim().to_string();
                    return Some(self.submit_line(line));
                }
                if needs_more {
                    self.set_input(format!("/{name} "));
                    return Some(None);
                }
                let line = format!("/{name}");
                self.set_input("");
                Some(self.submit_line(line))
            }
            _ => None,
        }
    }

    fn submit_line(&mut self, text: String) -> Option<Action> {
        self.history.push(text.clone());
        self.history_pos = None;
        self.input.clear();
        self.cursor = 0;
        self.input_changed();
        self.scroll_up = 0;
        if text.starts_with('/') && popup::split(&text).is_some() {
            self.blocks.push(Block::User(text.clone()));
            return Some(Action::Command(text));
        }
        if let Some(command) = text.strip_prefix('!').map(str::trim).filter(|c| !c.is_empty()) {
            self.blocks.push(Block::User(text.clone()));
            return Some(Action::Shell(command.to_string()));
        }
        if let Some(note) = text.strip_prefix('#').map(str::trim).filter(|n| !n.is_empty()) {
            self.blocks.push(Block::User(text.clone()));
            return Some(Action::Remember(note.to_string()));
        }
        let expanded = self.expand_pastes(&text);
        if self.streaming || self.queue_paused {
            self.enqueue(text, expanded);
            return None;
        }
        self.blocks.push(Block::User(text));
        Some(Action::Submit(expanded))
    }

    pub fn next_queued(&mut self) -> Option<String> {
        if self.queue_paused || self.queue.is_empty() {
            return None;
        }
        let text = self.queue.remove(0);
        let display = if self.queue_display.is_empty() {
            text.clone()
        } else {
            self.queue_display.remove(0)
        };
        if let Some(at) = self
            .blocks
            .iter()
            .position(|b| matches!(b, Block::Queued(t) if *t == display))
        {
            self.blocks.remove(at);
        }
        self.blocks.push(Block::User(display));
        Some(text)
    }

    pub fn pause_queue(&mut self) {
        if !self.queue.is_empty() {
            self.queue_paused = true;
            self.notice(format!(
                "{} queued message{} kept: enter sends, esc clears",
                self.queue.len(),
                if self.queue.len() == 1 { "" } else { "s" }
            ));
        }
    }

    pub fn clear_queue(&mut self) {
        let dropped = self.queue.len();
        self.queue.clear();
        self.queue_display.clear();
        self.queue_paused = false;
        self.blocks.retain(|b| !matches!(b, Block::Queued(_)));
        self.notice(format!(
            "dropped {dropped} queued message{}",
            if dropped == 1 { "" } else { "s" }
        ));
    }

    pub fn advance_frame(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    pub fn notice(&mut self, text: impl Into<String>) {
        self.blocks.push(Block::Notice(text.into()));
    }

    pub fn apply(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Provider(Event::TextDelta(text)) => {
                self.status.streamed_chars += text.len();
                match self.blocks.last_mut() {
                    Some(Block::Assistant(existing)) => existing.push_str(&text),
                    _ => self.blocks.push(Block::Assistant(text)),
                }
            }
            AgentEvent::Provider(Event::Usage {
                input,
                output,
                cached,
            }) => {
                self.status.input_tokens += u64::from(input);
                self.status.output_tokens += u64::from(output);
                self.status.cached_tokens += u64::from(cached);
            }
            AgentEvent::Provider(Event::Error(err)) => {
                self.notice(format!("error: {err}"));
            }
            AgentEvent::Provider(_) => {}
            AgentEvent::ToolStarted { id, name, args } => {
                self.blocks.push(Block::Tool(ToolCard {
                    id,
                    name,
                    args,
                    status: CardStatus::Running,
                    ..ToolCard::default()
                }));
            }
            AgentEvent::ToolFinished { id, output, .. } => {
                if let Some(card) = self.card_mut(&id) {
                    card.status = if output.is_error {
                        CardStatus::Failed
                    } else {
                        CardStatus::Done
                    };
                    card.output = Some(output.content);
                }
            }
        }
    }

    pub fn task_update(&mut self, update: zarn_core::subagent::TaskUpdate) {
        use zarn_core::subagent::TaskState;
        if let TaskState::Started { background, .. } = &update.state {
            self.agents.retain(|r| r.id != update.id);
            self.agents.push(AgentRow {
                id: update.id.clone(),
                agent: update.agent.clone(),
                tool: None,
                started: std::time::Instant::now(),
                tokens: 0,
                background: *background,
                done: None,
            });
            return;
        }
        let Some(row) = self.agents.iter_mut().find(|r| r.id == update.id) else {
            return;
        };
        match update.state {
            TaskState::Tool(tool) => row.tool = Some(tool),
            TaskState::Tokens(n) => row.tokens = n,
            TaskState::Finished { ok } => {
                row.done = Some(ok);
                row.tool = None;
                if row.background {
                    let id = row.id.clone();
                    let agent = row.agent.clone();
                    self.agents.retain(|r| r.id != id);
                    self.notice(format!(
                        "background agent {id} ({agent}) {}; its result goes to the model with your next message (/tasks shows it)",
                        if ok { "finished" } else { "failed" }
                    ));
                }
            }
            TaskState::Started { .. } => {}
        }
    }

    pub fn progress(&mut self, command: &str, line: &str) {
        let card = self.blocks.iter_mut().rev().find_map(|b| match b {
            Block::Tool(card)
                if card.name == "bash"
                    && card.status == CardStatus::Running
                    && card.args.get("command").and_then(Value::as_str) == Some(command) =>
            {
                Some(card)
            }
            _ => None,
        });
        if let Some(card) = card {
            card.live.push(line.to_string());
            if card.live.len() > LIVE_LINES {
                card.live.remove(0);
            }
        }
    }

    pub fn set_card_detail(&mut self, id: &str, detail: String) {
        if let Some(card) = self.card_mut(id) {
            card.detail = Some(detail);
        }
    }

    fn card_mut(&mut self, id: &str) -> Option<&mut ToolCard> {
        self.blocks.iter_mut().rev().find_map(|b| match b {
            Block::Tool(card) if card.id == id => Some(card),
            _ => None,
        })
    }

    pub fn expand_last_output(&mut self) {
        if let Some(card) = self.blocks.iter_mut().rev().find_map(|b| match b {
            Block::Tool(card) if card.output.is_some() => Some(card),
            _ => None,
        }) {
            card.expanded = !card.expanded;
        }
    }

    pub fn answer_permission(&mut self, answer: Answer) {
        self.pending_expanded = false;
        self.permission_choice = 0;
        if let Some(request) = self.pending.take() {
            let _ = request.reply.send(answer);
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.kind == KeyEventKind::Release {
            return None;
        }

        if key.code != KeyCode::Char('c') || !key.modifiers.contains(KeyModifiers::CONTROL) {
            self.ctrl_c_armed = false;
        }
        let esc_was_armed = self.esc_armed;
        if key.code != KeyCode::Esc {
            self.esc_armed = false;
        }

        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.ctrl_c_armed {
                return Some(Action::Quit);
            }
            self.ctrl_c_armed = true;
            match &mut self.mode {
                Mode::Panel(_) => return Some(Action::Panel(settings::Effect::Close)),
                Mode::Onboarding(_) => {}
                Mode::Chat => self.notice("press ctrl+c again to exit"),
            }
            return None;
        }

        if self.pending.is_some() {
            return self.on_permission_key(key);
        }

        if self.plan_review.as_ref().is_some_and(|r| r.decided.is_none()) {
            return self.on_plan_key(key);
        }

        match &mut self.mode {
            Mode::Panel(panel) => return panel.on_key(key).map(Action::Panel),
            Mode::Onboarding(flow) => return flow.on_key(key).map(Action::Onboard),
            Mode::Chat => {}
        }

        if self.transcript {
            return self.on_transcript_key(key);
        }

        if self.search.is_some()
            && let Some(action) = self.on_search_key(key)
        {
            return action;
        }

        if let Some(overlay) = self.overlay.take() {
            return self.on_overlay_key(overlay, key);
        }

        if let Some(action) = self.on_popup_key(key) {
            return action;
        }

        if let Some(action) = self.on_mention_key(key) {
            return action;
        }

        if let Some(action) = self.on_vim_key(key) {
            return action;
        }

        if let Some(action) = self.keymap.action(&key) {
            use crate::keymap::KeyAction;
            match action {
                KeyAction::ExpandOutput => self.expand_last_output(),
                KeyAction::HistorySearch => return Some(Action::Command("/history".into())),
                KeyAction::CycleMode => return Some(Action::CycleMode),
                KeyAction::ProfilePicker => return Some(Action::Command("/profile".into())),
                KeyAction::ExternalEditor => return Some(Action::Editor),
                KeyAction::Transcript => self.toggle_transcript(),
                KeyAction::SearchScrollback => self.start_search(),
                KeyAction::ScrollUp => self.scroll_up = self.scroll_up.saturating_add(10),
                KeyAction::ScrollDown => self.scroll_up = self.scroll_up.saturating_sub(10),
                KeyAction::HistoryPrev => self.vim_vertical(false),
                KeyAction::HistoryNext => self.vim_vertical(true),
                KeyAction::Newline => self.insert('\n'),
                KeyAction::ExpandThinking => {}
            }
            return None;
        }

        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                if self.streaming {
                    return Some(Action::Cancel);
                }
                if !self.queue.is_empty() {
                    self.clear_queue();
                    return None;
                }
                if self.input.is_empty() {
                    if esc_was_armed {
                        self.esc_armed = false;
                        return Some(Action::Command("/rewind".into()));
                    }
                    self.esc_armed = true;
                }
            }
            (KeyCode::Enter, _) => {
                let text = self.input.trim().to_string();
                if !text.is_empty() {
                    return self.submit_line(text);
                }
                if self.queue_paused && !self.streaming {
                    self.queue_paused = false;
                    return Some(Action::SendQueued);
                }
            }
            (KeyCode::Backspace, _) => {
                if self.cursor > 0 && !self.delete_token_before_cursor() {
                    let at = self.byte_at(self.cursor - 1);
                    self.input.remove(at);
                    self.cursor -= 1;
                    self.input_changed();
                }
            }
            (KeyCode::Delete, _) => {
                if self.cursor < self.len() {
                    let at = self.byte_at(self.cursor);
                    self.input.remove(at);
                    self.input_changed();
                }
            }
            (KeyCode::Left, _) => self.cursor = self.cursor.saturating_sub(1),
            (KeyCode::Right, _) => self.cursor = (self.cursor + 1).min(self.len()),
            (KeyCode::Home, _) => self.cursor = 0,
            (KeyCode::End, _) => self.cursor = self.len(),
            (KeyCode::Char(c), m) if !m.contains(KeyModifiers::CONTROL) => self.insert(c),
            _ => {}
        }

        None
    }

    fn line_bounds(&self) -> (usize, usize) {
        let chars: Vec<char> = self.input.chars().collect();
        let cursor = self.cursor.min(chars.len());
        let start = chars[..cursor].iter().rposition(|c| *c == '\n').map(|i| i + 1).unwrap_or(0);
        let end = chars[cursor..]
            .iter()
            .position(|c| *c == '\n')
            .map(|i| cursor + i)
            .unwrap_or(chars.len());
        (start, end)
    }

    fn vim_vertical(&mut self, down: bool) {
        let chars: Vec<char> = self.input.chars().collect();
        let (start, end) = self.line_bounds();
        let column = self.cursor - start;
        if down {
            if end >= chars.len() {
                return self.recall(1);
            }
            let next_start = end + 1;
            let next_end = chars[next_start..]
                .iter()
                .position(|c| *c == '\n')
                .map(|i| next_start + i)
                .unwrap_or(chars.len());
            self.cursor = (next_start + column).min(next_end);
        } else {
            if start == 0 {
                return self.recall(-1);
            }
            let prev_end = start - 1;
            let prev_start = chars[..prev_end].iter().rposition(|c| *c == '\n').map(|i| i + 1).unwrap_or(0);
            self.cursor = (prev_start + column).min(prev_end);
        }
    }

    fn vim_word(&mut self, forward: bool) {
        let chars: Vec<char> = self.input.chars().collect();
        let mut i = self.cursor.min(chars.len());
        if forward {
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
        } else {
            while i > 0 && chars[i - 1].is_whitespace() {
                i -= 1;
            }
            while i > 0 && !chars[i - 1].is_whitespace() {
                i -= 1;
            }
        }
        self.cursor = i;
    }

    fn on_vim_key(&mut self, key: KeyEvent) -> Option<Option<Action>> {
        let mut vim = self.vim?;
        if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return None;
        }
        if !vim.normal {
            if key.code == KeyCode::Esc && !self.streaming && self.queue.is_empty() {
                vim.normal = true;
                self.cursor = self.cursor.saturating_sub(1).max(self.line_bounds().0);
                self.vim = Some(vim);
                return Some(None);
            }
            return None;
        }
        let pending = std::mem::take(&mut vim.pending_d);
        let len = self.len();
        match key.code {
            KeyCode::Char('h') | KeyCode::Left => self.cursor = self.cursor.saturating_sub(1).max(self.line_bounds().0),
            KeyCode::Char('l') | KeyCode::Right => {
                let (_, end) = self.line_bounds();
                self.cursor = (self.cursor + 1).min(end.saturating_sub(1).max(self.line_bounds().0));
            }
            KeyCode::Char('j') | KeyCode::Down => self.vim_vertical(true),
            KeyCode::Char('k') | KeyCode::Up => self.vim_vertical(false),
            KeyCode::Char('w') => self.vim_word(true),
            KeyCode::Char('b') => self.vim_word(false),
            KeyCode::Char('0') | KeyCode::Home => self.cursor = self.line_bounds().0,
            KeyCode::Char('$') | KeyCode::End => self.cursor = self.line_bounds().1.saturating_sub(1).max(self.line_bounds().0),
            KeyCode::Char('x') | KeyCode::Delete => {
                if self.cursor < len && self.input.chars().nth(self.cursor) != Some('\n') {
                    let at = self.byte_at(self.cursor);
                    self.input.remove(at);
                    self.input_changed();
                }
            }
            KeyCode::Char('d') if pending => {
                let (start, end) = self.line_bounds();
                let chars: Vec<char> = self.input.chars().collect();
                let (from, to) = if end < chars.len() {
                    (start, end + 1)
                } else if start > 0 {
                    (start - 1, end)
                } else {
                    (start, end)
                };
                self.input = chars[..from].iter().chain(chars[to..].iter()).collect();
                self.cursor = from.min(self.len());
                self.input_changed();
            }
            KeyCode::Char('d') => vim.pending_d = true,
            KeyCode::Char('i') => vim.normal = false,
            KeyCode::Char('a') => {
                vim.normal = false;
                self.cursor = (self.cursor + 1).min(self.line_bounds().1);
            }
            KeyCode::Char('o') => {
                vim.normal = false;
                self.cursor = self.line_bounds().1;
                self.insert('\n');
            }
            KeyCode::Enter => {
                self.vim = Some(Vim { normal: false, pending_d: false });
                let text = self.input.trim().to_string();
                return Some(if text.is_empty() { None } else { self.submit_line(text) });
            }
            KeyCode::Esc => {
                self.vim = Some(vim);
                return None;
            }
            _ => {}
        }
        self.vim = Some(vim);
        Some(None)
    }

    pub fn toggle_transcript(&mut self) {
        self.transcript = !self.transcript;
        if self.transcript {
            self.transcript_scroll = 0;
        }
    }

    pub fn start_search(&mut self) {
        self.search = Some(Search {
            typing: true,
            ..Search::default()
        });
    }

    pub fn scroll_by(&mut self, up: i32) {
        let target = if self.transcript {
            &mut self.transcript_scroll
        } else {
            &mut self.scroll_up
        };
        *target = if up >= 0 {
            target.saturating_add(up as u16)
        } else {
            target.saturating_sub(up.unsigned_abs() as u16)
        };
    }

    fn wheel_moves_selection(&self) -> bool {
        match &self.mode {
            Mode::Panel(_) => true,
            Mode::Onboarding(flow) => matches!(flow.step, crate::onboarding::Step::Kind(_) | crate::onboarding::Step::Model(_)),
            Mode::Chat => {
                matches!(self.overlay, Some(Overlay::Pick(..)))
                    || self.popup_visible()
                    || self.mention_visible()
            }
        }
    }

    fn picker_mut(&mut self) -> Option<&mut crate::widgets::Picker> {
        match &mut self.mode {
            Mode::Panel(panel) => panel.picker_mut(),
            Mode::Onboarding(flow) => match &mut flow.step {
                crate::onboarding::Step::Kind(picker) | crate::onboarding::Step::Model(picker) => Some(picker),
                _ => None,
            },
            Mode::Chat => match &mut self.overlay {
                Some(Overlay::Pick(picker, _)) => Some(picker),
                _ => None,
            },
        }
    }

    pub fn on_mouse(&mut self, event: crossterm::event::MouseEvent) -> Option<Action> {
        use crate::hits::Hit;
        use crossterm::event::{MouseButton, MouseEventKind};
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        match event.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = event.kind == MouseEventKind::ScrollUp;
                if self.wheel_moves_selection() {
                    return self.on_key(KeyEvent::new(
                        if up { KeyCode::Up } else { KeyCode::Down },
                        KeyModifiers::NONE,
                    ));
                }
                self.scroll_by(if up { 3 } else { -3 });
                None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                match self.hits.at(event.column, event.row)? {
                    Hit::Popup(i) => {
                        if !self.popup_visible() && !self.mention_visible() {
                            return None;
                        }
                        self.popup_selected = i;
                        self.on_key(enter)
                    }
                    Hit::Picker(i) => {
                        let picker = self.picker_mut()?;
                        picker.selected = i;
                        self.on_key(enter)
                    }
                    Hit::Permission(i) => {
                        self.pending.as_ref()?;
                        self.permission_choice = i;
                        self.on_key(enter)
                    }
                    Hit::Plan(i) => {
                        if self.plan_review.as_ref().is_none_or(|r| r.decided.is_some() || r.feedback.is_some()) {
                            return None;
                        }
                        self.plan_choice = i;
                        self.on_key(enter)
                    }
                    Hit::Confirm(i) => {
                        if !matches!(self.overlay, Some(Overlay::Confirm(..))) {
                            return None;
                        }
                        self.confirm_choice = i;
                        self.on_key(enter)
                    }
                    Hit::Tab(i) => {
                        if let Mode::Panel(panel) = &mut self.mode {
                            panel.select_tab(i);
                        }
                        None
                    }
                    Hit::Row(i) => match &mut self.mode {
                        Mode::Panel(panel) => panel.click_row(i).map(Action::Panel),
                        _ => None,
                    },
                }
            }
            _ => None,
        }
    }

    fn on_transcript_key(&mut self, key: KeyEvent) -> Option<Action> {
        if self.keymap.action(&key) == Some(crate::keymap::KeyAction::Transcript) {
            self.transcript = false;
            return None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.transcript = false,
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(-1),
            KeyCode::PageUp => self.scroll_by(10),
            KeyCode::PageDown => self.scroll_by(-10),
            KeyCode::Home | KeyCode::Char('g') => self.transcript_scroll = u16::MAX,
            KeyCode::End | KeyCode::Char('G') => self.transcript_scroll = 0,
            _ => {}
        }
        None
    }

    fn on_search_key(&mut self, key: KeyEvent) -> Option<Option<Action>> {
        let search = self.search.as_mut()?;
        if search.typing {
            match key.code {
                KeyCode::Esc => self.search = None,
                KeyCode::Enter => {
                    if search.query.is_empty() {
                        self.search = None;
                    } else {
                        search.typing = false;
                        search.hit = 0;
                    }
                }
                KeyCode::Backspace => {
                    search.query.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    search.query.push(c);
                    search.hit = 0;
                }
                _ => {}
            }
            return Some(None);
        }
        match key.code {
            KeyCode::Char('n') if self.input.is_empty() => search.hit = search.hit.saturating_add(1),
            KeyCode::Char('N') if self.input.is_empty() => search.hit = search.hit.saturating_sub(1),
            KeyCode::Esc => self.search = None,
            _ if self.keymap.action(&key) == Some(crate::keymap::KeyAction::SearchScrollback) => {
                self.start_search();
            }
            _ => {
                self.search = None;
                return None;
            }
        }
        Some(None)
    }

    fn on_plan_key(&mut self, key: KeyEvent) -> Option<Action> {
        let review = self.plan_review.as_mut()?;
        if let Some(field) = review.feedback.as_mut() {
            match key.code {
                KeyCode::Esc => review.feedback = None,
                KeyCode::Enter => {
                    let text = field.value.trim().to_string();
                    let verdict = PlanVerdict::Reject(if text.is_empty() {
                        "no feedback given".into()
                    } else {
                        text
                    });
                    review.decided = Some(verdict.clone());
                    return Some(Action::Plan(verdict));
                }
                _ => {
                    field.on_key(key);
                }
            }
            return None;
        }
        let picked = match key.code {
            KeyCode::Up => {
                self.plan_choice = self.plan_choice.saturating_sub(1);
                return None;
            }
            KeyCode::Down => {
                self.plan_choice = (self.plan_choice + 1).min(crate::views::PLAN_OPTIONS.len() - 1);
                return None;
            }
            KeyCode::Enter => Some(self.plan_choice),
            KeyCode::Char(c @ '1'..='3') => Some(c as usize - '1' as usize),
            _ => None,
        };
        let key = match picked {
            Some(0) => KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            Some(1) => KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            Some(_) => KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
            None => key,
        };
        let review = self.plan_review.as_mut()?;
        let verdict = match key.code {
            KeyCode::Char('a') | KeyCode::Char('A') => PlanVerdict::Approve("accept-edits".into()),
            KeyCode::Char('d') | KeyCode::Char('D') => PlanVerdict::Approve("default".into()),
            KeyCode::Char('r') | KeyCode::Char('R') => {
                review.feedback = Some(crate::widgets::TextField::new(""));
                return None;
            }
            KeyCode::Esc => PlanVerdict::Reject("the user dismissed the plan without feedback".into()),
            KeyCode::PageUp => {
                self.scroll_up = self.scroll_up.saturating_add(10);
                return None;
            }
            KeyCode::PageDown => {
                self.scroll_up = self.scroll_up.saturating_sub(10);
                return None;
            }
            _ => return None,
        };
        review.decided = Some(verdict.clone());
        self.plan_choice = 0;
        Some(Action::Plan(verdict))
    }

    fn on_permission_key(&mut self, key: KeyEvent) -> Option<Action> {
        const LETTERS: [char; 6] = ['y', 'a', 'p', 'l', 'u', 'n'];
        let picked = match key.code {
            KeyCode::Up => {
                self.permission_choice = self.permission_choice.saturating_sub(1);
                return None;
            }
            KeyCode::Down | KeyCode::Tab => {
                self.permission_choice = (self.permission_choice + 1).min(LETTERS.len() - 1);
                return None;
            }
            KeyCode::Enter => Some(self.permission_choice),
            KeyCode::Char(c @ '1'..='6') => Some(c as usize - '1' as usize),
            _ => None,
        };
        let key = match picked {
            Some(i) => KeyEvent::new(KeyCode::Char(LETTERS[i.min(LETTERS.len() - 1)]), KeyModifiers::NONE),
            None => key,
        };
        let pattern = self.pending.as_ref().map(|p| p.pattern.clone()).unwrap_or_default();
        let save = |scope| Some(Action::SaveRule(pattern.clone(), scope));
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => self.answer_permission(Answer::Once),
            KeyCode::Char('a') | KeyCode::Char('A') => {
                self.answer_permission(Answer::Pattern(pattern.clone()))
            }
            KeyCode::Char('p') | KeyCode::Char('P') => {
                self.answer_permission(Answer::Pattern(pattern.clone()));
                return save(zarn_config::layers::Scope::Project);
            }
            KeyCode::Char('l') | KeyCode::Char('L') => {
                self.answer_permission(Answer::Pattern(pattern.clone()));
                return save(zarn_config::layers::Scope::Local);
            }
            KeyCode::Char('u') | KeyCode::Char('U') => {
                self.answer_permission(Answer::Pattern(pattern.clone()));
                return save(zarn_config::layers::Scope::User);
            }
            KeyCode::Char('e') | KeyCode::Char('E') => self.pending_expanded = !self.pending_expanded,
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.pending_expanded = !self.pending_expanded
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.answer_permission(Answer::No)
            }
            _ => {}
        }
        None
    }

    fn len(&self) -> usize {
        self.input.chars().count()
    }

    fn byte_at(&self, index: usize) -> usize {
        self.input
            .char_indices()
            .nth(index)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len())
    }

    fn insert(&mut self, c: char) {
        let at = self.byte_at(self.cursor);
        self.input.insert(at, c);
        self.cursor += 1;
        self.input_changed();
    }

    fn recall(&mut self, delta: i32) {
        if self.history.is_empty() {
            return;
        }
        let last = self.history.len() - 1;
        let next = match (self.history_pos, delta) {
            (None, -1) => Some(last),
            (None, _) => None,
            (Some(pos), -1) => Some(pos.saturating_sub(1)),
            (Some(pos), _) if pos >= last => None,
            (Some(pos), _) => Some(pos + 1),
        };

        self.history_pos = next;
        self.input = next.map(|i| self.history[i].clone()).unwrap_or_default();
        self.cursor = self.len();
        self.popup_hidden = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zarn_core::tools::ToolOutput;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn enter_submits_and_shift_enter_does_not() {
        let mut app = App::new("m");
        app.on_key(key(KeyCode::Char('h')));
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)),
            None
        );
        assert_eq!(app.input, "h\n");
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Action::Submit("h".into()))
        );
        assert!(app.input.is_empty());
    }

    fn typed(app: &mut App, text: &str) -> Option<Action> {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter))
    }

    #[test]
    fn input_while_streaming_is_queued_and_shown_as_queued() {
        let mut app = App::new("m");
        app.streaming = true;
        assert_eq!(typed(&mut app, "first"), None);
        assert_eq!(typed(&mut app, "second"), None);
        assert_eq!(app.queue, vec!["first", "second"]);
        assert!(matches!(app.blocks.last(), Some(Block::Queued(t)) if t == "second"));
        app.streaming = false;
        assert_eq!(app.next_queued().as_deref(), Some("first"));
        assert!(matches!(app.blocks.last(), Some(Block::User(t)) if t == "first"));
        assert_eq!(app.next_queued().as_deref(), Some("second"));
        assert_eq!(app.next_queued(), None);
    }

    #[test]
    fn esc_cancels_and_keeps_the_queue_then_a_second_esc_clears_it() {
        let mut app = App::new("m");
        app.streaming = true;
        typed(&mut app, "later");
        assert_eq!(app.on_key(key(KeyCode::Esc)), Some(Action::Cancel));
        assert_eq!(app.queue.len(), 1);
        app.streaming = false;
        app.pause_queue();
        assert_eq!(app.next_queued(), None);
        app.on_key(key(KeyCode::Esc));
        assert!(app.queue.is_empty());
        assert!(!app.blocks.iter().any(|b| matches!(b, Block::Queued(_))));
    }

    #[test]
    fn enter_on_an_empty_line_resumes_a_paused_queue() {
        let mut app = App::new("m");
        app.streaming = true;
        typed(&mut app, "later");
        app.streaming = false;
        app.pause_queue();
        assert_eq!(app.on_key(key(KeyCode::Enter)), Some(Action::SendQueued));
        assert_eq!(app.next_queued().as_deref(), Some("later"));
    }

    #[test]
    fn commands_still_run_while_streaming() {
        let mut app = App::new("m");
        app.streaming = true;
        assert_eq!(typed(&mut app, "/cost"), Some(Action::Command("/cost".into())));
        assert!(app.queue.is_empty());
    }

    #[test]
    fn ctrl_p_opens_the_profile_picker() {
        let mut app = App::new("m");
        assert_eq!(app.on_key(ctrl('p')), Some(Action::Command("/profile".into())));
    }

    #[test]
    fn saving_a_pattern_answers_the_call_and_asks_to_persist_it() {
        let (tx, rx) = oneshot::channel();
        let mut app = App::new("m");
        let mut request = PermissionRequest::new("edit", Risk::Write, Value::Null, tx);
        request.pattern = "edit(src/a.rs)".into();
        app.pending = Some(request);
        assert_eq!(
            app.on_key(key(KeyCode::Char('p'))),
            Some(Action::SaveRule("edit(src/a.rs)".into(), zarn_config::layers::Scope::Project))
        );
        assert_eq!(rx.blocking_recv().unwrap(), Answer::Pattern("edit(src/a.rs)".into()));
    }

    #[test]
    fn esc_esc_on_an_empty_line_asks_to_rewind() {
        let mut app = App::new("m");
        assert_eq!(app.on_key(key(KeyCode::Esc)), None);
        assert_eq!(app.on_key(key(KeyCode::Esc)), Some(Action::Command("/rewind".into())));
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::Char('x')));
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.on_key(key(KeyCode::Esc)), None);
    }

    #[test]
    fn typing_at_lists_files_and_tab_inserts_the_chosen_path() {
        let mut app = App::new("m");
        app.files = vec!["src/main.rs".into(), "src/mentions.rs".into(), "README.md".into()];
        for c in "look at @ment".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert!(app.mention_visible());
        assert_eq!(app.mention_entries()[0].name, "src/mentions.rs");
        assert_eq!(app.on_key(key(KeyCode::Tab)), None);
        assert_eq!(app.input, "look at @src/mentions.rs ");
        assert!(!app.mention_visible());
        for c in "please".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Action::Submit("look at @src/mentions.rs please".into()))
        );
    }

    #[test]
    fn a_large_paste_is_collapsed_into_a_token_and_expanded_when_sent() {
        let mut app = App::new("m");
        let big: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        app.paste("before ");
        app.paste(&big);
        assert_eq!(app.input, "before [Pasted text #1 +30 lines]");
        app.paste(" after");
        let sent = app.on_key(key(KeyCode::Enter));
        let Some(Action::Submit(text)) = sent else { panic!("{sent:?}") };
        assert!(text.starts_with("before line 1\n"));
        assert!(text.ends_with("line 30\n after"));
        assert!(matches!(app.blocks.last(), Some(Block::User(t)) if t == "before [Pasted text #1 +30 lines] after"));
    }

    #[test]
    fn a_long_single_line_paste_also_collapses_and_small_ones_do_not() {
        let mut app = App::new("m");
        app.paste(&"x".repeat(2_500));
        assert_eq!(app.input, "[Pasted text #1 +1 lines]");
        let mut small = App::new("m");
        small.paste("short text");
        assert_eq!(small.input, "short text");
    }

    #[test]
    fn backspace_on_a_token_removes_the_whole_paste() {
        let mut app = App::new("m");
        app.paste("a ");
        app.paste(&"y\n".repeat(25));
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.input, "a ");
        assert!(app.pastes.is_empty());
        app.on_key(key(KeyCode::Char('b')));
        assert_eq!(app.on_key(key(KeyCode::Enter)), Some(Action::Submit("a b".into())));
    }

    #[test]
    fn a_queued_paste_shows_collapsed_and_sends_expanded() {
        let mut app = App::new("m");
        app.streaming = true;
        app.paste(&"z\n".repeat(21));
        app.on_key(key(KeyCode::Enter));
        assert!(matches!(app.blocks.last(), Some(Block::Queued(t)) if t == "[Pasted text #1 +21 lines]"));
        app.streaming = false;
        assert_eq!(app.next_queued().unwrap(), "z\n".repeat(21));
        assert!(matches!(app.blocks.last(), Some(Block::User(t)) if t == "[Pasted text #1 +21 lines]"));
    }

    #[test]
    fn pasting_an_image_path_attaches_it_as_a_token() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("shot.png");
        std::fs::write(&image, [137u8, 80, 78, 71]).unwrap();
        let mut app = App::new("m");
        app.paste(&format!("'{}'", image.display()));
        assert_eq!(app.input, "[Image #1 shot.png]");
        let Some(Action::Submit(text)) = app.on_key(key(KeyCode::Enter)) else { panic!() };
        assert!(text.starts_with(IMAGE_MARK));
        assert!(text.contains("shot.png]]"));
    }

    #[test]
    fn bang_and_hash_lines_become_shell_and_memory_actions() {
        let mut app = App::new("m");
        app.set_input("!git status");
        assert_eq!(app.on_key(key(KeyCode::Enter)), Some(Action::Shell("git status".into())));
        app.set_input("# always run cargo fmt");
        assert_eq!(app.on_key(key(KeyCode::Enter)), Some(Action::Remember("always run cargo fmt".into())));
        app.set_input("#");
        assert_eq!(app.on_key(key(KeyCode::Enter)), Some(Action::Submit("#".into())));
    }

    #[test]
    fn enter_sends_when_the_mention_is_already_complete() {
        let mut app = App::new("m");
        app.files = vec!["shot.png".into()];
        app.set_input("see @shot.png");
        assert_eq!(app.on_key(key(KeyCode::Enter)), Some(Action::Submit("see @shot.png".into())));
    }

    #[test]
    fn rebound_keys_follow_the_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.toml");
        std::fs::write(&path, "expand_output = \"ctrl+e\"\nhistory_search = \"ctrl+y\"\n").unwrap();
        let mut app = App::new("m");
        app.keymap = crate::keymap::Keymap::load_from(&path);
        assert_eq!(app.on_key(ctrl('y')), Some(Action::Command("/history".into())));
        assert_eq!(app.on_key(ctrl('f')), None);
        assert_eq!(app.on_key(ctrl('g')), Some(Action::Editor));
    }

    #[test]
    fn vim_mode_supports_the_basic_motions_and_edits() {
        let mut app = App::new("m");
        app.vim = Some(Vim::default());
        for c in "one two three".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Esc));
        assert!(app.vim.unwrap().normal);
        for c in ['0', 'w', 'x'] {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.input, "one wo three");
        app.on_key(key(KeyCode::Char('$')));
        app.on_key(key(KeyCode::Char('a')));
        app.on_key(key(KeyCode::Char('!')));
        assert_eq!(app.input, "one wo three!");
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::Char('o')));
        for c in "second".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.input, "one wo three!\nsecond");
        app.on_key(key(KeyCode::Char('k')));
        app.on_key(key(KeyCode::Char('d')));
        app.on_key(key(KeyCode::Char('d')));
        assert_eq!(app.input, "second");
        app.on_key(key(KeyCode::Char('0')));
        app.on_key(key(KeyCode::Char('l')));
        app.on_key(key(KeyCode::Char('h')));
        app.on_key(key(KeyCode::Char('i')));
        app.on_key(key(KeyCode::Char('>')));
        assert_eq!(app.input, ">second");
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::Char('b')));
        assert_eq!(app.on_key(key(KeyCode::Enter)), Some(Action::Submit(">second".into())));
    }

    #[test]
    fn without_vim_mode_letters_are_just_typed() {
        let mut app = App::new("m");
        app.on_key(key(KeyCode::Esc));
        app.on_key(key(KeyCode::Char('x')));
        assert_eq!(app.input, "x");
    }

    #[test]
    fn shift_tab_asks_to_cycle_the_mode() {
        let mut app = App::new("m");
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
            Some(Action::CycleMode)
        );
    }

    #[test]
    fn a_plan_review_takes_approval_or_typed_feedback() {
        let (tx, _rx) = oneshot::channel();
        let mut app = App::new("m");
        app.plan_review = Some(PlanReview::new("1. do it", tx));
        app.on_key(key(KeyCode::Char('x')));
        assert!(app.input.is_empty());
        assert_eq!(
            app.on_key(key(KeyCode::Char('a'))),
            Some(Action::Plan(PlanVerdict::Approve("accept-edits".into())))
        );

        let (tx, _rx) = oneshot::channel();
        app.plan_review = Some(PlanReview::new("1. do it", tx));
        app.on_key(key(KeyCode::Char('r')));
        for c in "add tests".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Action::Plan(PlanVerdict::Reject("add tests".into())))
        );
    }

    #[test]
    fn the_plan_review_and_confirm_dialogs_take_arrows_and_enter() {
        let (tx, _rx) = oneshot::channel();
        let mut app = App::new("m");
        app.plan_review = Some(PlanReview::new("1. do it", tx));
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.plan_choice, 1);
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Action::Plan(PlanVerdict::Approve("default".into())))
        );
        assert_eq!(app.plan_choice, 0);

        let (tx, _rx) = oneshot::channel();
        app.plan_review = Some(PlanReview::new("1. do it", tx));
        app.on_key(key(KeyCode::Char('3')));
        assert!(app.plan_review.as_ref().unwrap().feedback.is_some());

        let mut app = App::new("m");
        app.overlay = Some(Overlay::Confirm("sure?".into(), ConfirmFor::Bypass));
        app.on_key(key(KeyCode::Right));
        assert_eq!(app.confirm_choice, 1);
        assert_eq!(app.on_key(key(KeyCode::Enter)), None);
        assert!(app.overlay.is_none());
        app.overlay = Some(Overlay::Confirm("sure?".into(), ConfirmFor::Bypass));
        assert_eq!(app.confirm_choice, 0);
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            Some(Action::Confirmed(ConfirmFor::Bypass))
        );
    }

    #[test]
    fn up_and_down_move_between_lines_before_walking_the_history() {
        let mut app = App::new("m");
        app.history = vec!["older".into()];
        app.set_input("one\ntwo");
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.input, "one\ntwo");
        assert_eq!(app.cursor, 3);
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.input, "older");
    }

    #[test]
    fn arguments_complete_from_a_list_and_required_ones_wait_for_more() {
        let mut app = App::new("m");
        app.args.insert("mode".into(), vec![("default".into(), "".into()), ("plan".into(), "read only".into())]);
        app.set_input("/mode p");
        let names: Vec<String> = app.popup_matches().into_iter().map(|e| e.name).collect();
        assert_eq!(names, vec!["plan"]);
        assert_eq!(app.popup_prefix(), "");
        assert_eq!(app.on_key(key(KeyCode::Enter)), Some(Action::Command("/mode plan".into())));

        app.set_input("/limits ");
        assert_eq!(app.usage_hint().as_deref(), Some("[session|day <usd|off>]"));
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.input, "/limits session ");
        assert_eq!(app.usage_hint().as_deref(), Some("<usd|off>"));

        app.set_input("/limits d");
        assert_eq!(app.on_key(key(KeyCode::Enter)), None);
        assert_eq!(app.input, "/limits day ");

        app.set_input("/ren");
        assert_eq!(app.on_key(key(KeyCode::Enter)), None);
        assert_eq!(app.input, "/rename ");
        assert_eq!(app.usage_hint().as_deref(), Some("<title>"));

        app.set_input("/mode plan");
        assert!(app.popup_matches().is_empty(), "a finished argument closes the list");

        app.set_input("/loo");
        assert_eq!(app.on_key(key(KeyCode::Enter)), Some(Action::Command("/loop".into())), "alternatives mean no argument is required");
    }

    #[test]
    fn enter_on_blank_input_does_nothing() {
        let mut app = App::new("m");
        assert_eq!(app.on_key(key(KeyCode::Enter)), None);
        assert!(app.blocks.is_empty());
    }

    #[test]
    fn ctrl_c_needs_two_presses_and_resets_in_between() {
        let mut app = App::new("m");
        assert_eq!(app.on_key(ctrl('c')), None);
        app.on_key(key(KeyCode::Char('x')));
        assert_eq!(app.on_key(ctrl('c')), None);
        assert_eq!(app.on_key(ctrl('c')), Some(Action::Quit));
    }

    #[test]
    fn esc_cancels_only_while_streaming() {
        let mut app = App::new("m");
        assert_eq!(app.on_key(key(KeyCode::Esc)), None);
        app.streaming = true;
        assert_eq!(app.on_key(key(KeyCode::Esc)), Some(Action::Cancel));
    }

    #[test]
    fn history_walks_back_and_returns_to_a_blank_line() {
        let mut app = App::new("m");
        app.history = vec!["one".into(), "two".into()];
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.input, "two");
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.input, "one");
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.input, "two");
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.input, "");
    }

    #[test]
    fn tool_events_drive_the_card_status() {
        let mut app = App::new("m");
        app.apply(AgentEvent::ToolStarted {
            id: "t1".into(),
            name: "read".into(),
            args: Value::Null,
        });
        app.apply(AgentEvent::ToolFinished {
            id: "t1".into(),
            name: "read".into(),
            output: ToolOutput::error("boom"),
        });
        match app.blocks.last().unwrap() {
            Block::Tool(card) => {
                assert_eq!(card.status, CardStatus::Failed);
                assert_eq!(card.output.as_deref(), Some("boom"));
            }
            other => panic!("expected a tool card, got {other:?}"),
        }
    }

    #[test]
    fn text_deltas_append_to_one_assistant_block() {
        let mut app = App::new("m");
        app.apply(AgentEvent::Provider(Event::TextDelta("he".into())));
        app.apply(AgentEvent::Provider(Event::TextDelta("llo".into())));
        assert_eq!(app.blocks.len(), 1);
        match app.blocks.last().unwrap() {
            Block::Assistant(text) => assert_eq!(text, "hello"),
            other => panic!("expected assistant text, got {other:?}"),
        }
    }

    #[test]
    fn a_permission_prompt_swallows_ordinary_typing() {
        let (tx, rx) = oneshot::channel();
        let mut app = App::new("m");
        let mut request = PermissionRequest::new("bash", Risk::Execute, Value::Null, tx);
        request.pattern = "bash(cargo test:*)".into();
        app.pending = Some(request);
        app.on_key(key(KeyCode::Char('x')));
        assert!(app.input.is_empty());
        app.on_key(key(KeyCode::Char('a')));
        assert!(app.pending.is_none());
        assert_eq!(rx.blocking_recv().unwrap(), Answer::Pattern("bash(cargo test:*)".into()));
    }
}
