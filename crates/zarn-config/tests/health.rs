mod support;

use futures::StreamExt;
use support::*;
use zarn_config::guard::guarded_named;
use zarn_config::health;
use zarn_core::types::{Message, Request};
use zarn_providers::ProviderSpec;

fn spec(base: &str) -> ProviderSpec {
    ProviderSpec {
        kind: "openai-compat".into(),
        base_url: Some(base.into()),
        ..ProviderSpec::default()
    }
}

#[tokio::test]
async fn real_requests_record_latency_and_failures_per_provider() {
    let good = openai_server("hi").await;
    let bad = failing_server(500).await;
    let chain = guarded_named(
        &[
            ("health-bad".to_string(), spec(&bad.uri())),
            ("health-good".to_string(), spec(&good.uri())),
        ],
        None,
    )
    .unwrap();
    let mut stream = chain
        .stream(Request::new("m", vec![Message::user("x")]))
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let good = health::get("health-good").unwrap();
    assert_eq!(good.requests, 1);
    assert_eq!(good.failures, 0);
    assert!(good.latency.is_some());
    let bad = health::get("health-bad").unwrap();
    assert_eq!(bad.failures, 1);
    assert!(health::summary("health-bad").contains("1 failure since start"));
    assert!(health::summary("health-good").starts_with("not tested · "));
}

#[tokio::test]
async fn an_explicit_test_records_its_result_and_age() {
    let good = openai_server("OK").await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("u.toml"),
        format!(
            "[providers.health-tested]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[profiles.default]\nprovider = \"health-tested\"\nmodel = \"m\"\n",
            good.uri()
        ),
    )
    .unwrap();
    let layers = zarn_config::layers::Layers::load_from(dir.path().join("p.toml"), Some(dir.path().join("u.toml"))).unwrap();
    let (_d, store) = store();
    zarn_config::ops::test_named(&layers, &store, "health-tested").await.unwrap();
    assert!(health::summary("health-tested").starts_with("test ok 0s ago · "));
}
