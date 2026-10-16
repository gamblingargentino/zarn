use std::sync::Arc;
use zarn_config::auth::{CredentialBackend, KeySource, KeyStore, MemoryBackend, StoreError};

const KEY: &str = "test-key-not-real-0001";
const OTHER: &str = "test-key-not-real-0002";
const THIRD: &str = "test-key-not-real-0003";

fn store_with(backend: Arc<MemoryBackend>) -> (tempfile::TempDir, KeyStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = KeyStore::new(backend, dir.path().join("auth.json"));
    (dir, store)
}

fn set_env(var: &str, value: &str) {
    unsafe { std::env::set_var(var, value) };
}

fn unset_env(var: &str) {
    unsafe { std::env::remove_var(var) };
}

#[test]
fn a_key_round_trips_through_the_mock_credential_store() {
    let backend = Arc::new(MemoryBackend::new());
    let (_dir, store) = store_with(backend.clone());

    assert_eq!(store.save("openai", KEY).unwrap(), KeySource::Store);
    assert_eq!(backend.get("openai").unwrap().as_deref(), Some(KEY));
    assert_eq!(
        store.resolve("openai", None),
        Some((KEY.to_string(), KeySource::Store))
    );

    assert_eq!(store.save("openai", OTHER).unwrap(), KeySource::Store);
    assert_eq!(store.resolve("openai", None).unwrap().0, OTHER);

    assert!(store.remove("openai").unwrap());
    assert_eq!(store.resolve("openai", None), None);
    assert!(!store.remove("openai").unwrap());
    assert!(backend.is_empty());
}

#[test]
fn saving_to_a_working_store_never_touches_the_fallback_file() {
    let (dir, store) = store_with(Arc::new(MemoryBackend::new()));
    store.save("anthropic", KEY).unwrap();
    assert!(!dir.path().join("auth.json").exists());
    assert!(store.fallback_notice().is_none());
}

#[test]
fn an_unavailable_store_falls_back_to_the_file_and_says_so_once() {
    let (dir, store) = store_with(Arc::new(MemoryBackend::unavailable()));

    assert_eq!(store.save("google", KEY).unwrap(), KeySource::File);
    let path = dir.path().join("auth.json");
    assert!(path.exists());

    let notice = store.fallback_notice().expect("a notice the first time");
    assert!(notice.contains("auth.json"));
    assert!(notice.contains("zarn logout"));
    assert!(!notice.contains(KEY));
    assert!(store.fallback_notice().is_none());
    let again = KeyStore::new(Arc::new(MemoryBackend::unavailable()), &path);
    again.save("other", OTHER).unwrap();
    assert!(again.fallback_notice().is_none(), "the notice is shown once, not once per run");

    assert_eq!(
        store.resolve("google", None),
        Some((KEY.to_string(), KeySource::File))
    );
    assert!(store.remove("google").unwrap());
    assert_eq!(store.resolve("google", None), None);
}

#[test]
fn an_empty_key_is_refused() {
    let (_dir, store) = store_with(Arc::new(MemoryBackend::new()));
    assert!(matches!(store.save("openai", "   "), Err(StoreError::Failed(_))));
}

#[test]
fn resolution_prefers_env_then_store_then_file() {
    let var = "ZARN_TEST_RESOLUTION_ORDER_KEY";
    unset_env(var);
    let backend = Arc::new(MemoryBackend::new());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("auth.json");

    let offline = KeyStore::new(Arc::new(MemoryBackend::unavailable()), &file);
    offline.save("p", THIRD).unwrap();

    let store = KeyStore::new(backend.clone(), &file);
    assert_eq!(
        store.resolve("p", Some(var)),
        Some((THIRD.to_string(), KeySource::File))
    );

    backend.set("p", OTHER).unwrap();
    assert_eq!(
        store.resolve("p", Some(var)),
        Some((OTHER.to_string(), KeySource::Store))
    );

    set_env(var, KEY);
    assert_eq!(
        store.resolve("p", Some(var)),
        Some((KEY.to_string(), KeySource::Env(var.to_string())))
    );

    assert_eq!(
        store.resolve("p", None),
        Some((OTHER.to_string(), KeySource::Store))
    );

    unset_env(var);
    backend.delete("p").unwrap();
    assert_eq!(
        store.resolve("p", Some(var)),
        Some((THIRD.to_string(), KeySource::File))
    );
}

#[test]
fn an_env_var_that_is_named_but_empty_falls_through() {
    let var = "ZARN_TEST_EMPTY_ENV_KEY";
    set_env(var, "");
    let backend = Arc::new(MemoryBackend::new());
    backend.set("p", OTHER).unwrap();
    let (_dir, store) = store_with(backend);
    assert_eq!(store.resolve("p", Some(var)).unwrap().1, KeySource::Store);
    unset_env(var);
}

#[test]
fn a_config_chain_carries_the_resolved_key_without_writing_it_anywhere() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[providers.a]\nkind = \"openai\"\n\n[profiles.default]\nprovider = \"a\"\n",
    )
    .unwrap();
    let before = std::fs::read_to_string(&path).unwrap();

    let backend = Arc::new(MemoryBackend::new());
    let (_keys, store) = store_with(backend);
    store.save("a", KEY).unwrap();

    let config = zarn_config::Config::load_from(std::slice::from_ref(&path)).unwrap();
    let chain = config.chain_for(&store, "default").unwrap();
    assert_eq!(chain[0].api_key.as_deref(), Some(KEY));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}
