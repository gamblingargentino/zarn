use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zarn_config::auth::{KeyStore, MemoryBackend};
use zarn_config::layers::Layers;
use zarn_tools::permissions::Policy;
use zarn_tui::onboarding::Onboarding;
use zarn_tui::render;
use zarn_tui::settings::{Effect, Panel, PanelData, SkillRow, Tab};
use zarn_tui::state::{Action, App, Mode, Overlay};
use zarn_tui::widgets::MaskedInput;

const KEY: &str = "test-key-not-real-snapshot";

fn rows(width: u16, height: u16, app: &App) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render::draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer.cell((x, y)).map(|c| c.symbol().to_string()).unwrap_or_default())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

fn snapshot(name: &str, rendered: &[String]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.txt"));
    let text = rendered.join("\n") + "\n";
    if std::env::var("ZARN_UPDATE_SNAPSHOTS").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing snapshot {}; run with ZARN_UPDATE_SNAPSHOTS=1 to create it\n{text}",
            path.display()
        )
    });
    assert_eq!(text, expected, "snapshot {name} differs");
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    store: Arc<KeyStore>,
}

impl Fixture {
    fn new(user_config: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("user")).unwrap();
        std::fs::write(root.join("user/config.toml"), user_config).unwrap();
        let store = Arc::new(KeyStore::new(
            Arc::new(MemoryBackend::new()),
            root.join("user/auth.json"),
        ));
        Fixture {
            _dir: dir,
            root,
            store,
        }
    }

    fn layers(&self) -> Layers {
        Layers::load_from(
            self.root.join("project/config.toml"),
            Some(self.root.join("user/config.toml")),
        )
        .unwrap()
    }

    fn panel(&self) -> Panel {
        let data = PanelData {
            skills: vec![SkillRow {
                name: "pdf".into(),
                description: "fill and read PDF forms".into(),
                source: "user".into(),
            }],
            mcp_status: Default::default(),
            tools: vec![
                ("bash".into(), Policy::Ask),
                ("read".into(), Policy::Allow),
                ("skill".into(), Policy::Allow),
            ],
            ..PanelData::default()
        };
        Panel::with_agent_dirs(
            self.layers(),
            self.store.clone(),
            data,
            Some(self.root.join("user/agents")),
            self.root.join("project/agents"),
        )
        .with_mode_dirs(Some(self.root.join("user/modes")), self.root.join("project/modes"))
    }

    fn user_config(&self) -> String {
        std::fs::read_to_string(self.root.join("user/config.toml")).unwrap_or_default()
    }
}

const TWO_PROVIDERS: &str = "[providers.claude]\nkind = \"anthropic\"\n\n[providers.local]\nkind = \"ollama\"\nbase_url = \"http://localhost:11434\"\n\n[profiles.default]\nprovider = \"claude\"\nmodel = \"claude-sonnet-5-5\"\nfallback = [\"local\"]\n\n[permissions]\nbash = \"deny\"\n";

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn typed(text: &str) -> Vec<KeyEvent> {
    text.chars().map(|c| key(KeyCode::Char(c))).collect()
}

fn app_with_panel(panel: Panel) -> App {
    let mut app = App::new("m");
    app.theme.banner = false;
    app.mode = Mode::Panel(Box::new(panel));
    app
}

#[test]
fn snapshot_the_providers_tab() {
    let fx = Fixture::new(TWO_PROVIDERS);
    fx.store.save("claude", KEY).unwrap();
    let app = app_with_panel(fx.panel());
    let out = rows(100, 14, &app);
    snapshot("panel_providers", &out);
    assert!(!out.join("\n").contains(KEY));
}

#[test]
fn snapshot_the_models_tab() {
    let fx = Fixture::new(TWO_PROVIDERS);
    let app = app_with_panel(fx.panel().open_at(Tab::Models));
    snapshot("panel_models", &rows(100, 24, &app));
}

#[test]
fn snapshot_the_permissions_tab() {
    let fx = Fixture::new(TWO_PROVIDERS);
    let app = app_with_panel(fx.panel().open_at(Tab::Permissions));
    snapshot("panel_permissions", &rows(100, 12, &app));
}

#[test]
fn snapshot_the_general_tab() {
    let fx = Fixture::new(TWO_PROVIDERS);
    let app = app_with_panel(fx.panel().open_at(Tab::General));
    snapshot("panel_general", &rows(110, 24, &app));
}

#[test]
fn snapshot_the_masked_key_prompt_in_the_panel() {
    let fx = Fixture::new(TWO_PROVIDERS);
    let mut panel = fx.panel();
    panel.on_key(key(KeyCode::Char('k')));
    for k in typed(KEY) {
        panel.on_key(k);
    }
    let app = app_with_panel(panel);
    let out = rows(100, 16, &app);
    snapshot("panel_masked_prompt", &out);
    let text = out.join("\n");
    assert!(!text.contains(KEY));
    assert!(!text.contains("test-key"));
    assert!(text.contains(&"•".repeat(KEY.len())));
}

#[test]
fn snapshot_the_masked_login_prompt_in_chat() {
    let mut app = App::new("m");
    app.theme.banner = false;
    let mut input = MaskedInput::new();
    input.paste(KEY);
    app.overlay = Some(Overlay::Secret {
        input,
        provider: "openai".into(),
    });
    let out = rows(90, 16, &app);
    snapshot("chat_masked_login", &out);
    assert!(!out.join("\n").contains(KEY));
}

#[test]
fn snapshot_the_command_popup_filtering_as_you_type() {
    let mut app = App::new("m");
    app.theme.banner = false;
    for k in typed("/mo") {
        app.on_key(k);
    }
    assert!(app.popup_visible());
    snapshot("chat_popup_mo", &rows(90, 14, &app));

    for k in typed("d") {
        app.on_key(k);
    }
    let names: Vec<String> = app.popup_matches().iter().map(|e| e.name.clone()).collect();
    assert_eq!(names[0], "model");
}

#[test]
fn snapshot_the_full_command_popup() {
    let mut app = App::new("m");
    app.theme.banner = false;
    app.on_key(key(KeyCode::Char('/')));
    snapshot("chat_popup_all", &rows(90, 14, &app));
}

#[test]
fn snapshot_the_first_run_screen() {
    let fx = Fixture::new("");
    let mut app = App::new("");
    app.mode = Mode::Onboarding(Box::new(Onboarding::new(fx.layers(), fx.store.clone())));
    snapshot("onboarding_start", &rows(90, 26, &app));
}

#[test]
fn enter_in_the_popup_runs_the_highlighted_command() {
    let mut app = App::new("m");
    for k in typed("/mod") {
        app.on_key(k);
    }
    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Some(Action::Command("/model".into()))
    );
    assert!(app.input.is_empty());
}

#[test]
fn tab_completes_and_typing_arguments_closes_the_popup() {
    let mut app = App::new("m");
    for k in typed("/log") {
        app.on_key(k);
    }
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Tab));
    assert_eq!(app.input, "/logout ");
    assert!(!app.popup_visible());
    for k in typed("openai") {
        app.on_key(k);
    }
    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Some(Action::Command("/logout openai".into()))
    );
}

#[test]
fn custom_commands_and_skills_show_in_the_popup() {
    let mut app = App::new("m");
    app.entries = zarn_tui::popup::entries(&[("review".into(), String::new(), String::new())], &[("pdf".into(), "forms".into())]);
    for k in typed("/re") {
        app.on_key(k);
    }
    let names: Vec<String> = app.popup_matches().iter().map(|e| e.name.clone()).collect();
    assert!(names.contains(&"review".to_string()));
    assert!(names.contains(&"resume".to_string()));
}

#[test]
fn adding_a_provider_from_the_panel_writes_config_and_stores_the_key_elsewhere() {
    let fx = Fixture::new("");
    let mut panel = fx.panel();
    assert_eq!(panel.on_key(key(KeyCode::Char('a'))), None);
    assert_eq!(panel.on_key(key(KeyCode::Enter)), None);
    assert_eq!(panel.on_key(key(KeyCode::Enter)), None);
    assert_eq!(panel.on_key(key(KeyCode::Enter)), Some(Effect::Reload));
    for k in typed(KEY) {
        panel.on_key(k);
    }
    assert_eq!(panel.on_key(key(KeyCode::Enter)), Some(Effect::Reload));

    let written = fx.user_config();
    assert!(written.contains("[providers.anthropic]"));
    assert!(written.contains("provider = \"anthropic\""));
    assert!(!written.contains(KEY));
    assert!(!written.contains("api_key"));
    assert_eq!(fx.store.resolve("anthropic", None).unwrap().0, KEY);
    assert!(panel.message.as_ref().unwrap().0.contains("press t to test"));
}

#[test]
fn the_test_shortcut_marks_the_provider_running_and_asks_the_loop_to_test() {
    let fx = Fixture::new(TWO_PROVIDERS);
    let mut panel = fx.panel();
    assert_eq!(
        panel.on_key(key(KeyCode::Char('t'))),
        Some(Effect::Test("claude".into()))
    );
    panel.set_test("claude", zarn_tui::settings::TestState::Failed("authentication failed: invalid x-api-key".into()));
    let rendered = rows(120, 12, &app_with_panel(panel)).join("\n");
    assert!(rendered.contains("FAILED: authentication failed: invalid x-api-key"));
}

#[test]
fn permissions_cycle_and_persist_to_the_file_that_defines_them() {
    let fx = Fixture::new(TWO_PROVIDERS);
    let mut panel = fx.panel().open_at(Tab::Permissions);
    assert_eq!(panel.on_key(key(KeyCode::Enter)), Some(Effect::Reload));
    assert!(fx.user_config().contains("bash = \"allow\""));
    panel.on_key(key(KeyCode::Down));
    assert_eq!(panel.on_key(key(KeyCode::Char('d'))), Some(Effect::Reload));
    assert!(fx.user_config().contains("read = \"deny\""));
}

#[test]
fn fallback_order_can_be_reordered_with_brackets() {
    let fx = Fixture::new(
        "[providers.a]\nkind = \"ollama\"\n[providers.b]\nkind = \"ollama\"\n[providers.c]\nkind = \"ollama\"\n[profiles.default]\nprovider = \"a\"\nmodel = \"m\"\nfallback = [\"b\", \"c\"]\n",
    );
    let mut panel = fx.panel().open_at(Tab::Models);
    for _ in 0..3 {
        panel.on_key(key(KeyCode::Down));
    }
    assert_eq!(panel.on_key(key(KeyCode::Char(']'))), Some(Effect::Reload));
    assert!(fx.user_config().contains("fallback = [\n    \"c\",\n    \"b\",\n]") || fx.user_config().contains("fallback = [\"c\", \"b\"]"));
}

#[test]
fn toggling_a_skill_persists_and_esc_closes_the_panel() {
    let fx = Fixture::new("");
    let mut panel = fx.panel().open_at(Tab::Skills);
    assert_eq!(panel.on_key(key(KeyCode::Char(' '))), Some(Effect::Reload));
    assert!(fx.user_config().contains("pdf = false"));
    assert_eq!(panel.on_key(key(KeyCode::Esc)), Some(Effect::Close));
}

#[test]
fn the_auto_compact_toggle_persists_through_ops() {
    let fx = Fixture::new("");
    let mut panel = fx.panel().open_at(Tab::General);
    for _ in 0..3 {
        panel.on_key(key(KeyCode::Down));
    }
    assert_eq!(panel.on_key(key(KeyCode::Enter)), Some(Effect::Reload));
    assert!(fx.user_config().contains("auto_compact = false"));
}

#[test]
fn general_toggles_write_to_the_user_config() {
    let fx = Fixture::new("");
    let mut panel = fx.panel().open_at(Tab::General);
    panel.on_key(key(KeyCode::Down));
    assert_eq!(panel.on_key(key(KeyCode::Enter)), Some(Effect::Reload));
    assert!(fx.user_config().contains("banner = false"));
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

const REVIEW_MODE: &str = "---\nname: review\ndescription: read-only code review\ntools: read, glob, grep\npermissions: edit=deny, write=deny, bash=ask\n---\nReview the code.\n";

#[test]
fn snapshot_the_modes_tab() {
    let fx = Fixture::new(TWO_PROVIDERS);
    std::fs::create_dir_all(fx.root.join("project/modes")).unwrap();
    std::fs::write(fx.root.join("project/modes/review.md"), REVIEW_MODE).unwrap();
    let app = app_with_panel(fx.panel().open_at(Tab::Modes).with_active_mode("plan"));
    let out = rows(100, 14, &app);
    snapshot("panel_modes", &out);
    let text = out.join("\n");
    assert!(text.contains("active mode") && text.contains("plan"));
    assert!(text.contains("review") && text.contains("project"));
    assert!(text.contains("built-in"));
}

#[test]
fn a_custom_mode_is_created_edited_and_deleted_as_a_markdown_file() {
    let fx = Fixture::new(TWO_PROVIDERS);
    let before = fx.user_config();
    let mut panel = fx.panel().open_at(Tab::Modes);
    panel.on_key(key(KeyCode::Char('a')));
    for k in typed("docs") {
        panel.on_key(k);
    }
    panel.on_key(key(KeyCode::Down));
    for k in typed("writes documentation") {
        panel.on_key(k);
    }
    panel.on_key(key(KeyCode::Down));
    panel.on_key(key(KeyCode::Down));
    for k in typed("bash=deny") {
        panel.on_key(k);
    }
    for _ in 0..4 {
        panel.on_key(key(KeyCode::Down));
    }
    for k in typed("Write docs.") {
        panel.on_key(k);
    }
    assert_eq!(panel.on_key(ctrl('s')), Some(Effect::Reload));
    let path = fx.root.join("user/modes/docs.md");
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("name: docs"));
    assert!(saved.contains("permissions: bash=deny"));
    assert!(saved.contains("Write docs."));
    assert_eq!(fx.user_config(), before, "config.toml must not change");

    let row = panel.rows().iter().position(|r| r.label == "docs").unwrap();
    panel.cursor[Tab::Modes.index()] = row;
    panel.on_key(key(KeyCode::Enter));
    panel.on_key(key(KeyCode::Down));
    for _ in 0.."writes documentation".len() {
        panel.on_key(key(KeyCode::Backspace));
    }
    for k in typed("edited") {
        panel.on_key(k);
    }
    assert_eq!(panel.on_key(ctrl('s')), Some(Effect::Reload));
    assert!(std::fs::read_to_string(&path).unwrap().contains("description: edited"));

    let row = panel.rows().iter().position(|r| r.label == "docs").unwrap();
    panel.cursor[Tab::Modes.index()] = row;
    panel.on_key(key(KeyCode::Char('d')));
    assert_eq!(panel.on_key(key(KeyCode::Char('y'))), Some(Effect::Reload));
    assert!(!path.exists());
    assert_eq!(fx.user_config(), before);
}

#[test]
fn a_mode_with_a_builtin_name_cannot_be_created_and_builtins_cannot_be_deleted() {
    let fx = Fixture::new("");
    let mut panel = fx.panel().open_at(Tab::Modes);
    panel.on_key(key(KeyCode::Char('a')));
    for k in typed("plan") {
        panel.on_key(k);
    }
    assert_eq!(panel.on_key(ctrl('s')), None);
    assert!(panel.message.as_ref().unwrap().0.contains("built-in"));
    panel.on_key(key(KeyCode::Esc));
    panel.cursor[Tab::Modes.index()] = 1;
    panel.on_key(key(KeyCode::Char('d')));
    assert!(panel.message.as_ref().unwrap().0.contains("built in"));
    assert!(panel.editor.is_none());
}

#[test]
fn choosing_the_active_mode_writes_only_general_mode() {
    let fx = Fixture::new("");
    let mut panel = fx.panel().open_at(Tab::Modes);
    panel.on_key(key(KeyCode::Enter));
    for k in typed("plan") {
        panel.on_key(k);
    }
    assert_eq!(panel.on_key(key(KeyCode::Enter)), Some(Effect::SetMode("plan".into())));
    assert_eq!(fx.user_config().trim(), "[general]\nmode = \"plan\"");
}

#[test]
fn the_models_tab_lists_roles_and_toggles_and_sets_routing() {
    let fx = Fixture::new(TWO_PROVIDERS);
    let mut panel = fx.panel().open_at(Tab::Models);
    let rows = panel.rows();
    for role in zarn_config::ROLES {
        assert!(rows.iter().any(|r| r.label == format!("role · {role}")), "{role}");
    }
    let routing = rows.iter().position(|r| r.label == "routing").unwrap();
    panel.cursor[Tab::Models.index()] = routing;
    assert_eq!(panel.on_key(key(KeyCode::Enter)), Some(Effect::Reload));
    assert!(fx.user_config().contains("[routing]\nenabled = true"));

    let compact = panel.rows().iter().position(|r| r.label == "role · compact").unwrap();
    panel.cursor[Tab::Models.index()] = compact;
    panel.on_key(key(KeyCode::Enter));
    for k in typed("local") {
        panel.on_key(k);
    }
    panel.on_key(key(KeyCode::Enter));
    for k in typed("qwen3:8b") {
        panel.on_key(k);
    }
    assert_eq!(panel.on_key(key(KeyCode::Enter)), Some(Effect::Reload));
    let saved = fx.user_config();
    assert!(saved.contains("[routing.roles.compact]"), "{saved}");
    assert!(saved.contains("provider = \"local\""));
    assert!(saved.contains("model = \"qwen3:8b\""));
    assert_eq!(panel.rows()[compact].value, "local · qwen3:8b");

    assert_eq!(panel.on_key(key(KeyCode::Char('d'))), Some(Effect::Reload));
    assert!(!fx.user_config().contains("roles.compact"));
}

fn report(provider: &str) -> zarn_config::quota::Report {
    use zarn_providers::discovery::{Credits, Limits, ModelInfo, Window};
    zarn_config::quota::Report {
        provider: provider.into(),
        kind: "anthropic".into(),
        fetched_at: zarn_config::quota::now() - 150,
        models: Ok(vec![
            ModelInfo {
                id: "claude-sonnet-5-5".into(),
                context_window: Some(1_000_000),
                ..ModelInfo::default()
            },
            ModelInfo {
                id: "claude-haiku-5-5".into(),
                ..ModelInfo::default()
            },
        ]),
        limits: Ok(Limits::Found {
            requests: Window {
                limit: Some("50".into()),
                remaining: Some("49".into()),
                reset: None,
            },
            tokens: Window::default(),
        }),
        credits: Ok(Credits::NotExposed),
    }
}

fn quota_panel(state: zarn_tui::settings::QuotaState) -> App {
    let fx = Fixture::new(TWO_PROVIDERS);
    let mut panel = fx.panel().open_at(Tab::Quota);
    panel.set_quota("claude", state);
    app_with_panel(panel)
}

#[test]
fn snapshot_the_quota_tab_loaded() {
    let app = quota_panel(zarn_tui::settings::QuotaState::Loaded(Box::new(report("claude"))));
    let out = rows(110, 12, &app);
    snapshot("panel_quota_loaded", &out);
    let text = out.join("\n");
    assert!(text.contains("claude-sonnet-5-5 (1M ctx)"));
    assert!(text.contains("requests 49/50"));
    assert!(text.contains("tokens unknown/unknown"));
    assert!(text.contains("not exposed by this provider"));
}

#[test]
fn snapshot_the_quota_tab_loading() {
    let app = quota_panel(zarn_tui::settings::QuotaState::Loading);
    let out = rows(110, 8, &app);
    snapshot("panel_quota_loading", &out);
    assert!(out.join("\n").contains("loading ⠋"));
}

#[test]
fn snapshot_the_quota_tab_failed() {
    let app = quota_panel(zarn_tui::settings::QuotaState::Failed(
        "claude: the key was rejected (HTTP 401)".into(),
    ));
    let out = rows(110, 8, &app);
    snapshot("panel_quota_failed", &out);
    assert!(out.join("\n").contains("FAILED: claude: the key was rejected (HTTP 401)"));
}

#[test]
fn refreshing_a_quota_row_shows_loading_and_asks_the_loop() {
    let fx = Fixture::new(TWO_PROVIDERS);
    let mut panel = fx.panel().open_at(Tab::Quota);
    panel.set_quota("claude", zarn_tui::settings::QuotaState::Loaded(Box::new(report("claude"))));
    panel.cursor[Tab::Quota.index()] = 2;
    assert_eq!(
        panel.on_key(key(KeyCode::Char('r'))),
        Some(Effect::RefreshQuota(Some("claude".into())))
    );
    assert!(panel.loading_quota());
}

#[test]
fn the_providers_tab_shows_health_from_real_use() {
    let fx = Fixture::new(
        "[providers.healthy-one]\nkind = \"ollama\"\nbase_url = \"http://localhost:11434\"\n",
    );
    zarn_config::health::record_request("healthy-one", std::time::Duration::from_millis(250), true);
    zarn_config::health::record_request("healthy-one", std::time::Duration::from_millis(250), false);
    zarn_config::health::record_test("healthy-one", std::time::Duration::from_millis(180), true);
    let rows = fx.panel().rows();
    assert_eq!(
        rows[0].note,
        "test ok 0s ago · 180 ms · 1 failure since start"
    );
}

#[test]
fn model_pickers_merge_discovered_models_with_their_context_window() {
    let fx = Fixture::new(TWO_PROVIDERS);
    let mut panel = fx.panel().open_at(Tab::Models);
    let row = panel.rows().iter().position(|r| r.label == "default · model").unwrap();
    panel.cursor[Tab::Models.index()] = row;
    panel.on_key(key(KeyCode::Enter));
    panel.set_models(
        "default",
        vec![("claude-sonnet-5-5".into(), Some(1_000_000)), ("claude-next".into(), Some(200_000))],
    );
    match &panel.editor {
        Some(zarn_tui::settings::Editor::Pick(picker, _)) => {
            let sonnet = picker.choices.iter().find(|c| c.value == "claude-sonnet-5-5").unwrap();
            assert_eq!(sonnet.detail, "1M ctx");
            let next = picker.choices.iter().find(|c| c.value == "claude-next").unwrap();
            assert_eq!(next.detail, "200k ctx");
        }
        _ => panic!("expected the model picker"),
    }
    for k in typed("a-model-nobody-listed") {
        panel.on_key(k);
    }
    assert_eq!(panel.on_key(key(KeyCode::Enter)), Some(Effect::Reload));
    assert!(fx.user_config().contains("model = \"a-model-nobody-listed\""));
}

#[test]
fn the_permissions_tab_lists_rules_and_deletes_them() {
    let fx = Fixture::new(&format!(
        "{TWO_PROVIDERS}allow = [\"bash(git diff:*)\"]\ndeny = [\"bash(rm -rf:*)\"]\n"
    ));
    let mut panel = fx.panel().open_at(Tab::Permissions);
    let rows = panel.rows();
    let deny = rows.iter().position(|r| r.value == "bash(rm -rf:*)").unwrap();
    assert_eq!(rows[deny].label, "rule · deny");
    assert_eq!(rows[deny].note, "user");
    let app = app_with_panel(fx.panel().open_at(Tab::Permissions));
    snapshot("panel_permission_rules", &rows_of(&app));
    let allow = rows.iter().position(|r| r.value == "bash(git diff:*)").unwrap();
    panel.cursor[Tab::Permissions.index()] = allow;
    assert_eq!(panel.on_key(key(KeyCode::Char('d'))), Some(Effect::Reload));
    let saved = fx.user_config();
    assert!(!saved.contains("git diff"), "{saved}");
    assert!(saved.contains("rm -rf"));
}

fn rows_of(app: &App) -> Vec<String> {
    rows(100, 14, app)
}

#[test]
fn snapshot_the_hooks_tab() {
    let fx = Fixture::new(&format!(
        "{TWO_PROVIDERS}\n[hooks]\ntimeout_secs = 10\n\n[[hooks.pre_tool]]\nmatch = \"edit|write\"\ncommand = \"./scripts/check-paths.sh\"\n\n[[hooks.turn_end]]\ncommand = \"notify-send done\"\n"
    ));
    let app = app_with_panel(fx.panel().open_at(Tab::Hooks));
    let out = rows(100, 10, &app);
    snapshot("panel_hooks", &out);
    let text = out.join("\n");
    assert!(text.contains("pre_tool · edit|write"));
    assert!(text.contains("./scripts/check-paths.sh"));
    assert!(text.contains("10 seconds"));
}

fn panel_key(panel: &mut Panel, code: KeyCode) -> Option<Effect> {
    panel.on_key(KeyEvent::new(code, KeyModifiers::NONE))
}

#[test]
fn snapshot_the_status_line_form_previews_live_and_refuses_bad_templates() {
    let fx = Fixture::new(&format!("[general]\nstatus_line = \"{{model}}\"\n\n{TWO_PROVIDERS}"));
    let mut panel = fx.panel().open_at(Tab::General).with_status_values(zarn_tui::status_line::Values {
        model: "gpt-x".into(),
        mode: "plan".into(),
        profile: "work".into(),
        context: "42%".into(),
        cost: "$0.0120".into(),
        cwd: "~/src/app".into(),
        branch: "main".into(),
    });
    let at = panel.rows().iter().position(|r| r.label == "status line").unwrap();
    for _ in 0..at {
        panel_key(&mut panel, KeyCode::Down);
    }
    panel_key(&mut panel, KeyCode::Enter);
    for _ in 0.."{model}".len() {
        panel_key(&mut panel, KeyCode::Backspace);
    }
    panel.paste("{model} · {mode} · {branch} · {context}");
    let app = app_with_panel(panel);
    let shown = rows(100, 24, &app);
    snapshot("panel_status_line_preview", &shown);
    assert!(shown.join("\n").contains("preview  gpt-x · plan · main · 42%"));

    let Mode::Panel(mut panel) = app.mode else { unreachable!() };
    for _ in 0..40 {
        panel_key(&mut panel, KeyCode::Backspace);
    }
    panel.paste("{model} {oops}");
    let app = app_with_panel(*panel);
    assert!(rows(100, 24, &app).join("\n").contains("{oops} is not a status line field"));
    let Mode::Panel(mut panel) = app.mode else { unreachable!() };
    panel_key(&mut panel, KeyCode::Enter);
    assert!(panel.message.as_ref().unwrap().0.contains("the previous status line is kept"));
    assert!(fx.user_config().contains("status_line = \"{model}\""));
}

#[test]
fn snapshot_the_plugins_tab_and_approve_a_plugins_code() {
    let fx = Fixture::new(TWO_PROVIDERS);
    let source = fx.root.join("kit-src");
    std::fs::create_dir_all(source.join("commands")).unwrap();
    std::fs::write(
        source.join("plugin.toml"),
        "name = \"kit\"\ndescription = \"sample\"\n\n[[hooks.pre_tool]]\ncommand = \"echo checking\"\n",
    )
    .unwrap();
    std::fs::write(source.join("commands/fix.md"), "Fix it").unwrap();
    let store = zarn_config::plugins::Store::at(&fx.root.join("user"));
    store.install(source.to_str().unwrap()).unwrap();

    let panel = fx.panel().with_plugin_root(fx.root.join("user")).open_at(Tab::Plugins);
    let (mut panel, shown) = show(panel, 100, 10);
    snapshot("panel_plugins", &shown);
    assert!(shown.join("\n").contains("kit  enabled; hooks and MCP wait for approval  1 commands, 1 hooks"), "{shown:?}");

    panel_key(&mut panel, KeyCode::Enter);
    let (mut panel, shown) = show(panel, 100, 16);
    let shown = shown.join("\n");
    assert!(shown.contains("Allow plugin kit to run this code on your machine?"), "{shown}");
    assert!(shown.contains("hook pre_tool: echo checking"), "{shown}");
    assert_eq!(panel.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)), Some(Effect::Reload));
    assert!(store.find("kit").unwrap().approved());
    let (mut panel, shown) = show(panel, 100, 10);
    assert!(shown.join("\n").contains("hooks and MCP approved"));

    panel.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
    panel.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    assert!(store.installed().is_empty());
}

fn show(panel: Panel, width: u16, height: u16) -> (Panel, Vec<String>) {
    let app = app_with_panel(panel);
    let shown = rows(width, height, &app);
    let Mode::Panel(panel) = app.mode else { unreachable!() };
    (*panel, shown)
}

#[test]
fn the_mcp_form_takes_references_and_refuses_literal_secrets() {
    let fx = Fixture::new(TWO_PROVIDERS);
    let mut panel = fx.panel().open_at(Tab::Mcp);
    let at = panel.rows().iter().position(|r| r.kind == zarn_tui::settings::RowKind::AddMcp).unwrap();
    for _ in 0..at {
        panel_key(&mut panel, KeyCode::Down);
    }
    let fill = |panel: &mut Panel, env: &str| {
        panel_key(panel, KeyCode::Enter);
        panel.paste("gh");
        panel_key(panel, KeyCode::Down);
        panel.paste("gh-mcp --stdio");
        panel_key(panel, KeyCode::Down);
        panel.paste(env);
        panel.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
    };
    let literal = format!("GITHUB_TOKEN=sk-{}", "z8".repeat(14));
    assert_eq!(fill(&mut panel, &literal), None);
    let (text, failed) = panel.message.clone().unwrap();
    assert!(failed && text.contains("looks like a openai secret"), "{text}");
    assert!(!fx.user_config().contains("sk-"));

    panel.editor = None;
    assert_eq!(fill(&mut panel, "GITHUB_TOKEN=key:gh-token, LOG=debug"), Some(Effect::RestartMcp("gh".into())));
    let saved = fx.user_config();
    assert!(saved.contains("GITHUB_TOKEN = \"key:gh-token\""), "{saved}");
    assert!(saved.contains("LOG = \"debug\""));
}
