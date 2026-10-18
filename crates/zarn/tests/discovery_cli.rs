mod support;

use std::sync::Arc;
use support::*;

fn models_mock() -> Mock {
    Mock::start(Arc::new(|seen: &Seen| {
        if seen.line.starts_with("GET") {
            (
                200,
                "application/json".into(),
                "{\"data\":[{\"id\":\"m\",\"context_window\":131072},{\"id\":\"m-mini\"}]}".into(),
            )
        } else {
            (200, "text/event-stream".into(), sse("ok"))
        }
    }))
}

#[test]
fn zarn_models_prints_models_limits_and_credits_using_the_cache() {
    let mock = models_mock();
    let home = Home::new();
    home.config(&format!(
        "[providers.mock]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[profiles.default]\nprovider = \"mock\"\nmodel = \"m\"\n",
        mock.base()
    ));
    let out = home.run(&["models"], "");
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("mock (openai-compat) · fetched"), "{text}");
    assert!(text.contains("m (131k ctx), m-mini"), "{text}");
    assert!(text.contains("credits  not exposed by this provider"));
    let gets = |m: &Mock| m.requests().iter().filter(|r| r.line.starts_with("GET")).count();
    assert_eq!(gets(&mock), 1);

    let again = home.run(&["models", "--provider", "mock"], "");
    assert!(again.status.success());
    assert_eq!(gets(&mock), 1, "served from the cache");

    let refreshed = home.run(&["models", "--provider", "mock", "--refresh"], "");
    assert!(refreshed.status.success());
    assert_eq!(gets(&mock), 2);

    let missing = home.run(&["models", "--provider", "ghost"], "");
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("no provider named 'ghost'"));
}

#[test]
fn zarn_limits_sets_and_shows_the_caps() {
    let home = Home::new();
    let out = home.run(&["limits", "--session", "5", "--day", "20"], "");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("session cap  $5.00"));
    let config = std::fs::read_to_string(home.root().join("cfg/config.toml")).unwrap();
    assert!(config.contains("session_usd = 5.0") && config.contains("day_usd = 20.0"));
    let off = home.run(&["limits", "--day", "off"], "");
    assert!(stdout(&off).contains("daily cap    off"));
}

#[test]
fn print_mode_stops_at_the_daily_cap() {
    let mock = Mock::openai("ok");
    let home = Home::new();
    home.config(&format!(
        "[pricing.m]\ninput = 1000.0\noutput = 1000.0\n\n[limits]\nday_usd = 0.01\n\n[providers.mock]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[profiles.default]\nprovider = \"mock\"\nmodel = \"m\"\n",
        mock.base()
    ));
    let first = home.run(&["--print", "hi"], "");
    assert!(stderr(&first).contains("stopped: the daily spend cap"), "{}", stderr(&first));
    let second = home.run(&["--print", "hi"], "");
    assert!(!second.status.success());
    assert!(stderr(&second).contains("day_usd"));
    assert_eq!(mock.requests().len(), 1);
}
