use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::fmt;

pub const MASK: char = '•';

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    Changed,
    Submit,
    Cancel,
    Ignored,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextField {
    pub value: String,
    pub cursor: usize,
}

impl TextField {
    pub fn paste_raw(&mut self, text: &str) {
        for c in text.chars() {
            let at = self.byte_at(self.cursor);
            self.value.insert(at, c);
            self.cursor += 1;
        }
    }

    pub fn paste(&mut self, text: &str) {
        for c in text.chars().filter(|c| !c.is_control()) {
            let at = self.byte_at(self.cursor);
            self.value.insert(at, c);
            self.cursor += 1;
        }
    }

    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        TextField { value, cursor }
    }

    fn byte_at(&self, index: usize) -> usize {
        self.value
            .char_indices()
            .nth(index)
            .map(|(i, _)| i)
            .unwrap_or(self.value.len())
    }

    pub fn len(&self) -> usize {
        self.value.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    pub fn set(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.len();
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Edit {
        match key.code {
            KeyCode::Enter => Edit::Submit,
            KeyCode::Esc => Edit::Cancel,
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.clear();
                self.cursor = 0;
                Edit::Changed
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let at = self.byte_at(self.cursor);
                self.value.insert(at, c);
                self.cursor += 1;
                Edit::Changed
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let at = self.byte_at(self.cursor - 1);
                self.value.remove(at);
                self.cursor -= 1;
                Edit::Changed
            }
            KeyCode::Delete if self.cursor < self.len() => {
                let at = self.byte_at(self.cursor);
                self.value.remove(at);
                Edit::Changed
            }
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                Edit::Ignored
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.len());
                Edit::Ignored
            }
            KeyCode::Home => {
                self.cursor = 0;
                Edit::Ignored
            }
            KeyCode::End => {
                self.cursor = self.len();
                Edit::Ignored
            }
            _ => Edit::Ignored,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        let len = self.0.len();
        self.0.replace_range(.., &"\0".repeat(len));
    }
}

#[derive(Default)]
pub struct MaskedInput {
    value: String,
}

impl fmt::Debug for MaskedInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MaskedInput({} chars)", self.value.chars().count())
    }
}

impl Drop for MaskedInput {
    fn drop(&mut self) {
        self.clear();
    }
}

impl MaskedInput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.value.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    pub fn masked(&self) -> String {
        MASK.to_string().repeat(self.len())
    }

    pub fn clear(&mut self) {
        let len = self.value.len();
        self.value.replace_range(.., &"\0".repeat(len));
        self.value.clear();
    }

    pub fn take(&mut self) -> String {
        std::mem::take(&mut self.value).trim().to_string()
    }

    pub fn paste(&mut self, text: &str) {
        self.value
            .extend(text.chars().filter(|c| !c.is_control()));
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Edit {
        match key.code {
            KeyCode::Enter => Edit::Submit,
            KeyCode::Esc => Edit::Cancel,
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.clear();
                Edit::Changed
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.value.push(c);
                Edit::Changed
            }
            KeyCode::Backspace => {
                self.value.pop();
                Edit::Changed
            }
            _ => Edit::Ignored,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub value: String,
    pub label: String,
    pub detail: String,
}

impl Choice {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Choice {
            value: value.into(),
            label: label.into(),
            detail: String::new(),
        }
    }

    pub fn plain(value: impl Into<String>) -> Self {
        let value = value.into();
        Choice::new(value.clone(), value)
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    Chosen(String),
    Custom(String),
    Cancel,
    Pending,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Picker {
    pub title: String,
    pub choices: Vec<Choice>,
    pub filter: TextField,
    pub selected: usize,
    pub allow_custom: bool,
    pub hint: String,
}

impl Picker {
    pub fn new(title: impl Into<String>, choices: Vec<Choice>) -> Self {
        Picker {
            title: title.into(),
            choices,
            ..Picker::default()
        }
    }

    pub fn custom(mut self, hint: impl Into<String>) -> Self {
        self.allow_custom = true;
        self.hint = hint.into();
        self
    }

    pub fn select_value(mut self, value: &str) -> Self {
        if let Some(i) = self.visible().iter().position(|c| c.value == value) {
            self.selected = i;
        }
        self
    }

    pub fn set_choices(&mut self, choices: Vec<Choice>) {
        self.choices = choices;
        self.selected = self.selected.min(self.visible().len().saturating_sub(1));
    }

    pub fn visible(&self) -> Vec<&Choice> {
        let needle = self.filter.value.to_lowercase();
        if needle.is_empty() {
            return self.choices.iter().collect();
        }
        let mut starts: Vec<&Choice> = Vec::new();
        let mut contains: Vec<&Choice> = Vec::new();
        for choice in &self.choices {
            let label = choice.label.to_lowercase();
            let value = choice.value.to_lowercase();
            if label.starts_with(&needle) || value.starts_with(&needle) {
                starts.push(choice);
            } else if label.contains(&needle) || value.contains(&needle) || choice.detail.to_lowercase().contains(&needle) {
                contains.push(choice);
            }
        }
        starts.extend(contains);
        starts
    }

    pub fn current(&self) -> Option<&Choice> {
        self.visible().get(self.selected).copied()
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Pick {
        match key.code {
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                Pick::Pending
            }
            KeyCode::Down => {
                let last = self.visible().len().saturating_sub(1);
                self.selected = (self.selected + 1).min(last);
                Pick::Pending
            }
            KeyCode::Esc => Pick::Cancel,
            KeyCode::Enter => match self.current() {
                Some(choice) => Pick::Chosen(choice.value.clone()),
                None if self.allow_custom && !self.filter.value.trim().is_empty() => {
                    Pick::Custom(self.filter.value.trim().to_string())
                }
                None => Pick::Pending,
            },
            KeyCode::Tab if self.allow_custom && !self.filter.value.trim().is_empty() => {
                Pick::Custom(self.filter.value.trim().to_string())
            }
            _ => {
                if self.filter.on_key(key) == Edit::Changed {
                    self.selected = 0;
                }
                Pick::Pending
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_into(input: &mut MaskedInput, text: &str) {
        for c in text.chars() {
            input.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn a_masked_input_shows_only_bullets_and_hides_itself_from_debug() {
        let mut input = MaskedInput::new();
        type_into(&mut input, "test-key-not-real");
        assert_eq!(input.masked(), MASK.to_string().repeat(17));
        assert!(!format!("{input:?}").contains("test"));
        input.on_key(key(KeyCode::Backspace));
        assert_eq!(input.take(), "test-key-not-rea");
        assert!(input.is_empty());
    }

    #[test]
    fn pasted_control_characters_are_dropped() {
        let mut input = MaskedInput::new();
        input.paste("abc\n\r\t");
        assert_eq!(input.len(), 3);
    }

    #[test]
    fn a_picker_filters_prefix_matches_first() {
        let mut picker = Picker::new(
            "t",
            vec![
                Choice::plain("gpt-5-mini"),
                Choice::plain("claude-sonnet"),
                Choice::plain("mini-claude"),
            ],
        );
        for c in "mini".chars() {
            picker.on_key(key(KeyCode::Char(c)));
        }
        let shown: Vec<&str> = picker.visible().iter().map(|c| c.value.as_str()).collect();
        assert_eq!(shown, vec!["mini-claude", "gpt-5-mini"]);
        assert_eq!(picker.on_key(key(KeyCode::Enter)), Pick::Chosen("mini-claude".into()));
    }

    #[test]
    fn a_custom_picker_accepts_unlisted_text() {
        let mut picker = Picker::new("t", vec![Choice::plain("a")]).custom("type a name");
        for c in "zzz".chars() {
            picker.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(picker.on_key(key(KeyCode::Enter)), Pick::Custom("zzz".into()));
    }

    #[test]
    fn a_text_field_edits_at_the_cursor() {
        let mut field = TextField::new("ac");
        field.on_key(key(KeyCode::Left));
        field.on_key(key(KeyCode::Char('b')));
        assert_eq!(field.value, "abc");
        assert_eq!(field.on_key(key(KeyCode::Enter)), Edit::Submit);
    }
}
