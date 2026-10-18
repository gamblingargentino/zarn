use crate::auth::KeyStore;

pub fn looks_like_secret(value: &str) -> bool {
    let v = value.trim();
    if crate::secrets::is_reference(v) {
        return false;
    }
    crate::secrets::matches(v).is_some() || v.split_whitespace().any(|w| crate::secrets::matches(w).is_some())
}

pub fn resolve_inline(store: &KeyStore, value: &str) -> Result<String, String> {
    if crate::secrets::is_reference(value) {
        return resolve(store, value.trim());
    }
    if !value.split_whitespace().any(crate::secrets::is_reference) {
        return Ok(value.to_string());
    }
    let parts: Result<Vec<String>, String> = value
        .split(' ')
        .map(|word| if crate::secrets::is_reference(word) { resolve(store, word) } else { Ok(word.to_string()) })
        .collect();
    parts.map(|p| p.join(" "))
}

pub fn resolve(store: &KeyStore, value: &str) -> Result<String, String> {
    if let Some(name) = value.strip_prefix("env:") {
        let found = std::env::var(name).map_err(|_| format!("environment variable {name} is not set"))?;
        crate::auth::register_secret(&found);
        return Ok(found);
    }
    if let Some(name) = value.strip_prefix("key:") {
        let (found, _) = store
            .resolve(name, None)
            .ok_or_else(|| format!("no key named {name} in the credential store; run zarn login {name}"))?;
        crate::auth::register_secret(&found);
        return Ok(found);
    }
    if looks_like_secret(value) {
        return Err("this looks like a literal secret; store it with zarn login <name> and write key:<name>, or use env:<VARIABLE>".into());
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::MemoryBackend;
    use std::sync::Arc;

    #[test]
    fn references_resolve_and_literals_that_look_secret_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::new(Arc::new(MemoryBackend::new()), dir.path().join("auth.json"));
        store.save("search", "test-key-not-real-refs-0001").unwrap();
        assert_eq!(resolve(&store, "key:search").unwrap(), "test-key-not-real-refs-0001");
        unsafe {
            std::env::set_var("ZARN_REFS_TEST_VAR", "test-key-not-real-env-0002");
        }
        assert_eq!(resolve(&store, "env:ZARN_REFS_TEST_VAR").unwrap(), "test-key-not-real-env-0002");
        assert!(resolve(&store, "env:ZARN_REFS_UNSET_VAR").unwrap_err().contains("not set"));
        assert!(resolve(&store, "key:missing").unwrap_err().contains("zarn login missing"));
        assert!(resolve(&store, &format!("sk-{}", "q7".repeat(12))).unwrap_err().contains("literal secret"));
        assert_eq!(resolve_inline(&store, "Bearer key:search").unwrap(), "Bearer test-key-not-real-refs-0001");
        assert_eq!(resolve_inline(&store, "plain value").unwrap(), "plain value");
        assert_eq!(resolve(&store, "application/json").unwrap(), "application/json");
        assert_eq!(resolve(&store, "--verbose").unwrap(), "--verbose");
    }
}
