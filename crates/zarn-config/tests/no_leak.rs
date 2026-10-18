use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use zarn_config::auth::{KeyStore, MemoryBackend};
use zarn_config::guard::{RedactingWriter, append_redacted, redacting};
use zarn_config::layers::Layers;
use zarn_config::ops::{self, ProviderDraft};
use zarn_core::session::Session;
use zarn_core::tools::{Ctx, Risk, Tool, ToolOutput};
use zarn_core::types::{Message, ToolCall, ToolSpec};

const KEY: &str = "test-key-not-real-leak-0123456789";

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn files_containing(dir: &Path, needle: &str) -> Vec<String> {
    let mut all = Vec::new();
    walk(dir, &mut all);
    all.into_iter()
        .filter(|p| p.file_name().is_none_or(|n| n != "auth.json"))
        .filter(|p| {
            std::fs::read(p)
                .map(|b| String::from_utf8_lossy(&b).contains(needle))
                .unwrap_or(false)
        })
        .map(|p| p.display().to_string())
        .collect()
}

struct EchoEnv;

#[async_trait::async_trait]
impl Tool for EchoEnv {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".into(),
            description: String::new(),
            schema: serde_json::Value::Null,
        }
    }

    fn risk(&self) -> Risk {
        Risk::Execute
    }

    async fn run(&self, _args: serde_json::Value, _ctx: &Ctx) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::ok(format!("OPENAI_API_KEY={KEY}")))
    }
}

#[tokio::test]
async fn after_a_login_no_key_reaches_config_logs_or_session_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut layers = Layers::load_from(
        root.join("project/.zarn/config.toml"),
        Some(root.join("user/config.toml")),
    )
    .unwrap();
    let store = KeyStore::new(Arc::new(MemoryBackend::new()), root.join("user/auth.json"));

    ops::add_provider(
        &mut layers,
        &ProviderDraft {
            name: "work".into(),
            kind: "openai".into(),
            model: Some("gpt-5".into()),
            ..ProviderDraft::default()
        },
    )
    .unwrap();
    ops::login(&mut layers, &store, "work", KEY).unwrap();
    ops::set_profile_model(&mut layers, "default", "gpt-5-mini").unwrap();
    ops::set_permission(&mut layers, "bash", zarn_tools::permissions::Policy::Ask).unwrap();
    ops::update_provider(&mut layers, "work", |spec| {
        spec.api_key = Some(KEY.to_string());
    })
    .unwrap();

    let cfg = layers.merged();
    let chain = cfg.chain_for(&store, "default").unwrap();
    assert_eq!(chain[0].api_key.as_deref(), Some(KEY));

    let log_path = root.join("data/zarn.log");
    std::fs::create_dir_all(log_path.parent().unwrap()).unwrap();
    {
        let file = std::fs::File::create(&log_path).unwrap();
        let mut log = RedactingWriter::new(file);
        writeln!(log, "WARN provider failed: 401 for key {KEY}").unwrap();
        writeln!(log, "DEBUG spec {:?}", chain[0]).unwrap();
        writeln!(log, "DEBUG header authorization: Bearer {KEY}").unwrap();
    }

    let tool = redacting(Box::new(EchoEnv));
    let output = tool
        .run(serde_json::Value::Null, &Ctx::new(root))
        .await
        .unwrap();
    assert!(!output.content.contains(KEY));

    let mut session = Session::create(&root.join("data/sessions"), root, "gpt-5", None).unwrap();
    let messages = vec![
        Message::user(format!("my key is {KEY}, keep it safe")),
        Message::Assistant {
            content: vec![],
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                args: serde_json::json!({ "command": format!("curl -H 'x-api-key: {KEY}'") }),
            }],
        },
        Message::ToolResult {
            id: "c1".into(),
            content: format!("OPENAI_API_KEY={KEY}"),
            is_error: false,
        },
        Message::assistant(format!("Your key {KEY} works.")),
    ];
    append_redacted(&mut session, &messages).unwrap();

    let leaks = files_containing(root, KEY);
    assert!(leaks.is_empty(), "the key leaked into {leaks:?}");

    let config = std::fs::read_to_string(root.join("user/config.toml")).unwrap();
    assert!(config.contains("[providers.work]"));
    assert!(!config.contains("api_key ="));
    let session_text = std::fs::read_to_string(session.path()).unwrap();
    assert!(session_text.contains("[redacted]"));
    assert!(session_text.contains("keep it safe"));
}
