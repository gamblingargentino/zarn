use crate::widgets::{Choice, Edit, MaskedInput, Pick, Picker, Secret, TextField};
use crossterm::event::{KeyCode, KeyEvent};
use std::sync::Arc;
use zarn_config::auth::KeyStore;
use zarn_config::catalog::{self, Kind};
use zarn_config::layers::Layers;
use zarn_config::ops::{self, ProviderDraft};
use zarn_providers::ProviderSpec;

pub const SKIP: &str = "__skip__";
pub const WEB: &str = "__web__";

#[derive(Debug)]
pub enum Step {
    Kind(Picker),
    BaseUrl(TextField),
    Key(MaskedInput),
    Model(Picker),
    Testing,
    Failed(String),
    Saved(String),
    Browser(String),
}

impl Step {
    pub fn title(&self) -> &'static str {
        match self {
            Step::Kind(_) => "Pick a provider",
            Step::BaseUrl(_) => "Endpoint",
            Step::Key(_) => "API key",
            Step::Model(_) => "Pick a model",
            Step::Testing => "Testing",
            Step::Failed(_) => "The test request failed",
            Step::Saved(_) => "Ready",
            Step::Browser(_) => "Set up in your browser",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Test,
    ListModels,
    Done,
    Skipped,
    Web,
}

pub struct Onboarding {
    pub step: Step,
    pub kind: Option<&'static Kind>,
    pub base_url: Option<String>,
    pub model: String,
    pub message: Option<String>,
    key: Option<Secret>,
    layers: Layers,
    store: Arc<KeyStore>,
}

fn kind_picker() -> Picker {
    let mut choices: Vec<Choice> = vec![Choice::new(WEB, "Set up in your browser (a page on localhost)")];
    choices.extend(catalog::KINDS.iter().map(|k| Choice::new(k.id, k.label)));
    choices.push(Choice::new(SKIP, "Skip setup for now"));
    Picker::new("Which provider do you want to use?", choices)
}

impl Onboarding {
    pub fn new(layers: Layers, store: Arc<KeyStore>) -> Self {
        Onboarding {
            step: Step::Kind(kind_picker()),
            kind: None,
            base_url: None,
            model: String::new(),
            message: None,
            key: None,
            layers,
            store,
        }
    }

    pub fn back_to_start(&mut self) {
        self.step = Step::Kind(kind_picker());
        self.message = None;
    }

    pub fn store_label(&self) -> String {
        self.store.backend_label()
    }

    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    fn name(&self) -> String {
        self.kind.map(|k| k.id.to_string()).unwrap_or_default()
    }

    pub fn draft(&self) -> ProviderDraft {
        ProviderDraft {
            name: self.name(),
            kind: self.name(),
            base_url: self.base_url.clone(),
            api_key_env: None,
            model: Some(self.model.clone()).filter(|m| !m.is_empty()),
        }
    }

    pub fn spec(&self) -> ProviderSpec {
        let mut spec = self.draft().spec();
        spec.api_key = self.key.as_ref().map(|k| k.expose().to_string());
        spec
    }

    fn needs_key_step(&self) -> bool {
        self.kind
            .is_some_and(|k| k.needs_key || k.id == "openai-compat")
    }

    fn after_kind(&mut self) -> Option<Effect> {
        let kind = self.kind?;
        if kind.asks_base_url {
            self.step = Step::BaseUrl(TextField::new(
                self.base_url
                    .clone()
                    .or_else(|| kind.default_base_url.map(str::to_string))
                    .unwrap_or_default(),
            ));
            return None;
        }
        self.after_base_url()
    }

    fn after_base_url(&mut self) -> Option<Effect> {
        if self.needs_key_step() {
            self.step = Step::Key(MaskedInput::new());
            return None;
        }
        self.show_models()
    }

    fn show_models(&mut self) -> Option<Effect> {
        let kind = self.kind?;
        let mut choices: Vec<Choice> = kind.models.iter().map(|m| Choice::plain(*m)).collect();
        if !self.model.is_empty() && !choices.iter().any(|c| c.value == self.model) {
            choices.insert(0, Choice::plain(self.model.clone()));
        }
        let picker = Picker::new("Which model?", choices)
            .custom("loading the models this key can use; or type any model name, then enter")
            .select_value(&self.model);
        self.step = Step::Model(picker);
        Some(Effect::ListModels)
    }

    fn start_test(&mut self) -> Option<Effect> {
        self.step = Step::Testing;
        self.message = None;
        Some(Effect::Test)
    }

    pub fn set_models(&mut self, models: Vec<String>) {
        if let Step::Model(picker) = &mut self.step {
            let mut choices = picker.choices.clone();
            let mut found = 0;
            for model in models.into_iter().filter(|m| ops::is_chat_model(m)) {
                found += 1;
                if !choices.iter().any(|c| c.value == model) {
                    choices.push(Choice::plain(model));
                }
            }
            picker.hint = match found {
                0 => "the provider listed no models; type a model name, then enter".to_string(),
                n => format!("{n} models this key can use · type to filter, or name any model"),
            };
            picker.set_choices(choices);
        }
    }

    pub fn test_result(&mut self, result: Result<String, String>) {
        match result {
            Err(err) => self.step = Step::Failed(err),
            Ok(ok) => match self.save() {
                Ok(where_) => {
                    self.key = None;
                    self.step = Step::Saved(format!("{ok}. {where_}"));
                }
                Err(err) => self.step = Step::Failed(format!("the test passed but saving failed: {err:#}")),
            },
        }
    }

    fn save(&mut self) -> anyhow::Result<String> {
        let draft = self.draft();
        let scope = ops::add_provider(&mut self.layers, &draft)?;
        let mut parts = vec![format!(
            "{} saved to the {} config as the default provider with model {}",
            draft.name,
            scope.label(),
            self.model
        )];
        if let Some(key) = &self.key {
            let source = self.store.save(&draft.name, key.expose())?;
            parts.push(format!("the key is in {}", source.describe(&self.store)));
            if let Some(notice) = self.store.fallback_notice() {
                parts.push(notice);
            }
        }
        let cfg = self.layers.merged();
        let profile = cfg.default_profile();
        if cfg
            .profiles
            .get(&profile)
            .is_some_and(|p| p.provider.as_deref() == Some(draft.name.as_str()))
        {
            ops::set_profile_model(&mut self.layers, &profile, &self.model)?;
        }
        Ok(parts.join("; "))
    }

    pub fn paste(&mut self, text: &str) {
        match &mut self.step {
            Step::Key(input) => input.paste(text),
            Step::BaseUrl(field) => field.paste(text),
            Step::Kind(picker) | Step::Model(picker) => picker.filter.paste(text),
            _ => {}
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Effect> {
        let step = std::mem::replace(&mut self.step, Step::Testing);
        match step {
            Step::Kind(mut picker) => match picker.on_key(key) {
                Pick::Chosen(value) if value == SKIP => {
                    self.step = Step::Kind(picker);
                    Some(Effect::Skipped)
                }
                Pick::Chosen(value) if value == WEB => {
                    self.step = Step::Kind(picker);
                    Some(Effect::Web)
                }
                Pick::Chosen(value) | Pick::Custom(value) => {
                    self.kind = catalog::kind(&value);
                    self.model.clear();
                    self.base_url = None;
                    self.key = None;
                    if self.kind.is_none() {
                        self.step = Step::Kind(picker);
                        return None;
                    }
                    self.after_kind()
                }
                Pick::Cancel => {
                    self.message = Some("pick a provider, or choose \"Skip setup for now\"".into());
                    self.step = Step::Kind(picker);
                    None
                }
                Pick::Pending => {
                    self.step = Step::Kind(picker);
                    None
                }
            },
            Step::BaseUrl(mut field) => match field.on_key(key) {
                Edit::Submit => {
                    let url = field.value.trim().to_string();
                    if url.is_empty() {
                        self.message = Some("enter the endpoint URL".into());
                        self.step = Step::BaseUrl(field);
                        return None;
                    }
                    self.base_url = Some(url);
                    self.message = None;
                    self.after_base_url()
                }
                Edit::Cancel => {
                    self.step = Step::Kind(kind_picker());
                    None
                }
                _ => {
                    self.step = Step::BaseUrl(field);
                    None
                }
            },
            Step::Key(mut input) => match input.on_key(key) {
                Edit::Submit => {
                    let value = input.take();
                    let required = self.kind.is_some_and(|k| k.needs_key);
                    if value.is_empty() && required {
                        self.message = Some("paste your API key; it is not shown while you type".into());
                        self.step = Step::Key(input);
                        return None;
                    }
                    self.key = (!value.is_empty()).then(|| Secret::new(value));
                    self.message = None;
                    self.show_models()
                }
                Edit::Cancel => {
                    self.step = Step::Kind(kind_picker());
                    None
                }
                _ => {
                    self.step = Step::Key(input);
                    None
                }
            },
            Step::Model(mut picker) => match picker.on_key(key) {
                Pick::Chosen(model) | Pick::Custom(model) => {
                    self.model = model;
                    self.start_test()
                }
                Pick::Cancel => {
                    if self.needs_key_step() {
                        self.step = Step::Key(MaskedInput::new());
                    } else {
                        self.step = Step::Kind(kind_picker());
                    }
                    None
                }
                Pick::Pending => {
                    if picker.visible().is_empty() && !picker.filter.value.is_empty() {
                        self.message = Some("press enter to use the model name you typed".into());
                    }
                    self.step = Step::Model(picker);
                    None
                }
            },
            Step::Testing => None,
            Step::Failed(err) => match key.code {
                KeyCode::Char('r') | KeyCode::Enter => self.start_test(),
                KeyCode::Char('k') if self.needs_key_step() => {
                    self.step = Step::Key(MaskedInput::new());
                    None
                }
                KeyCode::Char('m') => self.show_models(),
                KeyCode::Char('u') if self.kind.is_some_and(|k| k.asks_base_url) => self.after_kind(),
                KeyCode::Char('b') => {
                    self.step = Step::Kind(kind_picker());
                    None
                }
                KeyCode::Char('s') => {
                    self.step = Step::Failed(err);
                    Some(Effect::Skipped)
                }
                _ => {
                    self.step = Step::Failed(err);
                    None
                }
            },
            Step::Browser(url) => match key.code {
                KeyCode::Esc => {
                    self.step = Step::Kind(kind_picker());
                    None
                }
                KeyCode::Enter => {
                    self.step = Step::Browser(url);
                    Some(Effect::Web)
                }
                _ => {
                    self.step = Step::Browser(url);
                    None
                }
            },
            Step::Saved(text) => match key.code {
                KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ') => {
                    self.step = Step::Saved(text);
                    Some(Effect::Done)
                }
                _ => {
                    self.step = Step::Saved(text);
                    None
                }
            },
        }
    }

    pub fn failure_hints(&self) -> String {
        let mut hints = vec!["r retry"];
        if self.needs_key_step() {
            hints.push("k re-enter key");
        }
        hints.push("m change model");
        if self.kind.is_some_and(|k| k.asks_base_url) {
            hints.push("u change endpoint");
        }
        hints.push("b start over");
        hints.push("s skip setup");
        hints.join(" · ")
    }
}
