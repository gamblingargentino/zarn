use crate::brand;
use crate::onboarding::{self, Onboarding};
use crate::popup;
use crate::render;
use crate::settings::{self, Panel, PanelData, RowKind, SkillRow, Tab, TestState};
use crate::state::{Action, App, Block, ChatPick, Mode, Overlay, PermissionRequest};
use crate::terminal;
use crate::widgets::{Choice, MaskedInput, Picker};
use anyhow::Result;
use async_trait::async_trait;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent, EventStream,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use futures::StreamExt;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use zarn_config::auth::KeyStore;
use zarn_config::layers::{GeneralField, Layers};
use zarn_config::runtime::{self, Runtime};
use zarn_config::{Config, catalog, context, doctor, guard, ops, paths};
use zarn_core::agent::{Agent, AgentEvent};
use zarn_core::commands::{Command as CoreCommand, Commands};
use zarn_core::compact;
use zarn_core::cost::{CostTable, CostTracker};
use zarn_core::session::{self, Session};
use zarn_core::tools::{Ctx, Risk};
use zarn_core::types::{Message, StopReason};
use zarn_tools::mcp::{McpServer, ServerConfig};
use zarn_tools::permissions::{Answer, PermissionPrompt, Permissions, Policy};

const FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(140);
const MCP_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub const INIT_PROMPT: &str = "Inspect this repository and write a ZARN.md file at its root for future coding sessions. Use glob, grep and read to learn the layout, the build, test and lint commands, the main modules and how they fit together, and any conventions the code follows. Keep it short and specific to this project: commands that work, where things live, rules that are not obvious from the code. If a ZARN.md already exists, read it first and improve it rather than starting over. Write the file with the write tool, then reply with a two-line summary.";

pub struct TuiPrompt {
    tx: mpsc::Sender<PermissionRequest>,
}

#[async_trait]
impl PermissionPrompt for TuiPrompt {
    async fn ask(&self, tool: &str, risk: Risk, args: &Value) -> Answer {
        let (reply, answer) = oneshot::channel();
        let request = PermissionRequest {
            tool: tool.to_string(),
            risk,
            args: args.clone(),
            reply,
        };
        if self.tx.send(request).await.is_err() {
            return Answer::No;
        }
        answer.await.unwrap_or(Answer::No)
    }
}

pub fn permission_channel() -> (Arc<TuiPrompt>, mpsc::Receiver<PermissionRequest>) {
    let (tx, rx) = mpsc::channel(4);
    (Arc::new(TuiPrompt { tx }), rx)
}

#[derive(Clone, Default)]
pub struct Options {
    pub profile: Option<String>,
    pub model: Option<String>,
    pub resume: Option<PathBuf>,
    pub store: Option<Arc<KeyStore>>,
}

#[derive(Clone, Debug)]
enum ModelTarget {
    Panel(String),
    Onboarding,
    Chat,
}

enum UiEvent {
    Agent(AgentEvent),
    TurnDone {
        messages: Vec<Message>,
        result: Result<StopReason, String>,
    },
    Compacted(Result<Vec<Message>, String>),
    PanelTest {
        name: String,
        result: Result<String, String>,
    },
    OnboardTest(Result<String, String>),
    Models {
        target: ModelTarget,
        models: Vec<String>,
    },
    Mcp {
        name: String,
        config: ServerConfig,
        result: Result<McpServer, String>,
    },
    Doctor(String),
    Notice(String),
}

struct McpState {
    config: ServerConfig,
    status: String,
    server: Option<Arc<McpServer>>,
}

pub async fn run(options: Options) -> Result<()> {
    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
        EnableBracketedPaste
    );

    let outcome = match Controller::new(options) {
        Ok(controller) => controller.event_loop(&mut terminal).await,
        Err(err) => Err(err),
    };

    let _ = crossterm::execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        PopKeyboardEnhancementFlags
    );
    ratatui::restore();
    outcome
}

pub struct Controller {
    app: App,
    options: Options,
    store: Arc<KeyStore>,
    layers: Option<Layers>,
    cwd: PathBuf,
    ctx: Arc<Ctx>,
    gate: Arc<Permissions>,
    permissions: mpsc::Receiver<PermissionRequest>,
    agent: Option<Arc<Agent>>,
    runtime: Option<Runtime>,
    system: String,
    messages: Vec<Message>,
    session: Option<Session>,
    persisted: usize,
    session_failed: bool,
    cost: CostTracker,
    mcp: BTreeMap<String, McpState>,
    commands: Commands,
    ui_tx: mpsc::Sender<UiEvent>,
    ui_rx: Option<mpsc::Receiver<UiEvent>>,
    cancel: CancellationToken,
    keyboard: bool,
    fingerprint: u64,
}

fn flatten_messages(messages: &[Message]) -> Vec<Block> {
    let mut blocks = Vec::new();
    for message in messages {
        match message {
            Message::User(_) => blocks.push(Block::User(message.text())),
            Message::Assistant { .. } => {
                let text = message.text();
                if !text.trim().is_empty() {
                    blocks.push(Block::Assistant(text));
                }
            }
            _ => {}
        }
    }
    blocks
}

impl Controller {
    pub fn new(options: Options) -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let ctx = Arc::new(Ctx::new(&cwd));
        let (prompt, permissions) = permission_channel();
        let gate = Arc::new(Permissions::new(prompt));
        let (ui_tx, ui_rx) = mpsc::channel::<UiEvent>(256);
        let commands = Commands::load(&paths::project_dir().join("commands"));

        let mut controller = Controller {
            app: App::new(""),
            options: options.clone(),
            store: options.store.clone().unwrap_or_else(KeyStore::shared),
            layers: None,
            cwd,
            ctx,
            gate,
            permissions,
            agent: None,
            runtime: None,
            system: String::new(),
            messages: vec![Message::System(String::new())],
            session: None,
            persisted: 1,
            session_failed: false,
            cost: CostTracker::new(CostTable::empty(), ""),
            mcp: BTreeMap::new(),
            commands,
            ui_tx,
            ui_rx: Some(ui_rx),
            cancel: CancellationToken::new(),
            keyboard: terminal::keyboard_enhanced(),
            fingerprint: 0,
        };

        if let Some(path) = controller.options.resume.clone() {
            controller.resume(&path);
        }
        controller.rebuild();

        let fresh = controller
            .layers
            .as_ref()
            .is_some_and(|l| !l.merged().has_provider());
        if fresh && let Some(layers) = controller.layers.clone() {
            controller.app.mode = Mode::Onboarding(Box::new(Onboarding::new(
                layers,
                controller.store.clone(),
            )));
        }
        Ok(controller)
    }

    fn layers(&mut self) -> Option<&mut Layers> {
        if self.layers.is_none() {
            match Layers::load() {
                Ok(layers) => self.layers = Some(layers),
                Err(err) => {
                    self.app.notice(format!("config error: {err:#}"));
                    return None;
                }
            }
        }
        self.layers.as_mut()
    }

    fn cfg(&self) -> Config {
        self.layers.as_ref().map(|l| l.merged()).unwrap_or_default()
    }

    fn profile(&self) -> String {
        runtime::active_profile(&self.cfg(), self.options.profile.as_deref())
    }

    fn rebuild(&mut self) {
        match Layers::load() {
            Ok(layers) => self.layers = Some(layers),
            Err(err) => {
                self.layers = None;
                self.agent = None;
                self.runtime = None;
                self.app.not_ready = Some("config error; see /doctor".into());
                self.app.notice(format!("config error: {err:#}"));
                return;
            }
        }
        let cfg = self.cfg();
        self.fingerprint = context::fingerprint(&self.cwd);
        self.cost.set_table(zarn_config::pricing::table(&cfg));
        self.app.status.cost = self.cost.total().usd;
        self.app.theme.accent = brand::accent(cfg.general.accent.as_deref());
        self.app.theme.banner = cfg.general.banner();
        self.app.theme.show_cost = cfg.general.show_cost();
        self.gate.set_policies(cfg.permissions.clone());
        self.sync_mcp(&cfg);
        self.refresh_entries(&cfg);

        if !cfg.has_provider() {
            self.agent = None;
            self.runtime = None;
            self.app.not_ready = Some("no provider; run /login or /config".into());
            return;
        }

        let profile = self.profile();
        let plan = match runtime::plan(&cfg, &self.store, &profile, self.options.model.as_deref()) {
            Ok(plan) => plan,
            Err(err) => {
                self.agent = None;
                self.runtime = None;
                self.app.not_ready = Some(format!("{err:#}"));
                return;
            }
        };
        if !plan.skipped.is_empty() {
            self.app.notice(format!(
                "fallback providers without a key are skipped: {}",
                plan.skipped.join(", ")
            ));
        }
        let tools = self
            .mcp
            .values()
            .filter_map(|m| m.server.as_ref())
            .flat_map(|s| s.tools())
            .collect();
        let tx = self.ui_tx.clone();
        let notify: runtime::Notify = Arc::new(move |text: String| {
            let _ = tx.try_send(UiEvent::Notice(zarn_config::auth::redact(&text)));
        });
        let assembly = runtime::Assembly {
            store: self.store.clone(),
            notify: Some(notify),
        };
        match runtime::assemble(&cfg, plan, &self.cwd, self.ctx.clone(), tools, assembly) {
            Ok(rt) => {
                let agent = Agent::new(
                    rt.provider.clone(),
                    rt.tools.clone(),
                    self.ctx.clone(),
                    rt.agent_config(),
                )
                .with_gate(self.gate.clone());
                self.agent = Some(Arc::new(agent));
                self.system = rt.system.clone();
                self.set_system();
                self.app.status.model = rt.plan.model.clone();
                self.cost.set_model(rt.plan.model.clone());
                self.app.not_ready = None;
                self.runtime = Some(rt);
                self.app.status.cost = self.cost.total().usd;
                self.update_meter();
            }
            Err(err) => {
                self.agent = None;
                self.runtime = None;
                self.app.not_ready = Some(format!("{err:#}"));
            }
        }
        if let Some(notice) = self.store.fallback_notice() {
            self.app.notice(notice);
        }
    }

    fn set_system(&mut self) {
        match self.messages.first_mut() {
            Some(Message::System(text)) => *text = self.system.clone(),
            _ => self.messages.insert(0, Message::System(self.system.clone())),
        }
    }

    fn refresh_entries(&mut self, cfg: &Config) {
        let custom: Vec<String> = self
            .commands
            .names()
            .into_iter()
            .filter_map(|n| n.strip_prefix('/').map(str::to_string))
            .filter(|n| !zarn_core::commands::BUILTINS.iter().any(|(b, _)| b.trim_start_matches('/') == n))
            .collect();
        let skills: Vec<(String, String)> = context::discover_skills()
            .into_iter()
            .filter(|s| cfg.skill_enabled(&s.name))
            .map(|s| (s.name, s.description))
            .collect();
        self.app.entries = popup::entries(&custom, &skills);
    }

    fn sync_mcp(&mut self, cfg: &Config) {
        let stale: Vec<String> = self
            .mcp
            .iter()
            .filter(|(name, state)| cfg.mcp.get(*name) != Some(&state.config))
            .map(|(name, _)| name.clone())
            .collect();
        for name in stale {
            if let Some(state) = self.mcp.remove(&name)
                && let Some(server) = state.server
            {
                tokio::spawn(async move { server.shutdown().await });
            }
        }
        for (name, config) in &cfg.mcp {
            if !self.mcp.contains_key(name) {
                self.start_mcp(name.clone(), config.clone());
            }
        }
    }

    fn start_mcp(&mut self, name: String, config: ServerConfig) {
        if let Some(state) = self.mcp.remove(&name)
            && let Some(server) = state.server
        {
            tokio::spawn(async move { server.shutdown().await });
        }
        self.mcp.insert(
            name.clone(),
            McpState {
                config: config.clone(),
                status: "starting…".into(),
                server: None,
            },
        );
        self.panel_mcp_status(&name, "starting…");
        let tx = self.ui_tx.clone();
        tokio::spawn(async move {
            let result = match tokio::time::timeout(MCP_START_TIMEOUT, McpServer::start(&name, &config)).await {
                Err(_) => Err("did not start within 30 seconds".to_string()),
                Ok(Err(err)) => Err(zarn_config::auth::redact(&format!("{err:#}"))),
                Ok(Ok(server)) => Ok(server),
            };
            let _ = tx.send(UiEvent::Mcp { name, config, result }).await;
        });
    }

    fn panel_mcp_status(&mut self, name: &str, status: &str) {
        if let Mode::Panel(panel) = &mut self.app.mode {
            panel.set_mcp_status(name, status);
        }
    }

    fn mcp_statuses(&self) -> BTreeMap<String, String> {
        self.mcp
            .iter()
            .map(|(name, state)| (name.clone(), state.status.clone()))
            .collect()
    }

    fn panel_data(&self) -> PanelData {
        let cfg = self.cfg();
        let skills = context::discover_skills()
            .into_iter()
            .map(|s| SkillRow {
                name: s.name,
                description: s.description,
                source: s.source.label().to_string(),
            })
            .collect();
        let mut tools: Vec<(String, Policy)> = match &self.runtime {
            Some(rt) => rt
                .tools
                .names()
                .into_iter()
                .filter_map(|name| {
                    let risk = rt.tools.get(&name)?.risk();
                    Some((name, if risk == Risk::Safe { Policy::Allow } else { Policy::Ask }))
                })
                .collect(),
            None => zarn_tools::builtin_list()
                .into_iter()
                .map(|t| {
                    let policy = if t.risk() == Risk::Safe { Policy::Allow } else { Policy::Ask };
                    (t.spec().name, policy)
                })
                .collect(),
        };
        for name in cfg.permissions.keys() {
            if !tools.iter().any(|(t, _)| t == name) {
                tools.push((name.clone(), Policy::Ask));
            }
        }
        PanelData {
            skills,
            mcp_status: self.mcp_statuses(),
            tools,
        }
    }

    fn open_panel(&mut self, tab: Tab) -> Option<&mut Panel> {
        let data = self.panel_data();
        let store = self.store.clone();
        let layers = self.layers()?.clone();
        self.app.overlay = None;
        self.app.mode = Mode::Panel(Box::new(Panel::new(layers, store, data).open_at(tab)));
        match &mut self.app.mode {
            Mode::Panel(panel) => Some(panel),
            _ => None,
        }
    }

    async fn event_loop(mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        let mut ui_rx = self.ui_rx.take().expect("receiver");
        let mut keys = EventStream::new();
        let mut frames = tokio::time::interval(FRAME_INTERVAL);

        loop {
            terminal.draw(|frame| render::draw(frame, &self.app))?;

            tokio::select! {
                _ = frames.tick(), if self.app.streaming => self.app.advance_frame(),
                event = keys.next() => {
                    let Some(Ok(event)) = event else { continue };
                    if !self.on_terminal(event) {
                        break;
                    }
                }
                event = ui_rx.recv() => {
                    match event {
                        None => break,
                        Some(event) => self.on_ui(event),
                    }
                }
                request = self.permissions.recv() => {
                    if let Some(request) = request {
                        self.app.pending = Some(request);
                    }
                }
            }
        }

        self.shutdown().await;
        Ok(())
    }

    fn on_terminal(&mut self, event: TermEvent) -> bool {
        let action = match event {
            TermEvent::Key(key) => self.app.on_key(key),
            TermEvent::Paste(text) => {
                self.app.paste(&text);
                None
            }
            _ => None,
        };
        match action {
            Some(action) => self.act(action),
            None => true,
        }
    }

    async fn shutdown(&mut self) {
        self.cancel.cancel();
        for state in std::mem::take(&mut self.mcp).into_values() {
            if let Some(server) = state.server {
                server.shutdown().await;
            }
        }
    }

    pub fn app(&self) -> &App {
        &self.app
    }

    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        self.on_terminal(TermEvent::Key(key))
    }

    pub fn paste(&mut self, text: &str) {
        self.on_terminal(TermEvent::Paste(text.to_string()));
    }

    pub fn type_line(&mut self, text: &str) -> bool {
        self.app.set_input(text);
        self.key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ))
    }

    pub async fn pump(&mut self, wait: std::time::Duration) -> bool {
        let Some(mut ui_rx) = self.ui_rx.take() else {
            return false;
        };
        let got = tokio::select! {
            event = ui_rx.recv() => match event {
                Some(event) => {
                    self.on_ui(event);
                    true
                }
                None => false,
            },
            request = self.permissions.recv() => match request {
                Some(request) => {
                    self.app.pending = Some(request);
                    true
                }
                None => false,
            },
            _ = tokio::time::sleep(wait) => false,
        };
        self.ui_rx = Some(ui_rx);
        got
    }

    pub async fn settle(&mut self, limit: std::time::Duration) {
        let deadline = tokio::time::Instant::now() + limit;
        while tokio::time::Instant::now() < deadline {
            let got = self.pump(std::time::Duration::from_millis(60)).await;
            if !got && !self.busy() && self.app.pending.is_none() {
                break;
            }
            if self.app.pending.is_some() && !got {
                break;
            }
        }
    }

    fn busy(&self) -> bool {
        self.app.streaming
    }

    fn act(&mut self, action: Action) -> bool {
        match action {
            Action::Quit => return false,
            Action::Cancel => self.cancel.cancel(),
            Action::Submit(text) => self.submit(text),
            Action::Command(line) => return self.command(&line),
            Action::Panel(effect) => self.panel_effect(effect),
            Action::Onboard(effect) => self.onboard_effect(effect),
            Action::Picked(purpose, value) => self.picked(purpose, value),
            Action::SendQueued => self.send_next(),
            Action::Login(provider, secret) => {
                let store = self.store.clone();
                let result = match self.layers() {
                    Some(layers) => ops::login(layers, &store, &provider, secret.expose()),
                    None => return true,
                };
                drop(secret);
                match result {
                    Ok(source) => {
                        let place = source.describe(&self.store);
                        self.app.notice(format!("key for {provider} saved to {place}"));
                        self.rebuild();
                    }
                    Err(err) => self.app.notice(format!("login failed: {err:#}")),
                }
            }
        }
        true
    }

    fn send_next(&mut self) {
        if self.app.streaming {
            return;
        }
        if let Some(text) = self.app.next_queued() {
            self.submit(text);
        }
    }

    fn update_meter(&mut self) {
        let Some(agent) = &self.agent else {
            self.app.status.context = None;
            return;
        };
        let provider = agent.provider();
        let window = provider.caps().context_window;
        let used = provider.count_tokens(&self.messages).unwrap_or(0);
        self.app.status.context = Some((used, window));
    }

    fn reload_if_changed(&mut self) {
        let now = context::fingerprint(&self.cwd);
        if now != self.fingerprint {
            self.rebuild();
            self.app.notice("ZARN.md or skills changed on disk; reloaded them for this turn");
        }
    }

    fn start_compaction(&mut self, auto: bool) {
        let Some(agent) = self.agent.clone() else {
            self.app.notice("cannot compact: no provider is ready");
            return;
        };
        if compact::split_point(&self.messages, compact::KEEP_TURNS).is_none() {
            if !auto {
                self.app.notice("the conversation is too short to compact");
            }
            return;
        }
        self.app.streaming = true;
        if auto {
            let pct = self
                .app
                .status
                .context
                .map(|(u, w)| if w == 0 { 0 } else { u64::from(u) * 100 / u64::from(w) })
                .unwrap_or(0);
            self.app.notice(format!(
                "context is {pct}% full; compacting before the next turn"
            ));
        } else {
            self.app.notice("compacting…");
        }
        let messages = self.messages.clone();
        let model = agent.config().model.clone();
        let tx = self.ui_tx.clone();
        tokio::spawn(async move {
            let result = compact::compact(agent.provider().as_ref(), &messages, &model, compact::KEEP_TURNS)
                .await
                .map_err(|e| zarn_config::auth::redact(&e.to_string()));
            let _ = tx.send(UiEvent::Compacted(result)).await;
        });
    }

    fn needs_auto_compact(&self) -> bool {
        let Some(agent) = &self.agent else {
            return false;
        };
        self.cfg().general.auto_compact()
            && compact::should_compact(agent.provider().as_ref(), &self.messages)
            && compact::split_point(&self.messages, compact::KEEP_TURNS).is_some()
    }

    fn submit(&mut self, text: String) {
        if self.app.streaming {
            self.app.queue.push(text.clone());
            self.app.blocks.push(Block::Queued(text));
            return;
        }
        self.reload_if_changed();
        let Some(agent) = self.agent.clone() else {
            let why = self
                .app
                .not_ready
                .clone()
                .unwrap_or_else(|| "no provider is ready".into());
            self.app.notice(format!("cannot send: {why}"));
            return;
        };
        self.messages.push(Message::user(text));
        self.cancel = CancellationToken::new();
        self.app.streaming = true;
        spawn_turn(agent, self.messages.clone(), self.ui_tx.clone(), self.cancel.clone());
    }

    fn on_ui(&mut self, event: UiEvent) {
        match event {
            UiEvent::Agent(event) => {
                if let AgentEvent::Provider(provider_event) = &event {
                    self.cost.observe(provider_event);
                }
                self.app.apply(event);
                self.app.status.cost = self.cost.total().usd;
            }
            UiEvent::TurnDone { messages, result } => {
                self.app.streaming = false;
                self.messages = messages;
                self.set_system();
                self.cost.end_turn();
                self.app.status.cost = self.cost.total().usd;
                self.persist();
                self.update_meter();
                match result {
                    Ok(StopReason::Cancelled) => {
                        self.app.notice("cancelled");
                        self.app.pause_queue();
                        return;
                    }
                    Ok(_) => {}
                    Err(err) => self.app.notice(format!("error: {err}")),
                }
                if self.needs_auto_compact() {
                    self.start_compaction(true);
                } else {
                    self.send_next();
                }
            }
            UiEvent::Compacted(result) => {
                self.app.streaming = false;
                match result {
                    Ok(messages) => {
                        let before = self.messages.len();
                        self.messages = messages;
                        self.set_system();
                        self.session = None;
                        self.persisted = 1;
                        self.app.notice(format!(
                            "compacted {before} messages into {}",
                            self.messages.len()
                        ));
                    }
                    Err(err) => self.app.notice(format!("compaction failed: {err}")),
                }
                self.update_meter();
                self.send_next();
            }
            UiEvent::PanelTest { name, result } => {
                if let Mode::Panel(panel) = &mut self.app.mode {
                    panel.set_test(
                        &name,
                        match result {
                            Ok(ok) => TestState::Ok(ok),
                            Err(err) => TestState::Failed(err),
                        },
                    );
                } else {
                    match result {
                        Ok(ok) => self.app.notice(format!("{name}: {ok}")),
                        Err(err) => self.app.notice(format!("{name}: FAILED: {err}")),
                    }
                }
            }
            UiEvent::OnboardTest(result) => {
                if let Mode::Onboarding(flow) = &mut self.app.mode {
                    flow.test_result(result);
                }
            }
            UiEvent::Models { target, models } => match (&mut self.app.mode, target) {
                (Mode::Panel(panel), ModelTarget::Panel(profile)) => panel.set_models(&profile, models),
                (Mode::Onboarding(flow), ModelTarget::Onboarding) => flow.set_models(models),
                (Mode::Chat, ModelTarget::Chat) => {
                    if let Some(Overlay::Pick(picker, ChatPick::Model)) = &mut self.app.overlay {
                        let mut choices = picker.choices.clone();
                        for model in models {
                            if !choices.iter().any(|c| c.value == model) {
                                choices.push(Choice::plain(model));
                            }
                        }
                        picker.set_choices(choices);
                    }
                }
                _ => {}
            },
            UiEvent::Mcp { name, config, result } => {
                let current = self.mcp.get(&name).map(|s| s.config.clone());
                if current.as_ref() != Some(&config) {
                    if let Ok(server) = result {
                        tokio::spawn(async move { server.shutdown().await });
                    }
                    return;
                }
                let status = match &result {
                    Ok(server) => format!("running, {} tools", server.tool_names().len()),
                    Err(err) => format!("failed: {err}"),
                };
                if let Some(state) = self.mcp.get_mut(&name) {
                    state.status = status.clone();
                    state.server = result.ok().map(Arc::new);
                }
                self.panel_mcp_status(&name, &status);
                if status.starts_with("failed") && matches!(self.app.mode, Mode::Chat) {
                    self.app.notice(format!("mcp {name} {status}"));
                }
                self.rebuild();
            }
            UiEvent::Doctor(report) => self.app.notice(report),
            UiEvent::Notice(text) => self.app.notice(text),
        }
    }

    fn persist(&mut self) {
        if self.messages.len() <= 1 || self.session_failed {
            return;
        }
        if self.session.is_none() {
            match Session::create(
                &session::data_root(),
                &self.cwd,
                &self.app.status.model,
                Some(self.profile()),
            ) {
                Ok(created) => {
                    self.session = Some(created);
                    self.persisted = 1;
                }
                Err(err) => {
                    self.session_failed = true;
                    self.app.notice(format!("session not saved: {err:#}"));
                    return;
                }
            }
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let start = self.persisted.clamp(1, self.messages.len());
        if let Err(err) = guard::append_redacted(session, &self.messages[start..]) {
            self.app.notice(format!("session write failed: {err:#}"));
            return;
        }
        self.persisted = self.messages.len();
        if let Some(turn) = self.cost.turns().last() {
            let _ = session.record_usage(turn.usage.input, turn.usage.output, turn.usage.cached);
        }
    }

    fn resume(&mut self, path: &Path) {
        match session::read(path) {
            Ok((meta, loaded)) => {
                let loaded: Vec<Message> = loaded
                    .into_iter()
                    .skip_while(|m| matches!(m, Message::System(_)))
                    .collect();
                self.messages = vec![Message::System(self.system.clone())];
                self.messages.extend(loaded);
                self.app.blocks = flatten_messages(&self.messages);
                match Session::open(path) {
                    Ok(opened) => self.session = Some(opened),
                    Err(err) => self.app.notice(format!("cannot append to that session: {err:#}")),
                }
                self.persisted = self.messages.len();
                self.app.notice(format!(
                    "resumed {} ({} messages)",
                    meta.id,
                    self.messages.len() - 1
                ));
            }
            Err(err) => self.app.notice(format!("resume failed: {err:#}")),
        }
    }

    fn panel_effect(&mut self, effect: settings::Effect) {
        match effect {
            settings::Effect::Close => {
                self.app.mode = Mode::Chat;
                self.rebuild();
                self.app.notice("settings saved");
            }
            settings::Effect::Reload => {
                self.rebuild();
                let tools = self.panel_data().tools;
                if let Mode::Panel(panel) = &mut self.app.mode {
                    panel.data.tools = tools;
                }
            }
            settings::Effect::Test(name) => {
                let Ok(layers) = Layers::load() else {
                    if let Mode::Panel(panel) = &mut self.app.mode {
                        panel.set_test(&name, TestState::Failed("config could not be loaded".into()));
                    }
                    return;
                };
                let store = self.store.clone();
                let tx = self.ui_tx.clone();
                tokio::spawn(async move {
                    let result = ops::test_named(&layers, &store, &name).await;
                    let _ = tx.send(UiEvent::PanelTest { name, result }).await;
                });
            }
            settings::Effect::RestartMcp(name) => {
                self.rebuild();
                if let Some(config) = self.cfg().mcp.get(&name).cloned() {
                    self.start_mcp(name, config);
                }
            }
            settings::Effect::ListModels { provider, profile } => {
                let Some(spec) = self.cfg().providers.get(&provider).cloned() else {
                    return;
                };
                let tx = self.ui_tx.clone();
                tokio::spawn(async move {
                    let models = ops::list_models(&spec).await;
                    let _ = tx
                        .send(UiEvent::Models {
                            target: ModelTarget::Panel(profile),
                            models,
                        })
                        .await;
                });
            }
        }
    }

    fn onboard_effect(&mut self, effect: onboarding::Effect) {
        match effect {
            onboarding::Effect::Test => {
                let Mode::Onboarding(flow) = &self.app.mode else {
                    return;
                };
                let spec = flow.spec();
                let model = flow.model.clone();
                let tx = self.ui_tx.clone();
                tokio::spawn(async move {
                    let result = ops::test_provider(&spec, &model).await;
                    drop(spec);
                    let _ = tx.send(UiEvent::OnboardTest(result)).await;
                });
            }
            onboarding::Effect::ListModels => {
                let Mode::Onboarding(flow) = &self.app.mode else {
                    return;
                };
                let spec = flow.spec();
                let tx = self.ui_tx.clone();
                tokio::spawn(async move {
                    let models = ops::list_models(&spec).await;
                    let _ = tx
                        .send(UiEvent::Models {
                            target: ModelTarget::Onboarding,
                            models,
                        })
                        .await;
                });
            }
            onboarding::Effect::Done => {
                self.app.mode = Mode::Chat;
                self.rebuild();
                self.app.notice("setup complete. Type a message, or / for commands.");
            }
            onboarding::Effect::Skipped => {
                self.app.mode = Mode::Chat;
                self.rebuild();
                self.app.notice(
                    "setup skipped. Nothing can be sent until a provider works: run /login, /provider add or /config.",
                );
            }
        }
    }

    fn picked(&mut self, purpose: ChatPick, value: String) {
        match purpose {
            ChatPick::Model => self.set_model(&value),
            ChatPick::Theme => self.set_theme(&value),
            ChatPick::SwitchProvider => self.switch_provider(&value),
            ChatPick::Resume => self.resume(Path::new(&value)),
            ChatPick::Login => self.ask_login(&value),
            ChatPick::Logout => self.logout(&value),
        }
    }

    fn ask_login(&mut self, provider: &str) {
        if !catalog::valid_name(provider) {
            self.app.notice(format!("'{provider}' is not a valid provider name"));
            return;
        }
        self.app.overlay = Some(Overlay::Secret {
            input: MaskedInput::new(),
            provider: provider.to_string(),
        });
    }

    fn logout(&mut self, provider: &str) {
        match ops::logout(&self.store, provider) {
            Ok(true) => {
                self.app.notice(format!("removed the stored key for {provider}"));
                self.rebuild();
            }
            Ok(false) => self.app.notice(format!("no stored key for {provider}")),
            Err(err) => self.app.notice(format!("logout failed: {err:#}")),
        }
        if let Some(spec) = self.cfg().providers.get(provider)
            && let Some(var) = &spec.api_key_env
            && std::env::var(var).is_ok()
        {
            self.app.notice(format!(
                "{provider} still reads a key from ${var}; unset it in your shell to stop using it"
            ));
        }
    }

    fn set_model(&mut self, model: &str) {
        let profile = self.profile();
        let result = match self.layers() {
            Some(layers) => ops::set_profile_model(layers, &profile, model),
            None => return,
        };
        match result {
            Ok(()) => {
                self.options.model = None;
                self.rebuild();
                self.app.notice(format!("model for profile {profile} is now {model}"));
            }
            Err(err) => self.app.notice(format!("{err:#}")),
        }
    }

    fn set_theme(&mut self, color: &str) {
        if !brand::ACCENTS.iter().any(|(n, _)| *n == color) && brand::parse_hex(color).is_none() {
            self.app.notice(format!(
                "'{color}' is not a color; pick one of {} or #rrggbb",
                brand::ACCENTS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
            ));
            return;
        }
        let result = match self.layers() {
            Some(layers) => ops::set_general(layers, GeneralField::Accent, |g| {
                g.accent = Some(color.to_string())
            }),
            None => return,
        };
        match result {
            Ok(()) => {
                self.rebuild();
                self.app.notice(format!("accent color is now {color}"));
            }
            Err(err) => self.app.notice(format!("{err:#}")),
        }
    }

    fn switch_provider(&mut self, provider: &str) {
        let result = match self.layers() {
            Some(layers) => ops::switch_provider(layers, provider),
            None => return,
        };
        match result {
            Ok(profile) => {
                self.options.model = None;
                self.rebuild();
                let model = self
                    .cfg()
                    .profiles
                    .get(&profile)
                    .and_then(|p| p.model.clone())
                    .unwrap_or_else(|| "none".into());
                self.app.notice(format!(
                    "profile {profile} now uses {provider} with model {model}"
                ));
            }
            Err(err) => self.app.notice(format!("{err:#}")),
        }
    }

    fn provider_picker(&self, title: &str, with_kinds: bool, only_with_keys: bool) -> Picker {
        let layers = self.layers.clone().unwrap_or_else(|| Layers {
            project_path: paths::project_config(),
            user_path: paths::user_config(),
            project: Config::default(),
            user: Config::default(),
        });
        let rows = ops::providers(&layers, &self.store);
        let mut choices: Vec<Choice> = rows
            .iter()
            .filter(|r| !only_with_keys || matches!(r.key, Some(zarn_config::auth::KeySource::Store | zarn_config::auth::KeySource::File)))
            .map(|r| Choice::new(r.name.clone(), r.name.clone()).with_detail(format!("{} · {}", r.kind, r.key_label())))
            .collect();
        if with_kinds {
            for kind in catalog::KINDS {
                if catalog::kind(kind.id).is_some_and(|k| k.needs_key)
                    && !rows.iter().any(|r| r.name == kind.id)
                {
                    choices.push(Choice::new(kind.id, kind.id).with_detail(format!("new · {}", kind.label)));
                }
            }
        }
        Picker::new(title, choices)
    }

    fn model_picker(&mut self) {
        let cfg = self.cfg();
        let profile = self.profile();
        let current = cfg.profiles.get(&profile).cloned().unwrap_or_default();
        let spec = current
            .provider
            .as_ref()
            .and_then(|p| cfg.providers.get(p))
            .cloned();
        let kind = spec.as_ref().map(|s| s.kind.clone()).unwrap_or_default();
        let mut choices: Vec<Choice> = catalog::suggested_models(&kind)
            .iter()
            .map(|m| Choice::plain(*m))
            .collect();
        if let Some(model) = &current.model
            && !choices.iter().any(|c| &c.value == model)
        {
            choices.insert(0, Choice::plain(model.clone()));
        }
        let picker = Picker::new(format!("Model for profile {profile}"), choices)
            .custom("type any model name, then enter")
            .select_value(current.model.as_deref().unwrap_or_default());
        self.app.overlay = Some(Overlay::Pick(picker, ChatPick::Model));
        if let Some(spec) = spec
            && spec.kind == "ollama"
        {
            let tx = self.ui_tx.clone();
            tokio::spawn(async move {
                let models = ops::list_models(&spec).await;
                let _ = tx.send(UiEvent::Models { target: ModelTarget::Chat, models }).await;
            });
        }
    }

    fn command(&mut self, line: &str) -> bool {
        let Some((name, args)) = popup::split(line) else {
            return true;
        };
        let words = settings::split_words(&args);
        let first = words.first().map(String::as_str);
        match name.as_str() {
            "quit" | "exit" => return false,
            "help" => {
                let width = self.app.entries.iter().map(|e| e.name.len() + e.usage.len()).max().unwrap_or(0) + 3;
                let text = self
                    .app
                    .entries
                    .iter()
                    .map(|e| {
                        let usage = format!("/{} {}", e.name, e.usage);
                        format!("{usage:<width$} {}", e.description)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.app.notice(text);
            }
            "clear" => {
                self.messages.truncate(1);
                self.app.blocks.clear();
                self.session = None;
                self.persisted = 1;
                self.app.notice("conversation cleared");
            }
            "compact" => {
                if self.app.streaming {
                    self.app.notice("wait for the current turn to finish");
                    return true;
                }
                self.start_compaction(false);
            }
            "context" => {
                let Some(agent) = self.agent.clone() else {
                    self.app.notice("no provider is ready");
                    return true;
                };
                let parts = self.runtime.as_ref().map(|rt| rt.parts.clone()).unwrap_or_default();
                let tools = self.runtime.as_ref().map(|rt| rt.tools.specs()).unwrap_or_default();
                let provider = agent.provider().clone();
                let count = move |m: &[Message]| provider.count_tokens(m).unwrap_or(0);
                let rows = context::breakdown(&count, &parts, &tools, &self.messages);
                let window = agent.provider().caps().context_window;
                self.app.notice(context::render_breakdown(&rows, window));
            }
            "cost" => {
                let report = self.cost.report();
                self.app.notice(if report.trim().is_empty() {
                    "no usage yet".to_string()
                } else {
                    report
                });
            }
            "tools" => {
                let names = self
                    .runtime
                    .as_ref()
                    .map(|rt| rt.tools.names())
                    .unwrap_or_default();
                self.app.notice(if names.is_empty() {
                    "no tools: no provider is ready".to_string()
                } else {
                    names.join("\n")
                });
            }
            "resume" => match first {
                Some(id) => match session::find(&session::data_root(), &self.cwd, id) {
                    Some(path) => self.resume(&path),
                    None => self.app.notice(format!("no session matching '{id}' in this directory")),
                },
                None => {
                    let choices: Vec<Choice> = session::list(&session::data_root(), &self.cwd)
                        .into_iter()
                        .take(30)
                        .filter_map(|path| {
                            let (meta, messages) = session::read(&path).ok()?;
                            let first = messages
                                .iter()
                                .find(|m| matches!(m, Message::User(_)))
                                .map(|m| m.text())
                                .unwrap_or_default();
                            let short: String = meta.id.chars().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect();
                            Some(
                                Choice::new(path.display().to_string(), format!("{short}  {}", first.replace('\n', " ")))
                                    .with_detail(format!("{} messages", messages.len())),
                            )
                        })
                        .collect();
                    if choices.is_empty() {
                        self.app.notice("no earlier sessions in this directory");
                    } else {
                        self.app.overlay = Some(Overlay::Pick(Picker::new("Resume a session", choices), ChatPick::Resume));
                    }
                }
            },
            "login" => match first {
                Some(provider) => self.ask_login(provider),
                None => {
                    let picker = self.provider_picker("Log in to which provider?", true, false).custom("or type a provider name");
                    self.app.overlay = Some(Overlay::Pick(picker, ChatPick::Login));
                }
            },
            "logout" => match first {
                Some(provider) => self.logout(provider),
                None => {
                    let picker = self.provider_picker("Remove the key of which provider?", false, true);
                    if picker.choices.is_empty() {
                        self.app.notice("no keys are stored");
                    } else {
                        self.app.overlay = Some(Overlay::Pick(picker, ChatPick::Logout));
                    }
                }
            },
            "provider" => match first {
                None | Some("list") => {
                    let rows = self.layers.as_ref().map(|l| ops::providers(l, &self.store)).unwrap_or_default();
                    if rows.is_empty() {
                        self.app.notice("no providers; /provider add or /login <kind>");
                    } else {
                        let text = rows
                            .iter()
                            .map(|r| {
                                let mark = if r.is_default { "*" } else { " " };
                                format!("{mark} {:<14} {:<14} {}", r.name, r.kind, r.key_label())
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        self.app.notice(format!("{text}\n* default · /provider switch <name> · /provider add"));
                    }
                }
                Some("add") => {
                    if let Some(panel) = self.open_panel(Tab::Providers) {
                        panel.activate_kind(RowKind::AddProvider);
                    }
                }
                Some("switch") => match words.get(1) {
                    Some(name) => self.switch_provider(name),
                    None => {
                        let picker = self.provider_picker("Switch to which provider?", false, false);
                        self.app.overlay = Some(Overlay::Pick(picker, ChatPick::SwitchProvider));
                    }
                },
                Some(other) => self.app.notice(format!("unknown: /provider {other}; use list, add or switch")),
            },
            "model" => match args.trim() {
                "" => self.model_picker(),
                model => self.set_model(model),
            },
            "config" => {
                self.open_panel(Tab::Providers);
            }
            "skills" => match first {
                Some("list") => {
                    let cfg = self.cfg();
                    let skills = context::discover_skills();
                    self.app.notice(if skills.is_empty() {
                        "no skills in ~/.config/zarn/skills or ./.zarn/skills".to_string()
                    } else {
                        skills
                            .iter()
                            .map(|s| {
                                let state = if cfg.skill_enabled(&s.name) { "on " } else { "off" };
                                format!("[{state}] {:<16} {:<8} {}", s.name, s.source.label(), s.description)
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    });
                }
                _ => {
                    self.open_panel(Tab::Skills);
                }
            },
            "permissions" => {
                self.open_panel(Tab::Permissions);
            }
            "mcp" => self.mcp_command(&words),
            "agents" => match first {
                Some("list") => {
                    let found = zarn_config::agents::load();
                    self.app.notice(if found.is_empty() {
                        "no agents; /agents create".to_string()
                    } else {
                        found
                            .iter()
                            .map(|a| format!("{:<16} {:<8} {}", a.def.name, a.scope.label(), a.def.description))
                            .collect::<Vec<_>>()
                            .join("\n")
                    });
                }
                Some("create") | Some("new") => {
                    if let Some(panel) = self.open_panel(Tab::Agents) {
                        panel.activate_kind(RowKind::AddAgent);
                    }
                }
                Some("edit") => {
                    let wanted = words.get(1).cloned().unwrap_or_default();
                    if let Some(panel) = self.open_panel(Tab::Agents) {
                        match panel.agents.iter().position(|a| a.def.name == wanted) {
                            Some(i) => {
                                panel.activate_kind(RowKind::Agent(i));
                            }
                            None if wanted.is_empty() => {}
                            None => panel.message = Some((format!("no agent named {wanted}"), true)),
                        }
                    }
                }
                _ => {
                    self.open_panel(Tab::Agents);
                }
            },
            "init" => {
                if self.agent.is_none() {
                    self.app.notice("cannot run /init: no provider is ready");
                } else {
                    self.submit(INIT_PROMPT.to_string());
                }
            }
            "doctor" => {
                self.app.notice("running checks…");
                let store = self.store.clone();
                let tx = self.ui_tx.clone();
                let mut checks = terminal::checks(Some(self.keyboard));
                tokio::spawn(async move {
                    let mut all = doctor::run(&store, paths::project_config(), paths::user_config(), true).await;
                    all.append(&mut checks);
                    let _ = tx.send(UiEvent::Doctor(doctor::render(&all))).await;
                });
            }
            "status" => {
                let cfg = self.cfg();
                let profile = self.profile();
                let provider = self
                    .runtime
                    .as_ref()
                    .map(|rt| rt.plan.provider_name.clone())
                    .or_else(|| cfg.profiles.get(&profile).and_then(|p| p.provider.clone()))
                    .unwrap_or_else(|| "none".into());
                let session = self
                    .session
                    .as_ref()
                    .map(|s| s.meta().id.clone())
                    .unwrap_or_else(|| "not started (created on the first reply)".into());
                let mut lines = vec![
                    format!("profile   {profile}"),
                    format!("provider  {provider}"),
                    format!("model     {}", if self.app.status.model.is_empty() { "none" } else { &self.app.status.model }),
                    format!("cwd       {}", self.cwd.display()),
                    format!("session   {session}"),
                ];
                if let Some(reason) = &self.app.not_ready {
                    lines.push(format!("not ready {reason}"));
                }
                self.app.notice(lines.join("\n"));
            }
            "theme" => match first {
                Some(color) => self.set_theme(color),
                None => {
                    let cfg = self.cfg();
                    let choices = brand::ACCENTS.iter().map(|(n, _)| Choice::plain(*n)).collect();
                    let picker = Picker::new("Accent color", choices)
                        .custom("or type a hex color like #ff8800")
                        .select_value(cfg.general.accent.as_deref().unwrap_or("violet"));
                    self.app.overlay = Some(Overlay::Pick(picker, ChatPick::Theme));
                }
            },
            other => {
                match self.commands.parse(line) {
                    Some(CoreCommand::Prompt(body)) => {
                        self.submit(body);
                        return true;
                    }
                    Some(CoreCommand::Unknown(_)) | None => {}
                    Some(_) => {}
                }
                let cfg = self.cfg();
                let skill = context::discover_skills()
                    .into_iter()
                    .find(|s| s.name == other && cfg.skill_enabled(&s.name));
                match skill {
                    Some(skill) => match zarn_tools::skill::render(&skill, &args) {
                        Ok(text) => self.submit(format!(
                            "Use the {other} skill for this request. Its instructions follow.\n\n{text}"
                        )),
                        Err(err) => self.app.notice(format!("cannot read skill {other}: {err}")),
                    },
                    None => self.app.notice(format!("unknown command /{other}; type / to see them all")),
                }
            }
        }
        true
    }

    fn mcp_command(&mut self, words: &[String]) {
        match words.first().map(String::as_str) {
            None => {
                self.open_panel(Tab::Mcp);
            }
            Some("list") => {
                let cfg = self.cfg();
                if cfg.mcp.is_empty() {
                    self.app.notice("no MCP servers; /mcp add <name> <command or url>");
                    return;
                }
                let text = cfg
                    .mcp
                    .keys()
                    .map(|name| {
                        let status = self.mcp.get(name).map(|s| s.status.clone()).unwrap_or_else(|| "not started".into());
                        format!("{name:<16} {status}")
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.app.notice(text);
            }
            Some("add") => {
                let (Some(name), true) = (words.get(1), words.len() > 2) else {
                    self.app.notice("usage: /mcp add <name> <command> [args…] | <url>");
                    return;
                };
                let server = match ops::parse_server(&words[2..]) {
                    Ok(server) => server,
                    Err(err) => return self.app.notice(format!("{err:#}")),
                };
                let result = match self.layers() {
                    Some(layers) => ops::mcp_add(layers, name, server, None),
                    None => return,
                };
                match result {
                    Ok(scope) => {
                        self.app.notice(format!("MCP server {name} saved to the {} config; starting it", scope.label()));
                        self.rebuild();
                    }
                    Err(err) => self.app.notice(format!("{err:#}")),
                }
            }
            Some("remove") | Some("rm") => {
                let Some(name) = words.get(1) else {
                    self.app.notice("usage: /mcp remove <name>");
                    return;
                };
                let result = match self.layers() {
                    Some(layers) => ops::mcp_remove(layers, name),
                    None => return,
                };
                match result {
                    Ok(()) => {
                        self.app.notice(format!("MCP server {name} removed"));
                        self.rebuild();
                    }
                    Err(err) => self.app.notice(format!("{err:#}")),
                }
            }
            Some("restart") => {
                let Some(name) = words.get(1) else {
                    self.app.notice("usage: /mcp restart <name>");
                    return;
                };
                match self.cfg().mcp.get(name).cloned() {
                    Some(config) => {
                        self.start_mcp(name.clone(), config);
                        self.app.notice(format!("restarting {name}…"));
                    }
                    None => self.app.notice(format!("no MCP server named {name}")),
                }
            }
            Some(other) => self.app.notice(format!("unknown: /mcp {other}; use list, add, remove or restart")),
        }
    }
}

fn spawn_turn(
    agent: Arc<Agent>,
    mut messages: Vec<Message>,
    ui: mpsc::Sender<UiEvent>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
        let forward = {
            let ui = ui.clone();
            tokio::spawn(async move {
                while let Some(event) = rx.recv().await {
                    if ui.send(UiEvent::Agent(event)).await.is_err() {
                        return;
                    }
                }
            })
        };

        let result = agent
            .run(&mut messages, &tx, &cancel)
            .await
            .map_err(|e| zarn_config::auth::redact(&e.to_string()));

        drop(tx);
        let _ = forward.await;
        let _ = ui.send(UiEvent::TurnDone { messages, result }).await;
    });
}
