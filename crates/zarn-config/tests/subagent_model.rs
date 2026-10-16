mod support;

use std::sync::{Arc, Mutex};
use support::*;
use zarn_config::runtime::{self, Providers, Router};
use zarn_core::agent::AgentConfig;
use zarn_core::subagent::{AgentDef, TaskTool};
use zarn_core::tools::{Ctx, Tool, ToolRegistry};


fn two_providers(a: &str, b: &str) -> String {
    format!(
        "[providers.main]\nkind = \"openai-compat\"\nbase_url = \"{a}\"\n\n[providers.other]\nkind = \"openai-compat\"\nbase_url = \"{b}\"\n\n[profiles.default]\nprovider = \"main\"\nmodel = \"main-model\"\n\n[profiles.fast]\nprovider = \"other\"\nmodel = \"x\"\n"
    )
}

fn task(providers: &Providers, model: Option<&str>) -> TaskTool {
    TaskTool::new(
        vec![AgentDef {
            name: "helper".into(),
            description: "helps".into(),
            tools: Vec::new(),
            model: model.map(str::to_string),
            instructions: "be brief".into(),
        }],
        Router::new(providers.clone(), "main").factory(),
        Arc::new(|_: &[String]| ToolRegistry::new()),
        Arc::new(Ctx::new(".")),
        AgentConfig {
            model: providers.main_model.clone(),
            ..AgentConfig::default()
        },
    )
}

async fn providers_for(cfg_text: &str, notices: Arc<Mutex<Vec<String>>>) -> (tempfile::TempDir, Providers) {
    let cfg = config(cfg_text);
    let (dir, store) = store();
    let plan = runtime::plan(&cfg, &store, "default", None).unwrap();
    let main = zarn_config::guard::guarded_named(&plan.entries(), None).unwrap();
    let notify: runtime::Notify = Arc::new(move |t: String| notices.lock().unwrap().push(t));
    (
        dir,
        Providers {
            cfg,
            store,
            main,
            main_model: plan.model,
            notify: Some(notify),
        },
    )
}

#[tokio::test]
async fn an_agent_naming_a_model_is_served_by_the_provider_that_owns_it() {
    let a = openai_server("from main").await;
    let b = openai_server("from other").await;
    let (_d, providers) = providers_for(&two_providers(&a.uri(), &b.uri()), Default::default()).await;

    let out = task(&providers, Some("x"))
        .run(serde_json::json!({ "agent": "helper", "prompt": "go" }), &Ctx::new("."))
        .await
        .unwrap();

    assert_eq!(out.content, "from other");
    assert_eq!(models(&b).await, vec!["x".to_string()]);
    assert!(models(&a).await.is_empty());
}

#[tokio::test]
async fn an_agent_without_a_model_uses_the_main_provider_and_model() {
    let a = openai_server("from main").await;
    let b = openai_server("from other").await;
    let (_d, providers) = providers_for(&two_providers(&a.uri(), &b.uri()), Default::default()).await;

    task(&providers, None)
        .run(serde_json::json!({ "agent": "helper", "prompt": "go" }), &Ctx::new("."))
        .await
        .unwrap();

    assert_eq!(models(&a).await, vec!["main-model".to_string()]);
    assert!(models(&b).await.is_empty());
}

#[tokio::test]
async fn a_provider_prefixed_model_goes_to_that_provider() {
    let a = openai_server("from main").await;
    let b = openai_server("from other").await;
    let (_d, providers) = providers_for(&two_providers(&a.uri(), &b.uri()), Default::default()).await;

    task(&providers, Some("other:y-large"))
        .run(serde_json::json!({ "agent": "helper", "prompt": "go" }), &Ctx::new("."))
        .await
        .unwrap();

    assert_eq!(models(&b).await, vec!["y-large".to_string()]);
}

#[tokio::test]
async fn an_unknown_model_name_is_sent_to_the_main_provider_unchanged() {
    let a = openai_server("from main").await;
    let b = openai_server("from other").await;
    let (_d, providers) = providers_for(&two_providers(&a.uri(), &b.uri()), Default::default()).await;

    task(&providers, Some("brand-new"))
        .run(serde_json::json!({ "agent": "helper", "prompt": "go" }), &Ctx::new("."))
        .await
        .unwrap();

    assert_eq!(models(&a).await, vec!["brand-new".to_string()]);
}

#[tokio::test]
async fn a_model_whose_provider_has_no_key_falls_back_to_main_with_a_notice() {
    let a = openai_server("from main").await;
    let cfg = format!(
        "[providers.main]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[providers.claude]\nkind = \"anthropic\"\n\n[profiles.default]\nprovider = \"main\"\nmodel = \"main-model\"\n",
        a.uri()
    );
    let notices: Arc<Mutex<Vec<String>>> = Default::default();
    let (_d, providers) = providers_for(&cfg, notices.clone()).await;

    task(&providers, Some("claude:claude-haiku-5-5"))
        .run(serde_json::json!({ "agent": "helper", "prompt": "go" }), &Ctx::new("."))
        .await
        .unwrap();

    assert_eq!(models(&a).await, vec!["main-model".to_string()]);
    let notices = notices.lock().unwrap();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("claude has no API key"));
}
