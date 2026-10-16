mod support;

use std::time::{Duration, Instant};
use support::*;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};
use zarn_config::quota;

const KEY: &str = "test-key-not-real-quota-cache";

async fn models_server(delay_ms: u64) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("{\"data\":[{\"id\":\"m\",\"context_window\":32000}]}")
                .set_delay(Duration::from_millis(delay_ms)),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("{}")
                .insert_header("x-ratelimit-limit-requests", "100")
                .insert_header("x-ratelimit-remaining-requests", "99")
                .set_delay(Duration::from_millis(delay_ms)),
        )
        .mount(&server)
        .await;
    server
}

fn two(a: &str, b: &str) -> String {
    format!(
        "[providers.alpha]\nkind = \"openai-compat\"\nbase_url = \"{a}\"\n\n[providers.beta]\nkind = \"openai-compat\"\nbase_url = \"{b}\"\n\n[profiles.default]\nprovider = \"alpha\"\nmodel = \"m\"\n"
    )
}

#[tokio::test]
async fn reports_are_cached_without_keys_and_refreshed_on_demand() {
    let a = models_server(0).await;
    let b = models_server(0).await;
    let cfg = config(&two(&a.uri(), &b.uri()));
    let (dir, store) = store();
    store.save("alpha", KEY).unwrap();
    let cache = dir.path().join("cache");

    let first = quota::reports(&cfg, &store, None, false, &cache).await;
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].models().first().unwrap().context_window, Some(32_000));
    let hits = a.received_requests().await.unwrap().len();
    assert!(hits >= 2);

    for name in ["alpha", "beta"] {
        let body = std::fs::read_to_string(cache.join(format!("{name}.json"))).unwrap();
        assert!(!body.contains(KEY), "the key leaked into the cache");
        assert!(!body.contains("not-real"));
        assert!(body.contains("fetched_at"));
    }

    quota::reports(&cfg, &store, None, false, &cache).await;
    assert_eq!(a.received_requests().await.unwrap().len(), hits, "cache reused");

    quota::reports(&cfg, &store, Some("alpha"), true, &cache).await;
    assert!(a.received_requests().await.unwrap().len() > hits, "refresh hits the server");
}

#[tokio::test]
async fn one_failing_provider_does_not_stop_the_others_and_they_run_concurrently() {
    let slow_a = models_server(700).await;
    let slow_b = models_server(700).await;
    let broken = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&broken)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&broken)
        .await;
    let cfg = config(&format!(
        "{}\n[providers.gamma]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n",
        two(&slow_a.uri(), &slow_b.uri()),
        broken.uri()
    ));
    let (dir, store) = store();
    let started = Instant::now();
    let found = quota::reports(&cfg, &store, None, true, &dir.path().join("cache")).await;
    assert!(started.elapsed() < Duration::from_millis(1_300), "{:?}", started.elapsed());
    let gamma = found.iter().find(|r| r.provider == "gamma").unwrap();
    assert!(gamma.failed().unwrap().contains("gamma"));
    let text = quota::render(&found);
    assert!(text.contains("gamma (openai-compat)  FAILED"));
    assert!(text.contains("alpha (openai-compat) · fetched"));
    assert!(text.contains("requests 99/100"));
}

#[tokio::test]
async fn a_provider_that_hangs_is_cut_off_by_its_own_timeout() {
    let fast = models_server(0).await;
    let hung = models_server(5_000).await;
    let cfg = config(&two(&fast.uri(), &hung.uri()));
    let (dir, store) = store();
    let started = Instant::now();
    let found = quota::reports_within(
        &cfg,
        &store,
        None,
        true,
        &dir.path().join("cache"),
        Duration::from_millis(400),
    )
    .await;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(found[0].failed().is_none());
    assert!(found[1].failed().unwrap().contains("no answer within"));
}

#[tokio::test]
async fn providers_that_need_a_key_and_have_none_are_skipped() {
    let cfg = config("[providers.claude]\nkind = \"anthropic\"\n");
    let (dir, store) = store();
    assert!(quota::reports(&cfg, &store, None, true, dir.path()).await.is_empty());
}
