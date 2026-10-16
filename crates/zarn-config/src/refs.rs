use crate::auth::KeyStore;

pub fn looks_like_secret(value: &str) -> bool {
    let v = value.trim();
    if v.starts_with("env:") || v.starts_with("key:") {
        return false;
    }
    let shaped = ["sk-", "sk_", "gsk_", "AIza", "ghp_", "gho_", "github_pat_", "xox", "glpat-", "Bearer ", "bearer "]
        .iter()
        .any(|p| v.starts_with(p) && v.len() >= 20);
    let opaque = v.len() >= 32
        && !v.contains(' ')
        && !v.contains('/')
        && v.chars().any(|c| c.is_ascii_digit())
        && v.chars().any(|c| c.is_ascii_alphabetic());
    shaped || opaque
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
        assert!(resolve(&store, "sk-test-key-not-real-abcdefghijkl").unwrap_err().contains("literal secret"));
        assert!(resolve(&store, "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8").is_err());
        assert_eq!(resolve(&store, "application/json").unwrap(), "application/json");
        assert_eq!(resolve(&store, "--verbose").unwrap(), "--verbose");
    }
}
