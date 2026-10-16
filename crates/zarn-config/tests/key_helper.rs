#![cfg(unix)]

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use support::*;
use zarn_config::auth::{KeySource, REDACTED, redact};

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn the_helper_prints_the_key_which_is_trimmed_and_redacted() {
    let (dir, store) = store();
    let helper = script(dir.path(), "key.sh", "printf '  test-key-not-real-from-helper\\n'");
    let found = store.resolve_with("p", None, Some(&format!("{} --flag", helper.display())));
    assert_eq!(
        found,
        Some(("test-key-not-real-from-helper".to_string(), KeySource::Helper))
    );
    assert_eq!(redact("got test-key-not-real-from-helper"), format!("got {REDACTED}"));
}

#[test]
fn the_environment_wins_over_the_helper_which_wins_over_the_store() {
    let (dir, store) = store();
    let helper = script(dir.path(), "key.sh", "echo test-key-not-real-helper-order");
    let command = helper.display().to_string();
    store.save("p", "test-key-not-real-stored-order").unwrap();
    let var = "ZARN_TEST_HELPER_ORDER_KEY";
    unsafe { std::env::set_var(var, "test-key-not-real-env-order") };
    assert_eq!(
        store.resolve_with("p", Some(var), Some(&command)).unwrap().1,
        KeySource::Env(var.into())
    );
    unsafe { std::env::remove_var(var) };
    assert_eq!(store.resolve_with("p", Some(var), Some(&command)).unwrap().1, KeySource::Helper);
    assert_eq!(store.resolve_with("p", None, None).unwrap().1, KeySource::Store);
}

#[test]
fn a_failing_helper_names_the_provider_and_status_but_never_its_output() {
    let (dir, store) = store();
    let helper = script(dir.path(), "bad.sh", "echo test-key-not-real-leaky-output\nexit 3");
    assert!(store.resolve_with("work", None, Some(&helper.display().to_string())).is_none());
    let err = store.helper_error("work").unwrap();
    assert!(err.contains("work"), "{err}");
    assert!(err.contains("exit status: 3"), "{err}");
    assert!(!err.contains("leaky-output"));
}

#[test]
fn a_failing_helper_falls_through_to_the_credential_store() {
    let (dir, store) = store();
    let helper = script(dir.path(), "bad.sh", "exit 1");
    store.save("p", "test-key-not-real-fallthrough").unwrap();
    let found = store.resolve_with("p", None, Some(&helper.display().to_string())).unwrap();
    assert_eq!(found.1, KeySource::Store);
}

#[test]
fn a_slow_helper_is_killed_at_the_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let store = zarn_config::auth::KeyStore::new(
        std::sync::Arc::new(zarn_config::auth::MemoryBackend::new()),
        dir.path().join("auth.json"),
    )
    .with_helper_timeout(Duration::from_millis(300));
    let helper = script(dir.path(), "slow.sh", "sleep 30\necho test-key-not-real-too-late");
    let started = std::time::Instant::now();
    assert!(store.resolve_with("p", None, Some(&helper.display().to_string())).is_none());
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(store.helper_error("p").unwrap().contains("did not finish"));
}

#[test]
fn the_helper_runs_without_a_shell() {
    let (dir, store) = store();
    let marker = dir.path().join("marker");
    let command = format!("echo test-key-not-real-literal; touch {}", marker.display());
    let found = store.resolve_with("p", None, Some(&command)).unwrap();
    assert_eq!(found.0, format!("test-key-not-real-literal; touch {}", marker.display()));
    assert!(!marker.exists());
}

#[test]
fn a_provider_with_a_helper_gets_its_key_in_the_plan_and_config_still_rejects_literal_keys() {
    let (dir, store) = store();
    let helper = script(dir.path(), "key.sh", "echo test-key-not-real-plan-helper");
    let cfg = config(&format!(
        "[providers.a]\nkind = \"anthropic\"\napi_key_helper = \"{}\"\n\n[profiles.default]\nprovider = \"a\"\nmodel = \"m\"\n",
        helper.display()
    ));
    let plan = zarn_config::runtime::plan(&cfg, &store, "default", None).unwrap();
    assert_eq!(plan.specs[0].api_key.as_deref(), Some("test-key-not-real-plan-helper"));
    let literal: Result<zarn_config::Config, _> =
        toml::from_str("[providers.a]\nkind = \"anthropic\"\napi_key = \"test-key-not-real\"\n");
    assert!(literal.unwrap().validate().is_err());
}

#[test]
fn a_broken_helper_on_the_primary_explains_itself_in_the_plan_error() {
    let (dir, store) = store();
    let helper = script(dir.path(), "bad.sh", "exit 7");
    let cfg = config(&format!(
        "[providers.a]\nkind = \"anthropic\"\napi_key_helper = \"{}\"\n\n[profiles.default]\nprovider = \"a\"\nmodel = \"m\"\n",
        helper.display()
    ));
    let err = zarn_config::runtime::plan(&cfg, &store, "default", None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("api_key_helper for a failed with exit status: 7"), "{err}");
}
