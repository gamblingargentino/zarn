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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfirmFor {
    Bypass,
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
    pub queue: Vec<String>,
    pub queue_paused: bool,
    pub plan_review: Option<PlanReview>,
    pub pending_expanded: bool,
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
            Overlay::Confirm(text, purpose) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => Some(Action::Confirmed(purpose)),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.notice("cancelled");
                    None
                }
                _ => {
                    self.overlay = Some(Overlay::Confirm(text, purpose));
                    None
                }
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
        self.scroll_up = 0;
        if text.starts_with('/') && popup::split(&text).is_some() {
            self.blocks.push(Block::User(text.clone()));
            return Some(Action::Command(text));
        }
        if self.streaming || self.queue_paused {
            self.queue.push(text.clone());
            self.blocks.push(Block::Queued(text));
            return None;
        }
        self.blocks.push(Block::User(text.clone()));
        Some(Action::Submit(text))
    }

    pub fn next_queued(&mut self) -> Option<String> {
        if self.queue_paused || self.queue.is_empty() {
            return None;
        }
        let text = self.queue.remove(0);
        if let Some(at) = self
            .blocks
            .iter()
            .position(|b| matches!(b, Block::Queued(t) if *t == text))
        {
            self.blocks.remove(at);
        }
        self.blocks.push(Block::User(text.clone()));
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
        self.pending_expanded = false;
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

        if let Some(overlay) = self.overlay.take() {
            return self.on_overlay_key(overlay, key);
        }

        if let Some(action) = self.on_popup_key(key) {
            return action;
        }

        match (key.code, key.modifiers) {
            (KeyCode::BackTab, _) => return Some(Action::CycleMode),
            (KeyCode::Char('p'), m) if m.contains(KeyModifiers::CONTROL) => {
                return Some(Action::Command("/profile".into()));
            }
            (KeyCode::Tab, m) if m.contains(KeyModifiers::SHIFT) => return Some(Action::CycleMode),
            (KeyCode::Char('r'), m) if m.contains(KeyModifiers::CONTROL) => {
                self.expand_last_output()
            }
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
                if self.queue_paused && !self.streaming {
                    self.queue_paused = false;
                    return Some(Action::SendQueued);
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
        Some(Action::Plan(verdict))
    }

    fn on_permission_key(&mut self, key: KeyEvent) -> Option<Action> {
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
