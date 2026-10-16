use crate::brand;
use crate::onboarding::{self, Onboarding};
use crate::popup;
use crate::render;
use crate::settings::{self, Panel, PanelData, QuotaState, RowKind, SkillRow, Tab, TestState};
use crate::state::{Action, App, Block, ChatPick, ConfirmFor, Mode, Overlay, PermissionRequest, PlanReview};
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
use zarn_config::modes::{self, Loaded as Modes};
use zarn_config::{Config, catalog, context, doctor, guard, ops, paths};
use zarn_tools::exit_plan::{PlanApprover, PlanVerdict};
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
        let request = PermissionRequest::new(tool, risk, args.clone(), reply);
        if self.tx.send(request).await.is_err() {
            return Answer::No;
        }
        answer.await.unwrap_or(Answer::No)
    }
}

type PlanAsk = (String, oneshot::Sender<PlanVerdict>);

pub struct TuiApprover {
    tx: mpsc::Sender<PlanAsk>,
}

#[async_trait]
impl PlanApprover for TuiApprover {
    async fn review(&self, plan: &str) -> PlanVerdict {
        let (reply, answer) = oneshot::channel();
        if self.tx.send((plan.to_string(), reply)).await.is_err() {
            return PlanVerdict::Reject("the plan could not be shown".into());
        }
        answer
            .await
            .unwrap_or_else(|_| PlanVerdict::Reject("the plan was dismissed".into()))
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
    pub mode: Option<String>,
    pub fork: bool,
    pub add_dirs: Vec<PathBuf>,
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
        models: Vec<(String, Option<u32>)>,
    },
    Mcp {
        name: String,
        config: ServerConfig,
        result: Result<Box<McpServer>, String>,
    },
    Doctor(String),
    Notice(String),
    WebKeys(zarn_config::webkeys::Change),
    LoginDone(Result<String, String>),
    Title(Result<String, String>),
    Quota {
        reports: Vec<zarn_config::quota::Report>,
        chat: bool,
    },
    Compared(Vec<crate::state::CompareSide>),
    Prompted {
        text: String,
        blocked: Option<String>,
    },
    HookFailed(String),
    Shell {
        id: String,
        command: String,
        output: zarn_core::tools::ToolOutput,
    },
    Window { model: String, window: u32 },
    Task(zarn_core::subagent::TaskUpdate),
    Expanded { result: Result<String, String>, model: Option<String> },
    McpChanged(String),
    CommitDraft(Result<String, String>),
    Committed(Result<String, String>),
    PrDraft(Result<(String, String), String>),
    McpLoggedIn { name: String, result: Result<(), String> },
    McpRefreshed { name: String, result: Result<usize, String> },
    McpPrompt(Result<String, String>),
    Resolved { text: String, blocks: Vec<String>, notes: Vec<String> },
    Progress { command: String, line: String },
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
        EnableBracketedPaste,
        crossterm::event::EnableFocusChange
    );

    let outcome = match Controller::new(options) {
        Ok(mut controller) => {
            controller.interactive = true;
            controller.event_loop(&mut terminal).await
        }
        Err(err) => Err(err),
    };

    let _ = crossterm::execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        crossterm::event::DisableFocusChange,
        crossterm::event::DisableMouseCapture,
        PopKeyboardEnhancementFlags
    );
    ratatui::restore();
    outcome
}

type GitContext = (Arc<dyn zarn_core::provider::Provider>, String, Arc<dyn zarn_core::agent::ToolGate>);

struct LoopJob {
    id: usize,
    every: std::time::Duration,
    prompt: String,
    next: std::time::Instant,
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
    modes: Modes,
    mode: String,
    approver: Arc<TuiApprover>,
    plans: mpsc::Receiver<PlanAsk>,
    warned: std::collections::BTreeSet<String>,
    titled: bool,
    title: Option<String>,
    ledger: zarn_config::spend::Ledger,
    day_base: f64,
    session_base: f64,
    cap_notes: std::collections::BTreeSet<String>,
    cap_stopped: bool,
    parse_failures: u32,
    window: Option<(String, u32)>,
    hooks: Arc<std::sync::RwLock<zarn_config::hooks::Hooks>>,
    checkpoints: Arc<zarn_tools::checkpoint::Checkpoints>,
    checkpoint_notes: Arc<std::sync::Mutex<Vec<String>>>,
    pending_context: Vec<String>,
    last_shell: Option<(String, String)>,
    shell_runs: usize,
    interactive: bool,
    editor_pending: bool,
    todos: zarn_tools::todo::TodoList,
    notifier: crate::notify::Notifier,
    shells: Arc<zarn_tools::shells::Shells>,
    background: Arc<zarn_core::subagent::Background>,
    waiting: std::collections::VecDeque<PermissionRequest>,
    turn_model: Option<String>,
    loops: Vec<LoopJob>,
    loop_count: usize,
    memory: Arc<context::NestedMemory>,
    mouse: bool,
    mouse_on: bool,
    web: Option<zarn_config::webkeys::Running>,
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
        let cwd_for_checkpoints = cwd.clone();
        let cwd_for_memory = cwd.clone();
        let (prompt, permissions) = permission_channel();
        let gate = Arc::new(Permissions::new(prompt));
        let (ui_tx, ui_rx) = mpsc::channel::<UiEvent>(256);
        let commands = load_commands();
        let (plan_tx, plans) = mpsc::channel::<PlanAsk>(2);

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
            modes: Modes::default(),
            mode: "default".into(),
            approver: Arc::new(TuiApprover { tx: plan_tx }),
            plans,
            warned: Default::default(),
            titled: false,
            title: None,
            ledger: zarn_config::spend::Ledger::system(),
            day_base: 0.0,
            session_base: 0.0,
            cap_notes: Default::default(),
            cap_stopped: false,
            parse_failures: 0,
            window: None,
            hooks: Arc::default(),
            checkpoints: Arc::new(zarn_tools::checkpoint::Checkpoints::new(
                paths::data_dir().join("checkpoints").join(format!(
                    "{}-{}",
                    std::process::id(),
                    zarn_config::quota::now()
                )),
                cwd_for_checkpoints,
            )),
            checkpoint_notes: Arc::default(),
            pending_context: Vec::new(),
            last_shell: None,
            shell_runs: 0,
            interactive: false,
            editor_pending: false,
            todos: Arc::default(),
            notifier: crate::notify::Notifier::stdout(),
            shells: zarn_tools::shells::Shells::new(),
            background: zarn_core::subagent::Background::new(),
            waiting: std::collections::VecDeque::new(),
            turn_model: None,
            loops: Vec::new(),
            loop_count: 0,
            memory: context::NestedMemory::new(&cwd_for_memory),
            mouse: false,
            mouse_on: false,
            web: None,
        };

        let progress_tx = controller.ui_tx.clone();
        zarn_tools::progress::register(
            &controller.ctx,
            Arc::new(move |command: &str, line: &str| {
                let _ = progress_tx.try_send(UiEvent::Progress {
                    command: command.to_string(),
                    line: zarn_config::auth::redact(line),
                });
            }),
        );
        if let Some(path) = controller.options.resume.clone() {
            controller.resume(&path);
        }
        controller.rebuild();
        controller.startup_mode();

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
        let mode_profile = self.mode_def().and_then(|m| m.profile);
        runtime::active_profile(&self.cfg(), mode_profile.as_deref().or(self.options.profile.as_deref()))
    }

    fn mode_def(&self) -> Option<modes::ModeDef> {
        self.modes.find(&self.mode).map(|m| m.def.clone())
    }

    fn startup_mode(&mut self) {
        let from_cli = self.options.mode.clone();
        let wanted = from_cli.clone().unwrap_or_else(|| self.cfg().general.mode());
        if wanted == "default" {
            return;
        }
        if wanted == "bypass" {
            if from_cli.is_some() {
                self.ask_bypass();
            } else {
                self.app.notice("general.mode is bypass, but bypass is never restored at startup; starting in default. /mode bypass enters it after a confirmation.");
            }
            return;
        }
        self.set_mode(&wanted, false);
    }

    fn ask_bypass(&mut self) {
        self.app.overlay = Some(Overlay::Confirm(
            format!("Enter bypass mode? {}", modes::BYPASS_RISK),
            ConfirmFor::Bypass,
        ));
    }

    fn set_mode(&mut self, name: &str, persist: bool) {
        self.modes = modes::load();
        let Some(file) = self.modes.find(name).cloned() else {
            self.app.notice(format!(
                "no mode named {name}; modes: {}",
                self.modes.names().join(", ")
            ));
            return;
        };
        self.mode = name.to_string();
        self.app.status.mode = name.to_string();
        self.gate.set_overlay(modes::overlay(&file.def));
        if persist && name != "bypass" {
            let result = match self.layers() {
                Some(layers) => ops::set_general(layers, GeneralField::Mode, |g| {
                    g.mode = Some(name.to_string())
                }),
                None => Ok(()),
            };
            if let Err(err) = result {
                self.app.notice(format!("mode not saved: {err:#}"));
            }
        }
        self.rebuild();
        self.app.notice(format!("{name} mode: {}", file.def.description));
    }

    fn use_profile(&mut self, name: &str) {
        if !self.cfg().profiles.contains_key(name) {
            self.app.notice(format!("no profile named {name}"));
            return;
        }
        self.options.profile = Some(name.to_string());
        self.options.model = None;
        self.rebuild();
        self.app.notice(format!(
            "using profile {name} for this session; /config sets the default profile"
        ));
    }

    fn profile_picker(&mut self) {
        let cfg = self.cfg();
        let current = self.profile();
        let choices: Vec<Choice> = cfg
            .profiles
            .iter()
            .map(|(name, p)| {
                Choice::new(name.clone(), name.clone()).with_detail(format!(
                    "{} · {}",
                    p.provider.as_deref().unwrap_or("no provider"),
                    p.model.as_deref().unwrap_or("no model")
                ))
            })
            .collect();
        if choices.is_empty() {
            self.app.notice("no profiles yet; /config → Models adds one");
            return;
        }
        let picker = Picker::new("Profile for this session", choices).select_value(&current);
        self.app.overlay = Some(Overlay::Pick(picker, ChatPick::Profile));
    }

    fn compare(&mut self, words: &[String]) {
        let (Some(a), Some(b)) = (words.first(), words.get(1)) else {
            self.app.notice("usage: /compare <provider-a> <provider-b> <prompt>");
            return;
        };
        let prompt = words[2..].join(" ");
        if prompt.trim().is_empty() {
            self.app.notice("usage: /compare <provider-a> <provider-b> <prompt>");
            return;
        }
        let Some(rt) = self.runtime.as_ref() else {
            self.app.notice("cannot compare: no provider is ready");
            return;
        };
        let cfg = self.cfg();
        let mut jobs = Vec::new();
        for name in [a, b] {
            let label = name.clone();
            let model = ops::model_for(&cfg, name).unwrap_or_default();
            let provider = if cfg.providers.contains_key(name) {
                rt.router.providers.chain(std::slice::from_ref(name))
            } else {
                Err(format!("no provider named {name}"))
            };
            jobs.push((label, model, provider));
        }
        self.app.notice(format!("comparing {a} and {b}…"));
        let tx = self.ui_tx.clone();
        tokio::spawn(async move {
            let runs = jobs.into_iter().map(|(label, model, provider)| {
                let prompt = prompt.clone();
                async move { compare_one(label, model, provider, prompt).await }
            });
            let sides = futures::future::join_all(runs).await;
            let _ = tx.send(UiEvent::Compared(sides)).await;
        });
    }

    fn note_tool_call(&mut self, args: &Value) {
        if args.is_object() {
            self.parse_failures = 0;
            return;
        }
        self.parse_failures += 1;
        if self.parse_failures < 3 || self.app.overlay.is_some() {
            return;
        }
        let Some(rt) = self.runtime.as_ref() else {
            return;
        };
        let name = rt.plan.provider_name.clone();
        let native = self
            .cfg()
            .providers
            .get(&name)
            .is_some_and(|spec| {
                let default = if spec.kind == "ollama" { "prompted" } else { "native" };
                spec.tools.as_deref().unwrap_or(default) == "native"
            });
        if !native {
            return;
        }
        self.app.overlay = Some(Overlay::Confirm(
            format!(
                "{name}'s tool calls failed to parse 3 times in a row. Switch it to prompted tool calls (tools = \"prompted\")? Local models often handle that better."
            ),
            ConfirmFor::Prompted(name),
        ));
    }

    fn probe_window(&mut self) {
        let Some(rt) = self.runtime.as_ref() else {
            return;
        };
        let Some(spec) = rt.plan.specs.first().cloned() else {
            return;
        };
        if spec.kind != "ollama" || spec.context_window.is_some() {
            self.window = None;
            return;
        }
        let model = rt.model.clone();
        if self.window.as_ref().is_some_and(|(m, _)| *m == model) {
            return;
        }
        let base = zarn_providers::discovery::base_url(&spec);
        let tx = self.ui_tx.clone();
        tokio::spawn(async move {
            let probe = zarn_providers::OllamaProvider::new(base);
            if let Ok(Ok(Some(window))) =
                tokio::time::timeout(std::time::Duration::from_secs(5), probe.context_window(&model)).await
            {
                let _ = tx.send(UiEvent::Window { model, window }).await;
            }
        });
    }

    fn context_window(&self) -> Option<u32> {
        let rt = self.runtime.as_ref()?;
        match &self.window {
            Some((model, window)) if *model == rt.model => Some(*window),
            _ => Some(rt.provider.caps().context_window),
        }
    }

    fn cycle_mode(&mut self) {
        self.modes = modes::load();
        let next = self.modes.next_after(&self.mode);
        self.set_mode(&next, true);
    }

    fn decide_plan(&mut self, verdict: PlanVerdict) {
        let Some(mut review) = self.app.plan_review.take() else {
            return;
        };
        if let PlanVerdict::Approve(mode) = &verdict {
            self.set_mode(mode, true);
        } else {
            self.app.notice("plan rejected; staying in plan mode");
        }
        if let Some(reply) = review.reply.take() {
            let _ = reply.send(verdict);
        }
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
        self.app.keymap = crate::keymap::Keymap::load();
        match (cfg.general.vim(), self.app.vim.is_some()) {
            (true, false) => self.app.vim = Some(crate::state::Vim::default()),
            (false, true) => self.app.vim = None,
            _ => {}
        }
        self.app.files = crate::mentions::list_files(&self.cwd);
        self.fingerprint = context::fingerprint(&self.cwd);
        self.cost.set_table(zarn_config::pricing::table(&cfg));
        self.app.status.cost = self.cost.total().usd;
        self.app.theme.accent = brand::accent(cfg.general.accent.as_deref());
        self.app.theme.banner = cfg.general.banner();
        self.app.theme.show_cost = cfg.general.show_cost();
        let colorfgbg = std::env::var("COLORFGBG").ok();
        match crate::theme::resolve(cfg.general.theme.as_deref(), colorfgbg.as_deref()) {
            Ok(palette) => self.app.theme.palette = palette,
            Err(err) => self.app.notice(format!("theme: {err}")),
        }
        match cfg.general.status_line.as_deref().map(crate::status_line::Template::parse) {
            None => self.app.theme.status_line = None,
            Some(Ok(template)) => self.app.theme.status_line = Some(template),
            Some(Err(err)) => {
                let kept = if self.app.theme.status_line.is_some() { "keeping the previous one" } else { "using the default" };
                self.app.notice(format!("status_line: {err}; {kept}"));
            }
        }
        self.notifier.enabled = cfg.general.notifications();
        self.mouse = cfg.general.mouse();
        self.app.status.cwd = crate::status_line::short_path(&self.cwd, std::env::home_dir().as_deref());
        self.app.status.branch = crate::status_line::branch(&self.cwd);
        if let Ok(mut hooks) = self.hooks.write() {
            *hooks = cfg.hooks.clone();
        }
        for err in cfg.apply_permissions(&self.gate, &self.cwd) {
            self.app.notice(format!("permission rule ignored: {err}"));
        }
        self.gate.set_boundary(Some(self.options.add_dirs.clone()));
        self.sync_mcp(&cfg);
        self.refresh_entries(&cfg);

        if !cfg.has_provider() {
            self.agent = None;
            self.runtime = None;
            self.app.not_ready = Some("no provider; run /login or /config".into());
            return;
        }

        let profile = self.profile();
        self.app.status.profile = profile.clone();
        self.modes = modes::load();
        for warning in self.modes.warnings.clone() {
            if self.warned.insert(warning.clone()) {
                self.app.notice(warning);
            }
        }
        if self.modes.find(&self.mode).is_none() {
            self.mode = "default".into();
        }
        self.app.status.mode = self.mode.clone();
        let mode = self.mode_def();
        if let Some(def) = &mode {
            self.gate.set_overlay(modes::overlay(def));
        }
        let model_override = mode
            .as_ref()
            .and_then(|m| m.model.clone())
            .or_else(|| self.options.model.clone());
        let plan = match runtime::plan(&cfg, &self.store, &profile, model_override.as_deref()) {
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
            mode,
            approver: Some(self.approver.clone() as Arc<dyn PlanApprover>),
            todos: Some(self.todos.clone()),
            shells: Some(self.shells.clone()),
            task_gate: Some(self.hooked_gate()),
            task_monitor: Some({
                let tx = self.ui_tx.clone();
                Arc::new(move |update: zarn_core::subagent::TaskUpdate| {
                    let _ = tx.try_send(UiEvent::Task(update));
                })
            }),
            background: Some(self.background.clone()),
            memory: Some(self.memory.clone()),
        };
        match runtime::assemble(&cfg, plan, &self.cwd, self.ctx.clone(), tools, assembly) {
            Ok(rt) => {
                if self.app.streaming
                    && let Some(old) = &self.runtime
                    && (old.plan.provider_name != rt.plan.provider_name || old.model != rt.model)
                {
                    self.app.notice(format!(
                        "the current turn finishes on {} ({}); the next turn uses {} ({})",
                        old.plan.provider_name, old.model, rt.plan.provider_name, rt.model
                    ));
                }
                for warning in &rt.warnings {
                    if self.warned.insert(warning.clone()) {
                        self.app.notice(warning.clone());
                    }
                }
                let agent = rt.agent(self.ctx.clone(), self.hooked_gate());
                self.agent = Some(Arc::new(agent));
                self.system = rt.system.clone();
                self.set_system();
                self.app.status.model = rt.model.clone();
                self.cost.set_model(rt.model.clone());
                self.app.not_ready = None;
                self.runtime = Some(rt);
                self.app.status.cost = self.cost.total().usd;
                self.probe_window();
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
        self.commands = load_commands();
        let custom: Vec<(String, String, String)> = self
            .commands
            .custom()
            .into_iter()
            .filter(|c| !zarn_core::commands::BUILTINS.iter().any(|(b, _)| b.trim_start_matches('/') == c.name))
            .map(|c| (c.name.clone(), c.argument_hint.clone(), c.description.clone()))
            .chain(self.mcp.values().filter_map(|m| m.server.clone()).flat_map(|server| {
                server
                    .prompts()
                    .iter()
                    .map(|p| {
                        let usage: Vec<String> = p
                            .arguments
                            .iter()
                            .map(|a| if a.required { format!("<{}>", a.name) } else { format!("[{}]", a.name) })
                            .collect();
                        let description = if p.description.is_empty() { "MCP prompt".to_string() } else { format!("MCP prompt: {}", p.description) };
                        (format!("{}:{}", server.name(), p.name), usage.join(" "), description)
                    })
                    .collect::<Vec<_>>()
            }))
            .collect();
        for server in self.mcp.values().filter_map(|m| m.server.as_ref()) {
            for resource in server.resources() {
                let mention = format!("{}:{}", server.name(), resource.uri);
                if !self.app.files.contains(&mention) {
                    self.app.files.push(mention);
                }
            }
        }
        let skills: Vec<(String, String)> = context::discover_skills()
            .into_iter()
            .filter(|s| cfg.skill_enabled(&s.name))
            .map(|s| (s.name, s.description))
            .collect();
        self.app.entries = popup::entries(&custom, &skills);
        self.app.args = self.argument_values(cfg);
    }

    fn argument_values(&self, cfg: &Config) -> BTreeMap<String, Vec<(String, String)>> {
        let mut args: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        let loaded = modes::load();
        args.insert(
            "mode".into(),
            loaded.modes.iter().map(|m| (m.def.name.clone(), m.def.description.clone())).collect(),
        );
        let default_profile = cfg.default_profile();
        args.insert(
            "profile".into(),
            cfg.profiles
                .iter()
                .map(|(name, p)| {
                    let mut detail = format!(
                        "{} · {}",
                        p.provider.clone().unwrap_or_else(|| "no provider".into()),
                        p.model.clone().unwrap_or_else(|| "default model".into())
                    );
                    if *name == default_profile {
                        detail.push_str(" · default");
                    }
                    (name.clone(), detail)
                })
                .collect(),
        );
        let providers: Vec<(String, String)> = cfg
            .providers
            .iter()
            .map(|(name, spec)| (name.clone(), spec.kind.clone()))
            .collect();
        for key in ["provider switch", "logout", "compare"] {
            args.insert(key.into(), providers.clone());
        }
        let mut login = providers.clone();
        for kind in zarn_config::catalog::KINDS {
            if !login.iter().any(|(name, _)| name == kind.id) {
                login.push((kind.id.to_string(), kind.label.to_string()));
            }
        }
        args.insert("login".into(), login);
        let servers: Vec<(String, String)> = cfg
            .mcp
            .keys()
            .map(|name| {
                let state = self
                    .mcp
                    .get(name)
                    .map(|m| if m.server.is_some() { "running" } else { "not running" })
                    .unwrap_or("MCP server");
                (name.clone(), state.to_string())
            })
            .collect();
        for key in ["mcp remove", "mcp restart", "mcp login", "mcp logout"] {
            args.insert(key.into(), servers.clone());
        }
        args.insert(
            "agents edit".into(),
            zarn_config::agents::load()
                .into_iter()
                .map(|a| (a.def.name, a.def.description))
                .collect(),
        );
        args.insert(
            "theme".into(),
            brand::ACCENTS.iter().map(|(n, _)| (n.to_string(), "accent color".to_string())).collect(),
        );
        let profile = runtime::active_profile(cfg, self.options.profile.as_deref());
        if let Some(provider) = cfg.profiles.get(&profile).and_then(|p| p.provider.clone())
            && let Some(spec) = cfg.providers.get(&provider)
        {
            let mut models: Vec<(String, String)> = zarn_config::catalog::suggested_models(&spec.kind)
                .iter()
                .map(|m| (m.to_string(), format!("suggested for {provider}")))
                .collect();
            for model in zarn_config::quota::cached_models(&zarn_config::quota::cache_dir(), &provider) {
                if !models.iter().any(|(id, _)| *id == model.id) {
                    models.push((model.id, zarn_config::quota::context_label(model.context_window)));
                }
            }
            args.insert("model".into(), models);
        }
        args
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
        let store = self.store.clone();
        tokio::spawn(async move {
            let result = match zarn_config::mcp_auth::prepare(&store, &name, &config).await {
                Err(err) => Err(err),
                Ok(authed) => match tokio::time::timeout(MCP_START_TIMEOUT, McpServer::start(&name, &authed)).await {
                    Err(_) => Err("did not start within 30 seconds".to_string()),
                    Ok(Err(err)) => Err(zarn_config::auth::redact(&format!("{err:#}"))),
                    Ok(Ok(server)) => Ok(Box::new(server)),
                },
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
        let dir = zarn_config::quota::cache_dir();
        let quota = zarn_config::quota::eligible(&cfg, &self.store)
            .into_iter()
            .map(|(name, _)| {
                let state = match zarn_config::quota::load_cached(&dir, &name) {
                    Some(report) => QuotaState::Loaded(Box::new(report)),
                    None => QuotaState::Loading,
                };
                (name, state)
            })
            .collect();
        PanelData {
            skills,
            mcp_status: self.mcp_statuses(),
            tools,
            quota,
            tick: 0,
        }
    }

    fn fetch_quota(&self, only: Option<String>, refresh: bool, chat: bool) {
        let cfg = self.cfg();
        let store = self.store.clone();
        let tx = self.ui_tx.clone();
        tokio::spawn(async move {
            let dir = zarn_config::quota::cache_dir();
            let reports = zarn_config::quota::reports(&cfg, &store, only.as_deref(), refresh, &dir).await;
            let _ = tx.send(UiEvent::Quota { reports, chat }).await;
        });
    }

    fn panel_loading(&self) -> bool {
        matches!(&self.app.mode, Mode::Panel(panel) if panel.loading_quota())
    }

    fn open_panel(&mut self, tab: Tab) -> Option<&mut Panel> {
        let data = self.panel_data();
        let store = self.store.clone();
        let layers = self.layers()?.clone();
        self.app.overlay = None;
        let mode = self.mode.clone();
        let missing: Vec<String> = data
            .quota
            .iter()
            .filter(|(_, q)| **q == QuotaState::Loading)
            .map(|(n, _)| n.clone())
            .collect();
        for name in missing {
            self.fetch_quota(Some(name), false, false);
        }
        self.app.mode = Mode::Panel(Box::new(
            Panel::new(layers, store, data)
                .open_at(tab)
                .with_active_mode(mode)
                .with_status_values(render::status_values(&self.app)),
        ));
        match &mut self.app.mode {
            Mode::Panel(panel) => Some(panel),
            _ => None,
        }
    }

    async fn event_loop(mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        let mut ui_rx = self.ui_rx.take().expect("receiver");
        let mut keys = EventStream::new();
        let mut frames = tokio::time::interval(FRAME_INTERVAL);
        let mut loop_tick = tokio::time::interval(std::time::Duration::from_secs(1));

        loop {
            if self.mouse != self.mouse_on {
                self.mouse_on = self.mouse;
                let _ = if self.mouse {
                    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)
                } else {
                    crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture)
                };
            }
            terminal.draw(|frame| render::draw(frame, &self.app))?;

            tokio::select! {
                _ = loop_tick.tick(), if !self.loops.is_empty() => self.fire_loops(),
                _ = frames.tick(), if self.app.streaming || self.panel_loading() => {
                    self.app.advance_frame();
                    if let Mode::Panel(panel) = &mut self.app.mode {
                        panel.data.tick = panel.data.tick.wrapping_add(1);
                    }
                }
                event = keys.next() => {
                    let Some(Ok(event)) = event else { continue };
                    if !self.on_terminal(event) {
                        break;
                    }
                    if self.editor_pending {
                        self.editor_pending = false;
                        let _ = crossterm::terminal::disable_raw_mode();
                        let _ = crossterm::execute!(
                            std::io::stdout(),
                            crossterm::terminal::LeaveAlternateScreen,
                            crossterm::cursor::Show
                        );
                        self.open_editor();
                        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen);
                        let _ = crossterm::terminal::enable_raw_mode();
                        let _ = terminal.clear();
                        keys = EventStream::new();
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
                        self.show_permission(request);
                    }
                }
                ask = self.plans.recv() => {
                    if let Some((plan, reply)) = ask {
                        self.app.plan_review = Some(PlanReview::new(plan, reply));
                    }
                }
            }
        }

        self.shutdown().await;
        Ok(())
    }

    fn next_permission(&mut self) {
        while self.app.pending.is_none() {
            let Some(request) = self.waiting.pop_front() else { break };
            if request.reply.is_closed() {
                continue;
            }
            self.show_permission(request);
        }
    }

    fn on_terminal(&mut self, event: TermEvent) -> bool {
        let action = match event {
            TermEvent::Key(key) => self.app.on_key(key),
            TermEvent::Paste(text) => {
                self.app.paste(&text);
                None
            }
            TermEvent::Mouse(mouse) => self.app.on_mouse(mouse),
            TermEvent::FocusGained => {
                self.app.focused = Some(true);
                None
            }
            TermEvent::FocusLost => {
                self.app.focused = Some(false);
                None
            }
            _ => None,
        };
        let keep = match action {
            Some(action) => self.act(action),
            None => true,
        };
        self.next_permission();
        keep
    }

    async fn shutdown(&mut self) {
        if let Some(web) = self.web.take() {
            web.stop();
        }
        zarn_tools::progress::unregister(&self.ctx);
        self.shells.kill_all();
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

    pub fn set_notifier(&mut self, notifier: crate::notify::Notifier) {
        let enabled = self.notifier.enabled;
        self.notifier = notifier;
        self.notifier.enabled = enabled;
    }

    pub fn mouse_capture(&self) -> bool {
        self.mouse
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

    pub fn mouse(&mut self, event: crossterm::event::MouseEvent) -> bool {
        self.on_terminal(TermEvent::Mouse(event))
    }

    pub fn type_line(&mut self, text: &str) -> bool {
        self.app.set_input(text);
        self.key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ))
    }

    pub async fn pump(&mut self, wait: std::time::Duration) -> bool {
        if !self.loops.is_empty() {
            self.fire_loops();
        }
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
                    self.show_permission(request);
                    true
                }
                None => false,
            },
            ask = self.plans.recv() => match ask {
                Some((plan, reply)) => {
                    self.app.plan_review = Some(PlanReview::new(plan, reply));
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
            let waiting = self.app.pending.is_some() || self.app.plan_review.is_some();
            if !got && (!self.busy() || waiting) {
                break;
            }
        }
    }

    fn show_permission(&mut self, mut request: PermissionRequest) {
        if self.app.pending.is_some() {
            self.waiting.push_back(request);
            return;
        }
        request.pattern = zarn_tools::permissions::suggest(&request.tool, &request.args, &self.cwd);
        request.preview = zarn_tools::diff::preview(&request.tool, &request.args, &self.cwd);
        self.app.pending_expanded = false;
        self.notifier.permission(self.app.focused, &request.tool);
        self.app.pending = Some(request);
    }

    fn permission_summary(&self) -> String {
        let cfg = self.cfg();
        let mut out = Vec::new();
        let overlay = self.gate.overlay();
        let mode = self.mode_def();
        let mut mode_line = format!("mode     {}", self.mode);
        if overlay.allow_all {
            mode_line.push_str(" · every tool allowed except deny rules");
        } else if overlay.safe_only {
            mode_line.push_str(" · read-only tools and exit_plan only");
        }
        if !overlay.policies.is_empty() {
            let mut entries: Vec<String> = overlay
                .policies
                .iter()
                .map(|(t, p)| format!("{t}={}", zarn_tools::permissions::policy_word(*p)))
                .collect();
            entries.sort();
            mode_line.push_str(&format!(" · overlay {}", entries.join(", ")));
        }
        if let Some(def) = mode
            && !def.tools.is_empty()
        {
            mode_line.push_str(&format!(" · tools {}", def.tools.join(", ")));
        }
        out.push(mode_line);
        let rules = cfg.permissions.rules.list();
        let session = self.gate.session_patterns();
        if rules.is_empty() && session.is_empty() {
            out.push("rules    none; [a] or [p] in a prompt adds one".into());
        } else {
            out.push("rules".into());
            let layers = self.layers.clone();
            for (policy, rule) in rules {
                let scope = layers
                    .as_ref()
                    .map(|l| ops::rule_scope(l, &rule).label())
                    .unwrap_or("config");
                out.push(format!(
                    "  {:<5} {rule}  ({scope})",
                    zarn_tools::permissions::policy_word(policy)
                ));
            }
            for rule in session {
                out.push(format!("  allow {rule}  (this session)"));
            }
        }
        out.push("tools now".into());
        let tools: Vec<(String, Risk)> = match &self.runtime {
            Some(rt) => rt
                .tools
                .names()
                .into_iter()
                .filter_map(|n| rt.tools.get(&n).map(|t| (n.clone(), t.risk())))
                .collect(),
            None => zarn_tools::builtin_list()
                .into_iter()
                .map(|t| (t.spec().name, t.risk()))
                .collect(),
        };
        let width = tools.iter().map(|(n, _)| n.len()).max().unwrap_or(4);
        for (name, risk) in tools {
            let (policy, why) = self.gate.decide(&name, risk, &Value::Null);
            let extra = if name == "bash" && policy == Policy::Ask && cfg.general.safe_commands().is_some() {
                "; read-only commands run without asking"
            } else {
                ""
            };
            out.push(format!(
                "  {name:<width$}  {:<5} {why}{extra}",
                zarn_tools::permissions::policy_word(policy)
            ));
        }
        out.push("/permissions edit opens the settings tab".into());
        out.join("\n")
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
            Action::CycleMode => self.cycle_mode(),
            Action::Confirmed(ConfirmFor::Bypass) => self.set_mode("bypass", true),
            Action::Confirmed(ConfirmFor::Commit(message)) => self.commit(message),
            Action::Confirmed(ConfirmFor::Rewind(turn)) => self.rewind(turn),
            Action::Confirmed(ConfirmFor::Prompted(provider)) => {
                let result = match self.layers() {
                    Some(layers) => ops::update_provider(layers, &provider, |spec| {
                        spec.tools = Some("prompted".into())
                    }),
                    None => return true,
                };
                match result {
                    Ok(scope) => {
                        self.parse_failures = 0;
                        self.rebuild();
                        self.app.notice(format!(
                            "{provider} now uses prompted tool calls (tools = \"prompted\" in the {} config)",
                            scope.label()
                        ));
                    }
                    Err(err) => self.app.notice(format!("{err:#}")),
                }
            }
            Action::Plan(verdict) => self.decide_plan(verdict),
            Action::Shell(command) => self.shell(command),
            Action::Editor if self.interactive => self.editor_pending = true,
            Action::Editor => self.open_editor(),
            Action::Remember(note) => self.remember(note),
            Action::SaveRule(pattern, scope) => {
                let result = match self.layers() {
                    Some(layers) => ops::add_permission_rule(layers, scope, Policy::Allow, &pattern),
                    None => return true,
                };
                match result {
                    Ok(()) => self.app.notice(format!(
                        "allow rule {pattern} saved to the {} config",
                        scope.label()
                    )),
                    Err(err) => self.app.notice(format!("rule not saved: {err:#}")),
                }
            }
            Action::Login(provider, secret) => {
                let key = secret.expose().to_string();
                drop(secret);
                zarn_config::auth::register_secret(&key);
                let Some(layers) = self.layers.clone() else {
                    return true;
                };
                let provider = match ops::resolve_provider(&layers.merged(), &provider, &key) {
                    Ok(provider) => provider,
                    Err(err) => {
                        self.app.notice(format!("{err}: /login <provider>"));
                        return true;
                    }
                };
                self.app.notice(format!("checking the {provider} key and loading its models…"));
                let store = self.store.clone();
                let tx = self.ui_tx.clone();
                tokio::spawn(async move {
                    let mut layers = layers;
                    let cache = zarn_config::quota::cache_dir();
                    let result = ops::connect(&mut layers, &store, &cache, &provider, &key, true)
                        .await
                        .map(|done| done.summary())
                        .map_err(|err| zarn_config::auth::redact(&format!("login failed: {provider}: {err:#}")));
                    drop(key);
                    let _ = tx.send(UiEvent::LoginDone(result)).await;
                });
            }
        }
        true
    }

    fn send_next(&mut self) {
        if self.app.streaming {
            return;
        }
        if let Some(text) = self.app.next_queued() {
            if let Some(CoreCommand::Custom(invocation)) = self.commands.parse(&text) {
                self.app.blocks.pop_if(|b| matches!(b, Block::User(t) if *t == text));
                self.run_custom(&text, invocation);
                return;
            }
            self.submit(text);
        }
    }

    fn run_custom(&mut self, line: &str, invocation: zarn_core::commands::Invocation) {
        if self.app.streaming {
            self.app.enqueue(line.to_string(), line.to_string());
            return;
        }
        let mut rules = Vec::new();
        for entry in &invocation.command.allowed_tools {
            match zarn_tools::permissions::parse_rule(entry) {
                Ok(rule) => rules.push(rule),
                Err(err) => self.app.notice(format!("/{}: allowed-tools entry {entry} ignored: {err}", invocation.command.name)),
            }
        }
        self.gate.set_turn_rules(rules);
        let text = invocation.prompt();
        let model = invocation.command.model.clone();
        if zarn_core::commands::bang_commands(&text).is_empty() {
            self.turn_model = model;
            self.submit(text);
            return;
        }
        self.app.streaming = true;
        let ctx = self.ctx.clone();
        let gate = self.hooked_gate();
        let tx = self.ui_tx.clone();
        tokio::spawn(async move {
            let result = zarn_tools::bang::expand(&text, &ctx, gate.as_ref()).await;
            let _ = tx.send(UiEvent::Expanded { result, model }).await;
        });
    }

    fn export(&mut self, path: Option<&str>) {
        if self.messages.len() <= 1 {
            self.app.notice("nothing to export yet");
            return;
        }
        let target = match path {
            Some(p) => {
                let p = PathBuf::from(p);
                if p.is_absolute() { p } else { self.cwd.join(p) }
            }
            None => {
                let id = self
                    .session
                    .as_ref()
                    .map(|s| s.meta().id.chars().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect::<String>())
                    .unwrap_or_else(|| "conversation".into());
                self.cwd.join(format!("zarn-{id}.md"))
            }
        };
        let text = crate::export::markdown(self.title.as_deref(), &self.app.status.model, &self.messages);
        match std::fs::write(&target, text) {
            Ok(()) => self.app.notice(format!("exported the conversation to {} (keys redacted)", target.display())),
            Err(err) => self.app.notice(format!("could not write {}: {err}", target.display())),
        }
    }

    fn copy_last(&mut self) {
        let Some(reply) = crate::export::last_reply(&self.messages) else {
            self.app.notice("no assistant reply to copy yet");
            return;
        };
        let reply = zarn_config::auth::redact(&reply);
        let os = std::env::consts::OS;
        let command = crate::export::clipboard_command(os, |k| std::env::var(k).ok(), zarn_config::doctor::on_path);
        if let Some(command) = command {
            match crate::export::pipe_to(&command, &reply) {
                Ok(()) => {
                    self.app.notice(format!("copied the last reply ({} chars) with {}", reply.chars().count(), command[0]));
                    return;
                }
                Err(err) => self.app.notice(format!("{} failed: {err}", command[0])),
            }
        }
        if self.interactive && std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            use std::io::Write;
            let mut out = std::io::stdout();
            let _ = out.write_all(crate::export::osc52(&reply).as_bytes());
            let _ = out.flush();
            self.app.notice(format!(
                "sent the last reply ({} chars) to the clipboard with OSC 52; if nothing was copied, your terminal blocks it",
                reply.chars().count()
            ));
            return;
        }
        self.app.notice(format!("no clipboard is available here; the last reply is:\n{reply}"));
    }

    fn git_context(&mut self, what: &str) -> Option<GitContext> {
        if self.app.streaming {
            self.app.notice(format!("wait for the current turn to finish before /{what}"));
            return None;
        }
        let Some(rt) = &self.runtime else {
            self.app.notice(format!("/{what} needs a provider; run /login or /config"));
            return None;
        };
        Some((rt.provider.clone(), rt.model.clone(), self.hooked_gate()))
    }

    fn start_commit(&mut self) {
        let Some((provider, model, gate)) = self.git_context("commit") else { return };
        let trailer = self.cfg().git.commit_trailer.clone();
        let ctx = self.ctx.clone();
        let tx = self.ui_tx.clone();
        self.app.streaming = true;
        self.app.notice("drafting a commit message from the staged diff…");
        tokio::spawn(async move {
            let result = async {
                let diff = run_git(&ctx, gate.as_ref(), "git diff --cached").await?;
                if diff.trim().is_empty() || diff.trim() == "command succeeded with no output" {
                    return Err("nothing is staged; stage changes with git add first".to_string());
                }
                let raw = zarn_config::git::draft(provider.as_ref(), &model, zarn_config::git::COMMIT, &diff).await?;
                Ok(zarn_config::git::clean_message(&raw, trailer.as_deref()))
            }
            .await;
            let _ = tx.send(UiEvent::CommitDraft(result)).await;
        });
    }

    fn commit(&mut self, message: String) {
        let gate = self.hooked_gate();
        let cwd = self.cwd.clone();
        let tx = self.ui_tx.clone();
        let file = paths::data_dir().join(format!("commit-message-{}.txt", std::process::id()));
        let command = format!("git commit -F {}", file.display());
        if let Ok(rule) = zarn_tools::permissions::parse_rule(&format!("bash({command})")) {
            self.gate.set_turn_rules(vec![rule]);
        }
        self.app.streaming = true;
        tokio::spawn(async move {
            let result = async {
                if let Some(parent) = file.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::write(&file, format!("{message}\n")).map_err(|e| e.to_string())?;
                let args = serde_json::json!({ "command": command });
                if let zarn_core::agent::GateDecision::Deny(reason) = gate.check("bash", zarn_core::tools::Risk::Execute, &args).await {
                    return Err(reason);
                }
                let output = tokio::process::Command::new("git")
                    .arg("commit")
                    .arg("-F")
                    .arg(&file)
                    .current_dir(&cwd)
                    .output()
                    .await
                    .map_err(|e| e.to_string())?;
                let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
                if output.status.success() { Ok(text) } else { Err(text) }
            }
            .await;
            let _ = std::fs::remove_file(&file);
            let _ = tx.send(UiEvent::Committed(result.map_err(|e| zarn_config::auth::redact(&e)))).await;
        });
    }

    fn start_pr(&mut self) {
        let Some((provider, model, gate)) = self.git_context("pr") else { return };
        let ctx = self.ctx.clone();
        let cwd = self.cwd.clone();
        let tx = self.ui_tx.clone();
        self.app.streaming = true;
        self.app.notice("drafting a pull request from this branch…");
        tokio::spawn(async move {
            let result = async {
                let mut base = "master".to_string();
                for candidate in ["origin/HEAD", "main"] {
                    if run_git(&ctx, gate.as_ref(), &format!("git log -1 --format=%H {candidate}")).await.is_ok() {
                        base = candidate.to_string();
                        break;
                    }
                }
                let log = run_git(&ctx, gate.as_ref(), &format!("git log --oneline {base}..HEAD")).await?;
                let diff = run_git(&ctx, gate.as_ref(), &format!("git diff {base}...HEAD")).await?;
                let empty = |t: &str| t.trim().is_empty() || t.trim() == "command succeeded with no output";
                if empty(&log) && empty(&diff) {
                    return Err(format!("this branch has no changes against {base}"));
                }
                let material = format!("Commits:\n{log}\n\nDiff against {base}:\n{diff}");
                let raw = zarn_config::git::draft(provider.as_ref(), &model, zarn_config::git::PR, &material).await?;
                let (title, body) = zarn_config::git::split_pr(&raw);
                let file = cwd.join(".zarn").join("pr.md");
                if let Some(parent) = file.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&file, format!("# {title}\n\n{body}\n"));
                Ok((title, body))
            }
            .await;
            let _ = tx.send(UiEvent::PrDraft(result)).await;
        });
    }

    fn loop_command(&mut self, line: &str) {
        let rest = line.trim_start().trim_start_matches('/').trim_start_matches("loop").trim();
        let (head, tail) = rest.split_once(char::is_whitespace).map(|(a, b)| (a, b.trim())).unwrap_or((rest, ""));
        match head {
            "" | "list" => {
                if self.loops.is_empty() {
                    self.app.notice("no loops; /loop <interval> <prompt>, for example /loop 10m check the CI status");
                    return;
                }
                let lines: Vec<String> = self
                    .loops
                    .iter()
                    .map(|l| format!("  {}  every {}  {}", l.id, zarn_config::schedule::label(l.every), l.prompt))
                    .collect();
                self.app.notice(format!("loops in this session\n{}\n/loop stop <id|all> ends them", lines.join("\n")));
            }
            "stop" => {
                let before = self.loops.len();
                if tail == "all" || tail.is_empty() {
                    self.loops.clear();
                } else {
                    self.loops.retain(|l| l.id.to_string() != tail);
                }
                self.app.notice(format!("stopped {} loop{}", before - self.loops.len(), if before - self.loops.len() == 1 { "" } else { "s" }));
            }
            interval => {
                let every = match zarn_config::schedule::parse_interval(interval) {
                    Ok(every) => every,
                    Err(err) => return self.app.notice(format!("{err:#}")),
                };
                if tail.is_empty() {
                    self.app.notice("usage: /loop <interval> <prompt>");
                    return;
                }
                self.loop_count += 1;
                let id = self.loop_count;
                self.loops.push(LoopJob {
                    id,
                    every,
                    prompt: tail.to_string(),
                    next: std::time::Instant::now(),
                });
                self.app.notice(format!(
                    "loop {id}: \"{tail}\" every {}, starting now; it stops when you quit or run /loop stop {id}",
                    zarn_config::schedule::label(every)
                ));
                self.fire_loops();
            }
        }
    }

    fn fire_loops(&mut self) {
        if self.app.streaming || !self.app.queue.is_empty() || self.app.pending.is_some() {
            return;
        }
        let now = std::time::Instant::now();
        let Some(job) = self.loops.iter_mut().find(|l| l.next <= now) else { return };
        job.next = now + job.every;
        let prompt = job.prompt.clone();
        self.submit(prompt);
    }

    fn fork(&mut self) {
        if self.messages.len() <= 1 {
            self.app.notice("nothing to fork yet");
            return;
        }
        if self.app.streaming {
            self.app.notice("wait for the current turn to finish before forking");
            return;
        }
        self.persist();
        let original = self.session.as_ref().map(|s| s.meta().id.clone());
        self.session = None;
        self.persisted = 1;
        if let Some(title) = self.title.clone() {
            self.title = Some(format!("fork of {title}"));
        }
        self.persist();
        let fresh = self.session.as_ref().map(|s| s.meta().id.clone()).unwrap_or_default();
        self.app.notice(match original {
            Some(original) => format!("forked into {fresh}; {original} is unchanged"),
            None => format!("forked into {fresh}"),
        });
    }

    fn rename(&mut self, title: &str) {
        if title.is_empty() {
            self.app.notice(match &self.title {
                Some(t) => format!("this session is \"{t}\"; /rename <title> changes it"),
                None => "usage: /rename <title>".to_string(),
            });
            return;
        }
        self.title = Some(title.to_string());
        self.titled = true;
        if let Some(session) = self.session.as_mut()
            && let Err(err) = session.set_title(title)
        {
            self.app.notice(format!("could not save the title: {err:#}"));
            return;
        }
        self.app.notice(format!("session renamed to \"{title}\""));
    }

    fn maybe_title(&mut self) {
        if self.titled {
            return;
        }
        let first_user = self
            .messages
            .iter()
            .find(|m| matches!(m, Message::User(_)))
            .map(|m| m.text());
        let first_reply = self
            .messages
            .iter()
            .find(|m| matches!(m, Message::Assistant { .. }) && !m.text().trim().is_empty())
            .map(|m| m.text());
        let (Some(user), Some(reply), Some(rt)) = (first_user, first_reply, self.runtime.as_ref()) else {
            return;
        };
        self.titled = true;
        let routed = rt.router.for_role("title");
        let tx = self.ui_tx.clone();
        tokio::spawn(async move {
            let result =
                zarn_config::titles::generate(routed.provider.as_ref(), &routed.model, &user, &reply).await;
            let _ = tx.send(UiEvent::Title(result)).await;
        });
    }

    fn update_meter(&mut self) {
        let Some(agent) = &self.agent else {
            self.app.status.context = None;
            return;
        };
        let provider = agent.provider();
        let window = self.context_window().unwrap_or_else(|| provider.caps().context_window);
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
        let (provider, model) = match &self.runtime {
            Some(rt) => {
                let routed = rt.router.for_role("compact");
                (routed.provider, routed.model)
            }
            None => (agent.provider().clone(), agent.config().model.clone()),
        };
        let tx = self.ui_tx.clone();
        tokio::spawn(async move {
            let result = compact::compact(provider.as_ref(), &messages, &model, compact::KEEP_TURNS)
                .await
                .map_err(|e| zarn_config::auth::redact(&e.to_string()));
            let _ = tx.send(UiEvent::Compacted(result)).await;
        });
    }

    fn needs_auto_compact(&self) -> bool {
        let Some(agent) = &self.agent else {
            return false;
        };
        let window = self
            .context_window()
            .unwrap_or_else(|| agent.provider().caps().context_window);
        let used = agent.provider().count_tokens(&self.messages).unwrap_or(0);
        let ratio = if window == 0 { 0.0 } else { used as f32 / window as f32 };
        self.cfg().general.auto_compact()
            && ratio >= compact::TRIGGER
            && compact::split_point(&self.messages, compact::KEEP_TURNS).is_some()
    }

    fn submit_as(&mut self, text: String, role: &str) {
        let routed = self
            .runtime
            .as_ref()
            .filter(|rt| rt.router.configured(role))
            .map(|rt| (rt.router.for_role(role), rt.tools.clone()));
        let Some((routed, tools)) = routed else {
            return self.submit(text);
        };
        if self.app.streaming {
            self.app.enqueue(text.clone(), text);
            return;
        }
        let agent = Agent::new(
            routed.provider,
            tools,
            self.ctx.clone(),
            zarn_core::agent::AgentConfig {
                model: routed.model,
                ..Default::default()
            },
        )
        .with_gate(self.hooked_gate());
        self.checkpoints.begin(text.clone(), self.messages.len());
        self.messages.push(Message::user(text));
        self.cancel = CancellationToken::new();
        self.app.streaming = true;
        self.app.status.turn_started = Some(std::time::Instant::now());
        self.app.status.streamed_chars = 0;
        spawn_turn(Arc::new(agent), self.messages.clone(), self.ui_tx.clone(), self.cancel.clone());
    }

    fn spend(&self) -> (f64, f64) {
        let session = self.cost.total().usd.unwrap_or(0.0);
        (session, self.day_base + (session - self.session_base))
    }

    fn note_once(&mut self, key: String, text: String) {
        if self.cap_notes.insert(key) {
            self.app.notice(text);
        }
    }

    fn check_caps(&mut self, starting: bool) -> bool {
        let limits = self.cfg().limits;
        if limits.is_empty() {
            return true;
        }
        if starting {
            self.day_base = self.ledger.today();
            self.session_base = self.cost.total().usd.unwrap_or(0.0);
        }
        if let Some(text) =
            zarn_config::spend::unenforceable(&limits, self.cost.model(), self.cost.rates().is_some())
        {
            self.note_once(format!("unpriced:{}", self.cost.model()), text);
            return true;
        }
        let (session, day) = self.spend();
        match zarn_config::spend::check(&limits, session, day) {
            zarn_config::spend::Check::Ok => true,
            zarn_config::spend::Check::Warn(warnings) => {
                for warning in warnings {
                    let key = warning.split(" spend cap").next().unwrap_or("").to_string();
                    let cap = key.rsplit(' ').next().unwrap_or("").to_string();
                    self.note_once(format!("warn:{cap}"), warning);
                }
                true
            }
            zarn_config::spend::Check::Stop(text) => {
                self.app.notice(format!("stopped: {text}"));
                false
            }
        }
    }

    fn record_spend(&mut self) {
        let (session, _) = self.spend();
        let delta = session - self.session_base;
        if delta > 0.0 {
            if let Err(err) = self.ledger.add(delta) {
                self.app.notice(format!("daily spend not recorded: {err}"));
            }
            self.day_base = self.ledger.today();
            self.session_base = session;
        }
    }

    fn hooked_gate(&self) -> Arc<dyn zarn_core::agent::ToolGate> {
        let hooked = Arc::new(zarn_config::hooks::HookedGate::new(
            self.gate.clone(),
            self.hooks.clone(),
            self.cwd.clone(),
        ));
        Arc::new(
            zarn_tools::checkpoint::CheckpointGate::new(hooked, self.checkpoints.clone())
                .with_notes(self.checkpoint_notes.clone()),
        )
    }

    fn shell(&mut self, command: String) {
        self.shell_runs += 1;
        let id = format!("shell-{}", self.shell_runs);
        let args = serde_json::json!({ "command": command });
        self.app.apply(AgentEvent::ToolStarted {
            id: id.clone(),
            name: "bash".into(),
            args: args.clone(),
        });
        let gate = self.hooked_gate();
        let ctx = self.ctx.clone();
        let sandbox = self.cfg().general.sandbox();
        let tx = self.ui_tx.clone();
        tokio::spawn(async move {
            let output = match gate.check("bash", Risk::Execute, &args).await {
                zarn_core::agent::GateDecision::Deny(reason) => zarn_core::tools::ToolOutput::error(reason),
                zarn_core::agent::GateDecision::Allow => {
                    let tool = if sandbox {
                        zarn_tools::bash::Bash::sandboxed()
                    } else {
                        zarn_tools::bash::Bash::new()
                    };
                    let tool = guard::redacting(Box::new(tool));
                    match tool.run(args, &ctx).await {
                        Ok(out) => out,
                        Err(err) => zarn_core::tools::ToolOutput::error(format!("{err:#}")),
                    }
                }
            };
            let _ = tx.send(UiEvent::Shell { id, command, output }).await;
        });
    }

    fn sync_todos(&mut self) {
        let todos = self.todos.lock().map(|t| t.clone()).unwrap_or_default();
        self.app.todos = todos.clone();
        if let Some(session) = self.session.as_mut()
            && let Err(err) = session.set_todos(&todos)
        {
            self.app.notice(format!("todo list not saved: {err:#}"));
        }
    }

    fn open_editor(&mut self) {
        let editor = std::env::var("VISUAL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| std::env::var("EDITOR").ok().filter(|v| !v.trim().is_empty()));
        match crate::editor::edit_text(&self.app.input, editor.as_deref()) {
            Ok(text) => self.app.set_input(text),
            Err(err) => self.app.notice(err),
        }
    }

    fn remember(&mut self, note: String) {
        let project = self.cwd.join(context::MEMORY_FILE);
        let user = paths::user_dir().map(|d| d.join(context::MEMORY_FILE));
        match user.filter(|u| u.exists() && project.exists()) {
            Some(user) => {
                let choices = vec![
                    Choice::new(project.display().to_string(), "project ZARN.md").with_detail(project.display().to_string()),
                    Choice::new(user.display().to_string(), "user ZARN.md").with_detail(user.display().to_string()),
                ];
                self.app.overlay = Some(Overlay::Pick(
                    Picker::new("Add the note to which ZARN.md?", choices),
                    ChatPick::Memory(note),
                ));
            }
            None => self.append_memory(&note, &project),
        }
    }

    fn append_memory(&mut self, note: &str, path: &Path) {
        let project = self.cwd.join(context::MEMORY_FILE);
        let user = paths::user_dir().map(|d| d.join(context::MEMORY_FILE));
        if path != project && Some(path.to_path_buf()) != user {
            self.app.notice("refusing to write a note anywhere but a ZARN.md");
            return;
        }
        let mut body = std::fs::read_to_string(path).unwrap_or_default();
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(&format!("- {}\n", note.replace('\n', " ")));
        match path.parent().map(std::fs::create_dir_all).unwrap_or(Ok(())).and_then(|_| std::fs::write(path, body)) {
            Ok(()) => self.app.notice(format!("noted in {}", path.display())),
            Err(err) => self.app.notice(format!("could not write {}: {err}", path.display())),
        }
    }

    fn rewind_picker(&mut self) {
        if self.app.streaming {
            self.app.notice("wait for the turn to finish, or press esc to cancel it, before rewinding");
            return;
        }
        let turns = self.checkpoints.turns();
        if turns.is_empty() {
            self.app.notice("nothing to rewind yet");
            return;
        }
        let choices: Vec<Choice> = turns
            .iter()
            .enumerate()
            .rev()
            .map(|(i, t)| {
                let label: String = t.label.replace('\n', " ").chars().take(70).collect();
                Choice::new(i.to_string(), format!("turn {}: {label}", i + 1)).with_detail(format!(
                    "{} file{}",
                    t.files.len(),
                    if t.files.len() == 1 { "" } else { "s" }
                ))
            })
            .collect();
        let picker = Picker::new("Rewind to before which turn?", choices);
        self.app.overlay = Some(Overlay::Pick(picker, ChatPick::Rewind));
    }

    fn confirm_rewind(&mut self, value: &str) {
        let Some(plan) = value.parse::<usize>().ok().and_then(|i| self.checkpoints.plan(i)) else {
            self.app.notice("that turn is no longer available");
            return;
        };
        let dropped = self.messages.len().saturating_sub(plan.message_index);
        self.app.overlay = Some(Overlay::Confirm(
            format!(
                "Rewind to before turn {}? This drops {dropped} messages and:\n{}",
                plan.turn + 1,
                plan.describe(&self.cwd)
            ),
            ConfirmFor::Rewind(plan.turn),
        ));
    }

    fn rewind(&mut self, turn: usize) {
        let Some(plan) = self.checkpoints.plan(turn) else {
            self.app.notice("that turn is no longer available");
            return;
        };
        let errors = self.checkpoints.apply(&plan);
        self.messages.truncate(plan.message_index.max(1));
        self.set_system();
        self.app.blocks = flatten_messages(&self.messages);
        self.session = None;
        self.persisted = 1;
        let mut text = format!("rewound to before turn {}\n{}", plan.turn + 1, plan.describe(&self.cwd));
        for err in errors {
            text.push_str(&format!("\nfailed: {err}"));
        }
        self.update_meter();
        self.app.notice(text);
    }

    fn hooks(&self) -> zarn_config::hooks::Hooks {
        self.hooks.read().map(|h| h.clone()).unwrap_or_default()
    }

    fn fire_hooks(&self, event: &'static str, tool: Option<String>, payload: Value) {
        let hooks = self.hooks();
        if hooks.list(event).is_empty() {
            return;
        }
        let cwd = self.cwd.clone();
        let tx = self.ui_tx.clone();
        tokio::spawn(async move {
            let outcomes = zarn_config::hooks::fire(&hooks, event, tool.as_deref(), payload, &cwd).await;
            for outcome in outcomes.iter().filter(|o| !o.ok()) {
                let _ = tx
                    .send(UiEvent::HookFailed(format!("{event} hook: {}", outcome.reason())))
                    .await;
            }
        });
    }

    fn resource_refs(&self, text: &str) -> Vec<(Arc<McpServer>, String)> {
        let mut found = Vec::new();
        for word in text.split_whitespace() {
            let Some(rest) = word.strip_prefix('@') else { continue };
            let rest = rest.trim_end_matches([',', '.', ';', ')', '!', '?']);
            let Some((server, uri)) = rest.split_once(':') else { continue };
            if uri.is_empty() {
                continue;
            }
            if let Some(server) = self.mcp.get(server).and_then(|s| s.server.clone())
                && !found.iter().any(|(s, u): &(Arc<McpServer>, String)| s.name() == server.name() && u == uri)
            {
                found.push((server, uri.to_string()));
            }
        }
        found
    }

    fn mcp_prompt(&self, name: &str) -> Option<(Arc<McpServer>, zarn_tools::mcp::Prompt)> {
        let (server, prompt) = name.split_once(':')?;
        let server = self.mcp.get(server)?.server.clone()?;
        let prompt = server.prompts().iter().find(|p| p.name == prompt)?.clone();
        Some((server, prompt))
    }

    fn run_mcp_prompt(&mut self, server: Arc<McpServer>, prompt: zarn_tools::mcp::Prompt, args: &str) {
        let words = zarn_core::commands::split_args(args);
        let mut values = BTreeMap::new();
        for (i, arg) in prompt.arguments.iter().enumerate() {
            match words.get(i) {
                Some(value) => {
                    values.insert(arg.name.clone(), value.clone());
                }
                None if arg.required => {
                    let usage: Vec<String> = prompt
                        .arguments
                        .iter()
                        .map(|a| if a.required { format!("<{}>", a.name) } else { format!("[{}]", a.name) })
                        .collect();
                    self.app.notice(format!("usage: /{}:{} {}", server.name(), prompt.name, usage.join(" ")));
                    return;
                }
                None => {}
            }
        }
        if self.app.streaming {
            self.app.notice("wait for the current turn to finish before running an MCP prompt");
            return;
        }
        self.app.streaming = true;
        let tx = self.ui_tx.clone();
        tokio::spawn(async move {
            let result = server
                .client()
                .get_prompt(&prompt.name, &values)
                .await
                .map_err(|e| zarn_config::auth::redact(&format!("{e:#}")));
            let _ = tx.send(UiEvent::McpPrompt(result)).await;
        });
    }

    fn submit(&mut self, text: String) {
        self.submit_with(text, false);
    }

    fn submit_with(&mut self, text: String, resolved: bool) {
        if self.app.streaming {
            self.app.enqueue(text.clone(), text);
            return;
        }
        let refs = if resolved { Vec::new() } else { self.resource_refs(&text) };
        if !refs.is_empty() {
            self.app.streaming = true;
            let tx = self.ui_tx.clone();
            tokio::spawn(async move {
                let mut blocks = Vec::new();
                let mut notes = Vec::new();
                for (server, uri) in refs {
                    match server.client().read_resource(&uri).await {
                        Ok(body) => blocks.push(format!(
                            "<resource server=\"{}\" uri=\"{uri}\">\n{}\n</resource>",
                            server.name(),
                            body.trim_end()
                        )),
                        Err(err) => notes.push(format!("@{}:{uri} could not be read: {}", server.name(), zarn_config::auth::redact(&format!("{err:#}")))),
                    }
                }
                let _ = tx.send(UiEvent::Resolved { text, blocks, notes }).await;
            });
            return;
        }
        let hooks = self.hooks();
        if !hooks.prompt_submit.is_empty() {
            self.app.streaming = true;
            let cwd = self.cwd.clone();
            let tx = self.ui_tx.clone();
            tokio::spawn(async move {
                let outcomes = zarn_config::hooks::fire(
                    &hooks,
                    "prompt_submit",
                    None,
                    serde_json::json!({ "prompt": text }),
                    &cwd,
                )
                .await;
                let blocked = outcomes.iter().find(|o| !o.ok()).map(|o| o.reason());
                let _ = tx.send(UiEvent::Prompted { text, blocked }).await;
            });
            return;
        }
        self.submit_checked(text);
    }

    fn user_message(&mut self, text: &str, vision: bool) -> Message {
        let mut images = Vec::new();
        let mut rest = text.to_string();
        while let Some(start) = rest.find(crate::state::IMAGE_MARK) {
            let after = &rest[start + crate::state::IMAGE_MARK.len()..];
            let Some(end) = after.find("]]") else { break };
            images.push(std::path::PathBuf::from(&after[..end]));
            rest.replace_range(start..start + crate::state::IMAGE_MARK.len() + end + 2, "");
        }
        let expanded = crate::mentions::expand(rest.trim(), &self.cwd, vision, crate::mentions::ATTACH_BYTES);
        for note in &expanded.notes {
            self.app.notice(note.clone());
        }
        let mut content = expanded.content;
        for path in images {
            let name = path.display().to_string();
            if !vision {
                self.app.notice(format!(
                    "{name} is an image, but this model does not accept images; it was not attached"
                ));
                continue;
            }
            let Some(mime) = crate::mentions::image_mime(&name) else { continue };
            match std::fs::metadata(&path) {
                Ok(meta) if meta.len() > crate::mentions::IMAGE_BYTES => {
                    self.app.notice(format!("{name} is over 5 MB; it was not attached"));
                }
                Ok(_) => match std::fs::read(&path) {
                    Ok(data) => content.push(zarn_core::types::Content::Image {
                        mime: mime.to_string(),
                        data,
                    }),
                    Err(err) => self.app.notice(format!("{name} could not be read: {err}")),
                },
                Err(err) => self.app.notice(format!("{name} could not be read: {err}")),
            }
        }
        if let Some(results) = zarn_core::subagent::deliver(&self.background.take_finished()) {
            self.pending_context.insert(0, results);
        }
        if !self.pending_context.is_empty()
            && let Some(zarn_core::types::Content::Text(first)) = content.first_mut()
        {
            let context = std::mem::take(&mut self.pending_context).join("\n\n");
            *first = format!("{context}\n\n{first}");
        }
        Message::User(content)
    }

    fn submit_checked(&mut self, text: String) {
        if !self.check_caps(true) {
            return;
        }
        self.checkpoints.begin(text.clone(), self.messages.len());
        self.cap_stopped = false;
        self.reload_if_changed();
        let Some(mut agent) = self.agent.clone() else {
            let why = self
                .app
                .not_ready
                .clone()
                .unwrap_or_else(|| "no provider is ready".into());
            self.app.notice(format!("cannot send: {why}"));
            return;
        };
        if let Some(model) = self.turn_model.take()
            && let Some(rt) = &self.runtime
        {
            let (provider, model) = rt.router.for_model(&model);
            let config = zarn_core::agent::AgentConfig { model, ..rt.agent_config() };
            let mut one = Agent::new(provider, rt.tools.clone(), self.ctx.clone(), config).with_gate(self.hooked_gate());
            if let Some(filter) = rt.filter() {
                one = one.with_filter(filter);
            }
            agent = Arc::new(one);
        }
        let vision = agent.provider().caps().vision;
        let message = self.user_message(&text, vision);
        self.messages.push(message);
        self.cancel = CancellationToken::new();
        self.app.streaming = true;
        self.app.status.turn_started = Some(std::time::Instant::now());
        self.app.status.streamed_chars = 0;
        spawn_turn(agent, self.messages.clone(), self.ui_tx.clone(), self.cancel.clone());
    }

    fn on_ui(&mut self, event: UiEvent) {
        match event {
            UiEvent::Agent(event) => {
                let usage = matches!(&event, AgentEvent::Provider(zarn_core::provider::Event::Usage { .. }));
                let started = match &event {
                    AgentEvent::ToolStarted { id, name, args } => {
                        self.note_tool_call(args);
                        Some((id.clone(), name.clone(), args.clone()))
                    }
                    _ => None,
                };
                if let AgentEvent::ToolFinished { name, .. } = &event
                    && name == "todo"
                {
                    self.sync_todos();
                }
                if let AgentEvent::ToolFinished { id, name, output } = &event {
                    let args = self
                        .app
                        .blocks
                        .iter()
                        .rev()
                        .find_map(|b| match b {
                            Block::Tool(card) if card.id == *id => Some(card.args.clone()),
                            _ => None,
                        })
                        .unwrap_or(Value::Null);
                    self.fire_hooks(
                        "post_tool",
                        Some(name.clone()),
                        serde_json::json!({ "args": args, "output": output.content, "is_error": output.is_error }),
                    );
                }
                if let AgentEvent::Provider(provider_event) = &event {
                    self.cost.observe(provider_event);
                }
                self.app.apply(event);
                if let Some((id, name, args)) = started
                    && let Some(preview) = zarn_tools::diff::preview(&name, &args, &self.cwd)
                {
                    let label = if preview.new_file { "new file " } else { "" };
                    self.app.set_card_detail(&id, format!("{} {label}{}", preview.summary(), preview.path));
                }
                self.app.status.cost = self.cost.total().usd;
                if usage && self.app.streaming && !self.cap_stopped && !self.check_caps(false) {
                    self.cap_stopped = true;
                    self.cancel.cancel();
                }
            }
            UiEvent::TurnDone { messages, result } => {
                self.app.streaming = false;
                self.gate.clear_turn_rules();
                self.app.agents.retain(|row| row.background && row.done.is_none());
                if let Some(started) = self.app.status.turn_started.take() {
                    self.notifier.turn_finished(started.elapsed());
                }
                self.app.status.branch = crate::status_line::branch(&self.cwd);
                self.checkpoints.end();
                let notes: Vec<String> = self
                    .checkpoint_notes
                    .lock()
                    .map(|mut n| std::mem::take(&mut *n))
                    .unwrap_or_default();
                for note in notes {
                    self.app.notice(note);
                }
                self.messages = messages;
                self.set_system();
                self.cost.end_turn();
                self.app.status.cost = self.cost.total().usd;
                self.record_spend();
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
                self.maybe_title();
                self.fire_hooks(
                    "turn_end",
                    None,
                    serde_json::json!({ "messages": self.messages.len() }),
                );
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
                (Mode::Onboarding(flow), ModelTarget::Onboarding) => {
                    flow.set_models(models.into_iter().map(|(m, _)| m).collect())
                }
                (Mode::Chat, ModelTarget::Chat) => {
                    if let Some(Overlay::Pick(picker, ChatPick::Model)) = &mut self.app.overlay {
                        picker.set_choices(settings::merge_models(picker.choices.clone(), models));
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
                if let Ok(server) = &result {
                    let tx = self.ui_tx.clone();
                    let who = name.clone();
                    server.on_notification(Arc::new(move |method: &str| {
                        if method == "notifications/tools/list_changed" {
                            let _ = tx.try_send(UiEvent::McpChanged(who.clone()));
                        }
                    }));
                }
                if let Some(state) = self.mcp.get_mut(&name) {
                    state.status = status.clone();
                    state.server = result.ok().map(|server| Arc::new(*server));
                }
                self.panel_mcp_status(&name, &status);
                if status.starts_with("failed") && matches!(self.app.mode, Mode::Chat) {
                    self.app.notice(format!("mcp {name} {status}"));
                }
                self.rebuild();
            }
            UiEvent::Doctor(report) => self.app.notice(report),
            UiEvent::Notice(text) => self.app.notice(text),
            UiEvent::WebKeys(change) => self.web_change(change),
            UiEvent::LoginDone(Ok(summary)) => {
                self.app.notice(summary);
                self.rebuild();
            }
            UiEvent::LoginDone(Err(err)) => self.app.notice(err),
            UiEvent::Title(Ok(title)) => {
                self.title = Some(title.clone());
                if let Some(session) = self.session.as_mut()
                    && let Err(err) = session.set_title(&title)
                {
                    self.app.notice(format!("title not saved: {err:#}"));
                }
            }
            UiEvent::Title(Err(err)) => tracing::debug!(error = %err, "no session title"),
            UiEvent::Compared(sides) => self.app.blocks.push(Block::Compare(sides)),
            UiEvent::Task(update) => self.app.task_update(update),
            UiEvent::McpLoggedIn { name, result } => match result {
                Ok(()) => {
                    self.app.notice(format!("logged in to {name}; the token is in the credential store as zarn-mcp:{name}"));
                    if let Some(config) = self.cfg().mcp.get(&name).cloned() {
                        self.start_mcp(name, config);
                    }
                }
                Err(err) => self.app.notice(format!("login to {name} failed: {err}")),
            },
            UiEvent::CommitDraft(result) => {
                self.app.streaming = false;
                match result {
                    Ok(message) => {
                        self.app.overlay = Some(Overlay::Confirm(
                            format!("Commit the staged changes with this message?\n\n{message}"),
                            ConfirmFor::Commit(message),
                        ));
                    }
                    Err(err) => self.app.notice(format!("/commit: {err}")),
                }
            }
            UiEvent::Committed(result) => {
                self.app.streaming = false;
                self.gate.clear_turn_rules();
                match result {
                    Ok(output) => self.app.notice(format!("committed\n{}", output.trim())),
                    Err(err) => self.app.notice(format!("commit failed: {err}")),
                }
            }
            UiEvent::PrDraft(result) => {
                self.app.streaming = false;
                match result {
                    Ok((title, body)) => self.app.notice(format!(
                        "pull request draft (nothing was sent anywhere; also saved to .zarn/pr.md)\n\nTitle: {title}\n\n{body}"
                    )),
                    Err(err) => self.app.notice(format!("/pr: {err}")),
                }
            }
            UiEvent::McpChanged(name) => {
                if let Some(server) = self.mcp.get(&name).and_then(|s| s.server.clone()) {
                    let tx = self.ui_tx.clone();
                    tokio::spawn(async move {
                        let result = server.refresh_tools().await.map_err(|e| format!("{e:#}"));
                        let _ = tx.send(UiEvent::McpRefreshed { name, result }).await;
                    });
                }
            }
            UiEvent::McpRefreshed { name, result } => match result {
                Ok(n) => {
                    let status = format!("running, {n} tools");
                    if let Some(state) = self.mcp.get_mut(&name) {
                        state.status = status.clone();
                    }
                    self.panel_mcp_status(&name, &status);
                    self.app.notice(format!("mcp {name} changed its tools; now {n}"));
                    self.rebuild();
                }
                Err(err) => self.app.notice(format!("mcp {name} tool refresh failed: {err}")),
            },
            UiEvent::McpPrompt(result) => {
                self.app.streaming = false;
                match result {
                    Ok(text) => self.submit(text),
                    Err(err) => {
                        self.app.notice(err);
                        self.send_next();
                    }
                }
            }
            UiEvent::Resolved { text, blocks, notes } => {
                self.app.streaming = false;
                for note in notes {
                    self.app.notice(note);
                }
                self.pending_context.extend(blocks);
                self.submit_with(text, true);
            }
            UiEvent::Expanded { result, model } => {
                self.app.streaming = false;
                match result {
                    Ok(text) => {
                        self.turn_model = model;
                        self.submit(text);
                    }
                    Err(err) => {
                        self.gate.clear_turn_rules();
                        self.app.notice(err);
                        self.send_next();
                    }
                }
            }
            UiEvent::Progress { command, line } => self.app.progress(&command, &line),
            UiEvent::HookFailed(text) => self.app.notice(text),
            UiEvent::Shell { id, command, output } => {
                self.last_shell = Some((command, output.content.clone()));
                self.app.apply(AgentEvent::ToolFinished {
                    id,
                    name: "bash".into(),
                    output,
                });
                self.app.notice("not sent to the model; /attach-output adds it to your next message");
            }
            UiEvent::Prompted { text, blocked } => {
                self.app.streaming = false;
                match blocked {
                    Some(reason) => {
                        self.app.notice(format!("prompt blocked by a prompt_submit hook: {reason}"));
                        self.send_next();
                    }
                    None => self.submit_checked(text),
                }
            }
            UiEvent::Window { model, window } => {
                self.window = Some((model, window));
                self.update_meter();
            }
            UiEvent::Quota { reports, chat } => {
                if let Mode::Panel(panel) = &mut self.app.mode {
                    for report in &reports {
                        panel.set_quota(&report.provider, QuotaState::Loaded(Box::new(report.clone())));
                    }
                }
                if chat {
                    self.app.notice(zarn_config::quota::render(&reports));
                }
            }
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
                Ok(mut created) => {
                    if let Some(title) = &self.title {
                        let _ = created.set_title(title);
                    }
                    if !self.app.todos.is_empty() {
                        let _ = created.set_todos(&self.app.todos);
                    }
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
        if start >= self.messages.len() {
            return;
        }
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
                self.titled = meta.title.is_some();
                self.title = meta.title.clone();
                if let Ok(mut todos) = self.todos.lock() {
                    *todos = meta.todos.clone();
                }
                self.app.todos = meta.todos.clone();
                let loaded: Vec<Message> = loaded
                    .into_iter()
                    .skip_while(|m| matches!(m, Message::System(_)))
                    .collect();
                self.messages = vec![Message::System(self.system.clone())];
                self.messages.extend(loaded);
                self.app.blocks = flatten_messages(&self.messages);
                if self.options.fork {
                    self.session = None;
                    self.persisted = 1;
                    self.app.notice(format!(
                        "forked {} ({} messages); your next message starts a new session and the original is unchanged",
                        meta.id,
                        self.messages.len() - 1
                    ));
                    return;
                }
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
            settings::Effect::SetMode(name) if name == "bypass" => {
                self.app.mode = Mode::Chat;
                self.ask_bypass();
            }
            settings::Effect::SetMode(name) => self.set_mode(&name, false),
            settings::Effect::RefreshQuota(only) => self.fetch_quota(only, true, false),
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
                let Ok(spec) = ops::spec_with_key(&self.cfg(), &self.store, &provider) else {
                    return;
                };
                let tx = self.ui_tx.clone();
                tokio::spawn(async move {
                    let models = ops::model_choices(&provider, &spec).await;
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
                    let models = ops::list_models(&spec).await.into_iter().map(|m| (m, None)).collect();
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
            onboarding::Effect::Web => {
                if let Some(url) = self.open_key_page()
                    && let Mode::Onboarding(flow) = &mut self.app.mode
                {
                    flow.step = onboarding::Step::Browser(url);
                }
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
            ChatPick::Login if value == "web" => {
                self.open_key_page();
            }
            ChatPick::Login => self.ask_login(&value),
            ChatPick::Logout => self.logout(&value),
            ChatPick::Profile => self.use_profile(&value),
            ChatPick::Rewind => self.confirm_rewind(&value),
            ChatPick::Memory(note) => self.append_memory(&note, Path::new(&value)),
            ChatPick::History => self.app.set_input(value),
            ChatPick::Mode if value == "bypass" => self.ask_bypass(),
            ChatPick::Mode => self.set_mode(&value, true),
        }
    }

    pub fn key_page(&self) -> Option<&str> {
        self.web.as_ref().filter(|w| !w.is_stopped()).map(|w| w.url.as_str())
    }

    fn open_key_page(&mut self) -> Option<String> {
        if let Some(url) = self.key_page() {
            let url = url.to_string();
            zarn_config::webkeys::open_browser(&url);
            self.app.notice(format!("the API key page is already open at {url}"));
            return Some(url);
        }
        let tx = self.ui_tx.clone();
        let mut options = zarn_config::webkeys::Options::new(self.store.clone());
        options.on_change = Some(Arc::new(move |change| {
            let _ = tx.try_send(UiEvent::WebKeys(change));
        }));
        match zarn_config::webkeys::start(options) {
            Ok(running) => {
                let url = running.url.clone();
                let opened = zarn_config::webkeys::open_browser(&url);
                self.web = Some(running);
                self.app.notice(format!(
                    "API key setup is open at {url}\n{} Keys you save there apply here right away; click Done on the page when you are finished.",
                    if opened { "It should open in your browser now; if not, open the link on this machine." } else { "Open the link in a browser on this machine." }
                ));
                Some(url)
            }
            Err(err) => {
                self.app.notice(format!("could not open the key setup page: {err:#}; use /login <provider> instead"));
                None
            }
        }
    }

    fn web_change(&mut self, change: zarn_config::webkeys::Change) {
        use zarn_config::webkeys::Change;
        let onboarding = matches!(self.app.mode, Mode::Onboarding(_));
        match change {
            Change::Closed => {
                self.web = None;
                if let Mode::Onboarding(flow) = &mut self.app.mode
                    && matches!(flow.step, onboarding::Step::Browser(_))
                {
                    flow.back_to_start();
                }
                self.app.notice("the API key setup page is closed");
                return;
            }
            Change::Saved { provider, place } => {
                self.app.notice(format!("{provider}: key saved from the setup page to the {place}"))
            }
            Change::Removed(provider) => self.app.notice(format!("{provider}: key removed from the setup page")),
            Change::Added(provider) => self.app.notice(format!("{provider}: endpoint added from the setup page")),
            Change::Updated(provider) => self.app.notice(format!("{provider}: settings changed from the setup page")),
        }
        if onboarding {
            self.app.mode = Mode::Chat;
            self.rebuild();
            if self.app.not_ready.is_none() {
                self.app.notice("setup complete. Type a message, or / for commands.");
            }
            return;
        }
        self.rebuild();
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
        if crate::theme::NAMES.contains(&color) {
            let result = match self.layers() {
                Some(layers) => ops::set_general(layers, GeneralField::Theme, |g| g.theme = Some(color.to_string())),
                None => return,
            };
            match result {
                Ok(()) => {
                    self.rebuild();
                    self.app.notice(format!("theme is now {color} ({})", self.app.theme.palette.name));
                }
                Err(err) => self.app.notice(format!("{err:#}")),
            }
            return;
        }
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
            local_path: paths::local_config(),
            project_path: paths::project_config(),
            user_path: paths::user_config(),
            local: Config::default(),
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
        let provider = current.provider.clone().unwrap_or_default();
        let spec = ops::spec_with_key(&cfg, &self.store, &provider).ok();
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
        let cached = zarn_config::quota::cached_models(&zarn_config::quota::cache_dir(), &provider)
            .into_iter()
            .map(|m| (m.id, m.context_window))
            .collect();
        let choices = settings::merge_models(choices, cached);
        let picker = Picker::new(format!("Model for profile {profile}"), choices)
            .custom("type any model name, then enter")
            .select_value(current.model.as_deref().unwrap_or_default());
        self.app.overlay = Some(Overlay::Pick(picker, ChatPick::Model));
        if let Some(spec) = spec {
            let tx = self.ui_tx.clone();
            tokio::spawn(async move {
                let models = ops::model_choices(&provider, &spec).await;
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
                let keys = self.app.keymap.help();
                self.app.notice(format!(
                    "{text}\n\nkeys (rebind in ~/.config/zarn/keybindings.toml):\n{keys}\n\n{}\n\n!command runs a shell command without the model · #note adds a line to ZARN.md · @path attaches a file",
                    crate::state::VIM_HELP
                ));
            }
            "clear" => {
                self.messages.truncate(1);
                self.app.blocks.clear();
                self.session = None;
                self.persisted = 1;
                self.titled = false;
                self.title = None;
                if let Ok(mut todos) = self.todos.lock() {
                    todos.clear();
                }
                self.app.todos.clear();
                self.memory.reset();
                self.app.notice("conversation cleared");
            }
            "compact" => {
                if self.app.streaming {
                    self.app.notice("wait for the current turn to finish");
                    return true;
                }
                self.start_compaction(false);
            }
            "rewind" => self.rewind_picker(),
            "history" => {
                let mut seen = std::collections::BTreeSet::new();
                let choices: Vec<Choice> = self
                    .app
                    .history
                    .iter()
                    .rev()
                    .filter(|h| seen.insert((*h).clone()))
                    .map(|h| Choice::new(h.clone(), h.replace('\n', " ")))
                    .collect();
                if choices.is_empty() {
                    self.app.notice("no earlier inputs yet");
                } else {
                    let picker = Picker::new("Search earlier inputs (type to filter)", choices);
                    self.app.overlay = Some(Overlay::Pick(picker, ChatPick::History));
                }
            }
            "attach-output" => match self.last_shell.take() {
                Some((command, output)) => {
                    self.pending_context
                        .push(format!("Output of `{command}`, run by the user:\n{output}"));
                    self.app.notice(format!("the output of `{command}` will go with your next message"));
                }
                None => self.app.notice("no ! command output to attach; run one with !command"),
            },
            "profile" => match first {
                Some(name) => self.use_profile(name),
                None => self.profile_picker(),
            },
            "compare" => self.compare(&words),
            "limits" => {
                let target = match first {
                    Some("session") => Some(false),
                    Some("day") | Some("daily") => Some(true),
                    Some(other) => {
                        self.app.notice(format!("unknown: /limits {other}; use /limits session <usd|off> or /limits day <usd|off>"));
                        return true;
                    }
                    None => None,
                };
                match target {
                    None => {
                        let limits = self.cfg().limits;
                        let (session, _) = self.spend();
                        let day = self.ledger.today();
                        let show = |v: Option<f64>| v.map(|v| format!("${v:.2}")).unwrap_or_else(|| "off".into());
                        self.app.notice(format!(
                            "session cap {} · spent ${session:.4}\ndaily cap   {} · spent today ${day:.4}\nchange with /limits session <usd|off> or /limits day <usd|off>",
                            show(limits.session_usd),
                            show(limits.day_usd)
                        ));
                    }
                    Some(day) => {
                        let value = words.get(1).cloned().unwrap_or_default();
                        let result = ops::parse_cap(&value).and_then(|usd| match self.layers() {
                            Some(layers) => ops::set_spend_cap(layers, day, usd).map(|s| (usd, s)),
                            None => Err(anyhow::anyhow!("config could not be loaded")),
                        });
                        match result {
                            Ok((usd, scope)) => {
                                self.rebuild();
                                self.cap_notes.clear();
                                let label = if day { "daily" } else { "session" };
                                self.app.notice(match usd {
                                    Some(v) => format!("{label} spend cap set to ${v:.2} in the {} config", scope.label()),
                                    None => format!("{label} spend cap removed"),
                                });
                            }
                            Err(err) => self.app.notice(format!("{err:#}")),
                        }
                    }
                }
            }
            "models" => {
                let refresh = first == Some("refresh");
                self.app.notice(if refresh {
                    "fetching models and quotas from every provider with a key…"
                } else {
                    "models and quotas (cached for a few minutes; /models refresh to fetch again)…"
                });
                self.fetch_quota(None, refresh, true);
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
            "export" => self.export(first),
            "loop" => self.loop_command(line),
            "review" => {
                let focus = line.trim_start().trim_start_matches('/').trim_start_matches("review").trim();
                self.submit(zarn_config::git::review_prompt(focus));
            }
            "commit" => self.start_commit(),
            "pr" => self.start_pr(),
            "fork" => self.fork(),
            "tasks" => match (first, line.split_whitespace().nth(2)) {
                (Some("cancel"), Some(id)) => match self.background.cancel(id) {
                    Ok(text) => self.app.notice(text),
                    Err(err) => self.app.notice(err),
                },
                (Some("cancel"), None) => self.app.notice("usage: /tasks cancel <id>"),
                _ => self.app.notice(tasks_report(&self.background.list())),
            },
            "copy" => self.copy_last(),
            "rename" => {
                let title = line.trim_start().trim_start_matches('/').trim_start_matches("rename").trim();
                self.rename(title);
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
                            let first: String = first.replace('\n', " ").chars().take(80).collect();
                            let label = match &meta.title {
                                Some(title) => format!("{short}  {title}"),
                                None => format!("{short}  {first}"),
                            };
                            let detail = match &meta.title {
                                Some(_) => format!("{} messages · {first}", messages.len()),
                                None => format!("{} messages", messages.len()),
                            };
                            Some(Choice::new(path.display().to_string(), label).with_detail(detail))
                        })
                        .collect();
                    if choices.is_empty() {
                        self.app.notice("no earlier sessions in this directory");
                    } else {
                        self.app.overlay = Some(Overlay::Pick(
                            Picker::new("Resume a session (type to filter titles and first messages)", choices),
                            ChatPick::Resume,
                        ));
                    }
                }
            },
            "login" => match first {
                Some("web") => {
                    self.open_key_page();
                }
                Some(provider) => self.ask_login(provider),
                None => {
                    let mut picker = self.provider_picker("Log in to which provider?", true, false).custom("or type a provider name");
                    picker.choices.insert(
                        0,
                        Choice::new("web", "web").with_detail("set up keys in your browser on a localhost page"),
                    );
                    picker.choices.insert(
                        1,
                        Choice::new("auto", "auto").with_detail("paste any key; zarn works out the provider from it"),
                    );
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
            "permissions" => match first {
                Some("edit") => {
                    self.open_panel(Tab::Permissions);
                }
                _ => {
                    let text = self.permission_summary();
                    self.app.notice(text);
                }
            },
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
                    self.submit_as(INIT_PROMPT.to_string(), "init");
                }
            }
            "doctor" => {
                self.app.notice("running checks…");
                let store = self.store.clone();
                let tx = self.ui_tx.clone();
                let mut checks = terminal::checks(Some(self.keyboard));
                checks.extend(crate::keymap::checks());
                checks.push(terminal::mouse_check(self.mouse));
                tokio::spawn(async move {
                    let mut all = doctor::run(&store, paths::project_config(), paths::user_config(), true).await;
                    all.append(&mut checks);
                    let _ = tx.send(UiEvent::Doctor(doctor::render(&all))).await;
                });
            }
            "plugins" => {
                self.open_panel(Tab::Plugins);
            }
            "hooks" => {
                let text = zarn_config::hooks::describe(&self.hooks());
                self.app.notice(text);
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
            "mode" => match first {
                Some("bypass") => self.ask_bypass(),
                Some(name) => self.set_mode(name, true),
                None => {
                    self.modes = modes::load();
                    let choices = self
                        .modes
                        .modes
                        .iter()
                        .map(|m| {
                            Choice::new(m.def.name.clone(), m.def.name.clone())
                                .with_detail(format!("{} · {}", m.source.label(), m.def.description))
                        })
                        .collect();
                    let picker = Picker::new("Mode", choices).select_value(&self.mode);
                    self.app.overlay = Some(Overlay::Pick(picker, ChatPick::Mode));
                }
            },
            "theme" => match first {
                Some(color) => self.set_theme(color),
                None => {
                    let cfg = self.cfg();
                    let choices = crate::theme::NAMES
                        .iter()
                        .map(|n| Choice::new(*n, *n).with_detail("theme"))
                        .chain(brand::ACCENTS.iter().map(|(n, _)| Choice::new(*n, *n).with_detail("accent color")))
                        .collect();
                    let picker = Picker::new("Theme or accent color", choices)
                        .custom("or type a hex color like #ff8800")
                        .select_value(cfg.general.accent.as_deref().unwrap_or("violet"));
                    self.app.overlay = Some(Overlay::Pick(picker, ChatPick::Theme));
                }
            },
            other => {
                if let Some((server, prompt)) = self.mcp_prompt(other) {
                    self.run_mcp_prompt(server, prompt, &args);
                    return true;
                }
                match self.commands.parse(line) {
                    Some(CoreCommand::Custom(invocation)) => {
                        self.run_custom(line, invocation);
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
            Some("login") => {
                let Some(name) = words.get(1).cloned() else {
                    self.app.notice("usage: /mcp login <name>");
                    return;
                };
                let Some(config) = self.cfg().mcp.get(&name).cloned() else {
                    self.app.notice(format!("no MCP server named {name}"));
                    return;
                };
                if config.url.is_none() {
                    self.app.notice(format!("{name} runs locally; only remote (url) servers log in"));
                    return;
                }
                self.app.notice(format!("starting the device login for {name}…"));
                let store = self.store.clone();
                let tx = self.ui_tx.clone();
                tokio::spawn(async move {
                    let notice = tx.clone();
                    let who = name.clone();
                    let result = zarn_config::mcp_auth::login(&store, &name, &config, move |code| {
                        let link = code.verification_uri_complete.clone().unwrap_or_else(|| code.verification_uri.clone());
                        let _ = notice.try_send(UiEvent::Notice(format!(
                            "to authorize {who}, open {link} and enter the code {}; zarn waits for the approval",
                            code.user_code
                        )));
                    })
                    .await
                    .map_err(|e| zarn_config::auth::redact(&format!("{e:#}")));
                    let _ = tx.send(UiEvent::McpLoggedIn { name, result }).await;
                });
            }
            Some("logout") => {
                let Some(name) = words.get(1) else {
                    self.app.notice("usage: /mcp logout <name>");
                    return;
                };
                match zarn_config::mcp_auth::forget(&self.store, name) {
                    Ok(true) => self.app.notice(format!("removed the stored token for {name}")),
                    Ok(false) => self.app.notice(format!("no stored token for {name}")),
                    Err(err) => self.app.notice(format!("{err:#}")),
                }
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

async fn compare_one(
    label: String,
    model: String,
    provider: Result<Arc<dyn zarn_core::provider::Provider>, String>,
    prompt: String,
) -> crate::state::CompareSide {
    use crate::state::CompareSide;
    use zarn_core::provider::Event;
    let title = format!("{label} · {model}");
    let provider = match provider {
        Ok(p) => p,
        Err(err) => {
            return CompareSide {
                label: title,
                text: err,
                meta: "not run".into(),
                failed: true,
            };
        }
    };
    let started = std::time::Instant::now();
    let request = zarn_core::types::Request::new(model.clone(), vec![Message::user(prompt)]);
    let run = async {
        let mut stream = provider.stream(request).await.map_err(|e| e.to_string())?;
        let mut text = String::new();
        let (mut input, mut output) = (0u32, 0u32);
        while let Some(event) = stream.next().await {
            match event {
                Event::TextDelta(delta) => text.push_str(&delta),
                Event::Usage { input: i, output: o, .. } => {
                    input += i;
                    output += o;
                }
                Event::Error(err) => return Err(err.to_string()),
                Event::Done(_) => break,
                _ => {}
            }
        }
        Ok((text, input, output))
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(120), run).await;
    let elapsed = started.elapsed().as_secs_f32();
    match result {
        Ok(Ok((text, input, output))) => CompareSide {
            label: title,
            text: zarn_config::auth::redact(&text),
            meta: format!("{elapsed:.1} s · {input} in / {output} out"),
            failed: false,
        },
        Ok(Err(err)) => CompareSide {
            label: title,
            text: zarn_config::auth::redact(&err),
            meta: format!("failed after {elapsed:.1} s"),
            failed: true,
        },
        Err(_) => CompareSide {
            label: title,
            text: "no answer within 120 seconds".into(),
            meta: "timed out".into(),
            failed: true,
        },
    }
}

async fn run_git(ctx: &Ctx, gate: &dyn zarn_core::agent::ToolGate, command: &str) -> Result<String, String> {
    let args = serde_json::json!({ "command": command });
    if let zarn_core::agent::GateDecision::Deny(reason) = gate.check("bash", zarn_core::tools::Risk::Execute, &args).await {
        return Err(format!("{command} was not run: {reason}"));
    }
    match zarn_core::tools::Tool::run(&zarn_tools::bash::Bash::new(), args, ctx).await {
        Ok(out) if !out.is_error => Ok(out.content),
        Ok(out) => Err(out.content),
        Err(err) => Err(format!("{err:#}")),
    }
}

fn load_commands() -> Commands {
    let user = paths::user_dir().map(|d| d.join("commands"));
    let mut commands = Commands::load_all(user.as_deref(), &paths::project_dir().join("commands"));
    zarn_config::plugins::commands_into(&mut commands, &zarn_config::plugins::installed());
    commands
}

fn tasks_report(jobs: &[zarn_core::subagent::Job]) -> String {
    if jobs.is_empty() {
        return "no background agents; ask for one with task and background = true".into();
    }
    let mut out = vec!["background agents".to_string()];
    for job in jobs {
        let secs = job.elapsed.unwrap_or_else(|| job.started.elapsed()).as_secs();
        let prompt: String = job.prompt.replace('\n', " ").chars().take(60).collect();
        out.push(format!(
            "  {}  {}  {}  {}  {prompt}",
            job.id,
            job.agent,
            job.status.label(),
            crate::render::elapsed_label(secs)
        ));
        if !job.result.is_empty() {
            let result: String = job.result.trim().replace('\n', " ").chars().take(200).collect();
            out.push(format!("      {result}{}", if job.delivered { "" } else { "  (goes to the model with your next message)" }));
        }
    }
    out.push("/tasks cancel <id> stops a running agent".into());
    out.join("\n")
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

impl Drop for Controller {
    fn drop(&mut self) {
        zarn_tools::progress::unregister(&self.ctx);
        self.shells.kill_all();
        self.background.cancel_all();
    }
}
