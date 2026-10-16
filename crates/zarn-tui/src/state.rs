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
use zarn_tools::permissions::Answer;

pub const COLLAPSED_LINES: usize = 3;

#[derive(Debug)]
pub struct PermissionRequest {
    pub tool: String,
    pub risk: Risk,
    pub args: Value,
    pub reply: oneshot::Sender<Answer>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardStatus {
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

#[derive(Debug)]
pub struct ToolCard {
    pub id: String,
    pub name: String,
    pub args: Value,
    pub status: CardStatus,
    pub output: Option<String>,
    pub expanded: bool,
}

#[derive(Debug)]
pub enum Block {
    User(String),
    Assistant(String),
    Tool(ToolCard),
    Notice(String),
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cost: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatPick {
    Model,
    Theme,
    SwitchProvider,
    Resume,
    Login,
    Logout,
}

#[derive(Debug)]
pub enum Overlay {
    Pick(Picker, ChatPick),
    Secret {
        input: MaskedInput,
        provider: String,
    },
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
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub accent: Color,
    pub banner: bool,
    pub show_cost: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            accent: brand::ACCENT,
            banner: true,
            show_cost: true,
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
    pub popup_selected: usize,
    pub popup_hidden: bool,
    pub theme: Theme,
    pub not_ready: Option<String>,
    ctrl_c_armed: bool,
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

    pub fn popup_matches(&self) -> Vec<&Entry> {
        match popup::query(&self.input) {
            Some(q) => popup::filter(&self.entries, q),
            None => Vec::new(),
        }
    }

    pub fn popup_visible(&self) -> bool {
        matches!(self.mode, Mode::Chat)
            && self.overlay.is_none()
            && self.pending.is_none()
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
            None if self.pending.is_none() => {
                for c in text.chars().filter(|c| *c != '\r') {
                    self.insert(c);
                }
            }
            None => {}
        }
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
        let shown = self.popup_matches().len();
        let chosen = self
            .popup_matches()
            .get(self.popup_selected.min(shown.saturating_sub(1)))
            .map(|e| e.name.clone());
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
            KeyCode::Tab => {
                if let Some(name) = chosen {
                    self.set_input(format!("/{name} "));
                    self.popup_hidden = true;
                }
                Some(None)
            }
            KeyCode::Enter if !key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => {
                let name = chosen?;
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
        self.blocks.push(Block::User(text.clone()));
        self.scroll_up = 0;
        if text.starts_with('/') && popup::split(&text).is_some() {
            Some(Action::Command(text))
        } else {
            Some(Action::Submit(text))
        }
    }

    pub fn advance_frame(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    pub fn notice(&mut self, text: impl Into<String>) {
        self.blocks.push(Block::Notice(text.into()));
    }

    pub fn apply(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Provider(Event::TextDelta(text)) => match self.blocks.last_mut() {
                Some(Block::Assistant(existing)) => existing.push_str(&text),
                _ => self.blocks.push(Block::Assistant(text)),
            },
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
                    output: None,
                    expanded: false,
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

        match &mut self.mode {
            Mode::Panel(panel) => return panel.on_key(key).map(Action::Panel),
            Mode::Onboarding(flow) => return flow.on_key(key).map(Action::Onboard),
            Mode::Chat => {}
        }

        if let Some(overlay) = self.overlay.take() {
            return self.on_overlay_key(overlay, key);
        }

        if let Some(action) = self.on_popup_key(key) {
            return action;
        }

        match (key.code, key.modifiers) {
            (KeyCode::Char('r'), m) if m.contains(KeyModifiers::CONTROL) => {
                self.expand_last_output()
            }
            (KeyCode::Esc, _) => {
                if self.streaming {
                    return Some(Action::Cancel);
                }
            }
            (KeyCode::Enter, m)
                if m.contains(KeyModifiers::SHIFT) || m.contains(KeyModifiers::ALT) =>
            {
                self.insert('\n')
            }
            (KeyCode::Enter, _) => {
                let text = self.input.trim().to_string();
                if !text.is_empty() {
                    return self.submit_line(text);
                }
            }
            (KeyCode::Backspace, _) => {
                if self.cursor > 0 {
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
            (KeyCode::PageUp, _) => self.scroll_up = self.scroll_up.saturating_add(10),
            (KeyCode::PageDown, _) => self.scroll_up = self.scroll_up.saturating_sub(10),
            (KeyCode::Up, _) => self.recall(-1),
            (KeyCode::Down, _) => self.recall(1),
            (KeyCode::Char(c), _) => self.insert(c),
            _ => {}
        }

        None
    }

    fn on_permission_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => self.answer_permission(Answer::Once),
            KeyCode::Char('a') | KeyCode::Char('A') => self.answer_permission(Answer::Always),
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
        app.pending = Some(PermissionRequest {
            tool: "bash".into(),
            risk: Risk::Execute,
            args: Value::Null,
            reply: tx,
        });
        app.on_key(key(KeyCode::Char('x')));
        assert!(app.input.is_empty());
        app.on_key(key(KeyCode::Char('a')));
        assert!(app.pending.is_none());
        assert_eq!(rx.blocking_recv().unwrap(), Answer::Always);
    }
}
