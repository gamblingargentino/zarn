use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

pub const SERVICE: &str = "zarn";
pub const REDACTED: &str = "[redacted]";
const MIN_SECRET_LEN: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    Unavailable(String),
    Failed(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Unavailable(why) => {
                write!(f, "credential store unavailable: {}", redact(why))
            }
            StoreError::Failed(why) => write!(f, "credential store error: {}", redact(why)),
        }
    }
}

impl std::error::Error for StoreError {}

pub trait CredentialBackend: Send + Sync {
    fn label(&self) -> String;
    fn get(&self, provider: &str) -> Result<Option<String>, StoreError>;
    fn set(&self, provider: &str, key: &str) -> Result<(), StoreError>;
    fn delete(&self, provider: &str) -> Result<bool, StoreError>;
}

pub const STORE_ENV: &str = "ZARN_CREDENTIAL_STORE";

pub struct FileOnly;

impl CredentialBackend for FileOnly {
    fn label(&self) -> String {
        "owner-only key file".into()
    }

    fn get(&self, _provider: &str) -> Result<Option<String>, StoreError> {
        Err(StoreError::Unavailable(format!("{STORE_ENV}=file")))
    }

    fn set(&self, _provider: &str, _key: &str) -> Result<(), StoreError> {
        Err(StoreError::Unavailable(format!("{STORE_ENV}=file")))
    }

    fn delete(&self, _provider: &str) -> Result<bool, StoreError> {
        Err(StoreError::Unavailable(format!("{STORE_ENV}=file")))
    }
}

pub struct OsKeyring;

impl OsKeyring {
    fn entry(provider: &str) -> Result<keyring::Entry, StoreError> {
        keyring::Entry::new(SERVICE, provider).map_err(map_keyring)
    }
}

fn map_keyring(err: keyring::Error) -> StoreError {
    match err {
        keyring::Error::NoStorageAccess(e) | keyring::Error::PlatformFailure(e) => {
            StoreError::Unavailable(e.to_string())
        }
        keyring::Error::NoDefaultStore => {
            StoreError::Unavailable("no OS credential store on this system".into())
        }
        other => StoreError::Failed(other.to_string()),
    }
}

impl CredentialBackend for OsKeyring {
    fn label(&self) -> String {
        if cfg!(windows) {
            "Windows Credential Manager".into()
        } else if cfg!(target_os = "macos") {
            "macOS Keychain".into()
        } else {
            "Secret Service keyring".into()
        }
    }

    fn get(&self, provider: &str) -> Result<Option<String>, StoreError> {
        match Self::entry(provider)?.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(map_keyring(err)),
        }
    }

    fn set(&self, provider: &str, key: &str) -> Result<(), StoreError> {
        Self::entry(provider)?
            .set_password(key)
            .map_err(map_keyring)
    }

    fn delete(&self, provider: &str) -> Result<bool, StoreError> {
        match Self::entry(provider)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(err) => Err(map_keyring(err)),
        }
    }
}

#[derive(Default)]
pub struct MemoryBackend {
    entries: Mutex<HashMap<String, String>>,
    unavailable: bool,
}

impl MemoryBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn unavailable() -> Self {
        MemoryBackend {
            entries: Mutex::new(HashMap::new()),
            unavailable: true,
        }
    }

    fn check(&self) -> Result<(), StoreError> {
        if self.unavailable {
            Err(StoreError::Unavailable("mock backend is offline".into()))
        } else {
            Ok(())
        }
    }

    pub fn len(&self) -> usize {
        self.entries.lock().map(|e| e.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl CredentialBackend for MemoryBackend {
    fn label(&self) -> String {
        "in-memory store".into()
    }

    fn get(&self, provider: &str) -> Result<Option<String>, StoreError> {
        self.check()?;
        Ok(self
            .entries
            .lock()
            .map_err(|_| StoreError::Failed("poisoned".into()))?
            .get(provider)
            .cloned())
    }

    fn set(&self, provider: &str, key: &str) -> Result<(), StoreError> {
        self.check()?;
        self.entries
            .lock()
            .map_err(|_| StoreError::Failed("poisoned".into()))?
            .insert(provider.to_string(), key.to_string());
        Ok(())
    }

    fn delete(&self, provider: &str) -> Result<bool, StoreError> {
        self.check()?;
        Ok(self
            .entries
            .lock()
            .map_err(|_| StoreError::Failed("poisoned".into()))?
            .remove(provider)
            .is_some())
    }
}

#[derive(Default, Serialize, Deserialize)]
struct AuthFile {
    #[serde(default)]
    keys: BTreeMap<String, String>,
}

pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        FileStore { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Result<AuthFile, StoreError> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| {
                StoreError::Failed(format!("{} is not valid: {e}", self.path.display()))
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(AuthFile::default()),
            Err(err) => Err(StoreError::Failed(format!(
                "reading {}: {err}",
                self.path.display()
            ))),
        }
    }

    fn write(&self, file: &AuthFile) -> Result<(), StoreError> {
        let fail = |what: &str, err: std::io::Error| {
            StoreError::Failed(format!("{what} {}: {err}", self.path.display()))
        };
        let dir = self
            .path
            .parent()
            .ok_or_else(|| StoreError::Failed("auth file has no parent directory".into()))?;
        std::fs::create_dir_all(dir).map_err(|e| fail("creating the directory for", e))?;

        if file.keys.is_empty() {
            return match std::fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(fail("removing", err)),
            };
        }

        let body = serde_json::to_string_pretty(file)
            .map_err(|e| StoreError::Failed(e.to_string()))?;
        let tmp = self.path.with_extension("json.tmp");
        let staged = (|| {
            let mut out = owner_only(&tmp).map_err(|e| fail("creating", e))?;
            restrict(&tmp).map_err(|e| fail("restricting", e))?;
            out.write_all(body.as_bytes())
                .and_then(|_| out.sync_all())
                .map_err(|e| fail("writing", e))?;
            drop(out);
            std::fs::rename(&tmp, &self.path).map_err(|e| fail("replacing", e))
        })();
        if staged.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        staged?;
        restrict(&self.path).map_err(|e| fail("restricting", e))
    }

    pub fn get(&self, provider: &str) -> Result<Option<String>, StoreError> {
        Ok(self.read()?.keys.get(provider).cloned())
    }

    pub fn set(&self, provider: &str, key: &str) -> Result<(), StoreError> {
        let mut file = self.read()?;
        file.keys.insert(provider.to_string(), key.to_string());
        self.write(&file)
    }

    pub fn delete(&self, provider: &str) -> Result<bool, StoreError> {
        let mut file = self.read()?;
        let removed = file.keys.remove(provider).is_some();
        if removed {
            self.write(&file)?;
        }
        Ok(removed)
    }

    pub fn names(&self) -> Vec<String> {
        self.read()
            .map(|f| f.keys.into_keys().collect())
            .unwrap_or_default()
    }
}

#[cfg(unix)]
fn owner_only(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn owner_only(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

#[cfg(unix)]
fn restrict(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(windows)]
fn restrict(path: &Path) -> std::io::Result<()> {
    let user = std::env::var("USERNAME").unwrap_or_default();
    if user.is_empty() {
        return Ok(());
    }
    let status = std::process::Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{user}:F"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("icacls could not restrict the file"))
    }
}

#[cfg(not(any(unix, windows)))]
fn restrict(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeySource {
    Env(String),
    Helper,
    Store,
    File,
}

pub const HELPER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const HELPER_TTL: std::time::Duration = std::time::Duration::from_secs(300);

pub fn split_command(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                    has = false;
                }
            }
            (None, c) => current.push(c),
        }
    }
    if has || !current.is_empty() {
        words.push(current);
    }
    words
}

fn spawn_helper(words: &[String]) -> std::io::Result<std::process::Child> {
    let piped = |command: &mut std::process::Command| -> std::io::Result<std::process::Child> {
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
    };
    match piped(std::process::Command::new(&words[0]).args(&words[1..])) {
        Err(err) if cfg!(windows) && err.kind() == std::io::ErrorKind::NotFound => {
            piped(std::process::Command::new("cmd").arg("/c").args(words))
        }
        other => other,
    }
}

pub fn run_helper(
    provider: &str,
    command: &str,
    timeout: std::time::Duration,
) -> Result<String, String> {
    use std::io::Read;
    let words = split_command(command);
    if words.is_empty() {
        return Err(format!("api_key_helper for {provider} is empty"));
    }
    let mut attempt = 0;
    let mut child = loop {
        match spawn_helper(&words) {
            Err(err) if err.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 10 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(err) => {
                return Err(format!(
                    "api_key_helper for {provider} could not start: {}",
                    err.kind()
                ));
            }
            Ok(child) => break child,
        }
    };
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(pipe) = stdout.as_mut() {
            let _ = pipe.read_to_end(&mut out);
        }
        out
    });
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "api_key_helper for {provider} did not finish within {} seconds",
                    timeout.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(err) => {
                return Err(format!("api_key_helper for {provider} failed: {}", err.kind()));
            }
        }
    };
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("api_key_helper for {provider} failed with {status}"));
    }
    let key = String::from_utf8_lossy(&out).trim().to_string();
    if key.is_empty() {
        return Err(format!("api_key_helper for {provider} printed nothing"));
    }
    register_secret(&key);
    Ok(key)
}

impl KeySource {
    pub fn describe(&self, store: &KeyStore) -> String {
        match self {
            KeySource::Env(var) => format!("environment variable {var}"),
            KeySource::Helper => "api_key_helper".to_string(),
            KeySource::Store => store.backend_label(),
            KeySource::File => format!("{} (fallback file)", store.file_path().display()),
        }
    }
}

pub struct KeyStore {
    backend: Arc<dyn CredentialBackend>,
    file: FileStore,
    fallback_used: AtomicBool,
    notice_shown: AtomicBool,
    helper_keys: Mutex<HashMap<String, (String, String, std::time::Instant)>>,
    helper_errors: Mutex<HashMap<String, String>>,
    helper_timeout: std::time::Duration,
}

impl KeyStore {
    pub fn new(backend: Arc<dyn CredentialBackend>, file: impl Into<PathBuf>) -> Self {
        KeyStore {
            backend,
            file: FileStore::new(file),
            fallback_used: AtomicBool::new(false),
            notice_shown: AtomicBool::new(false),
            helper_keys: Mutex::new(HashMap::new()),
            helper_errors: Mutex::new(HashMap::new()),
            helper_timeout: HELPER_TIMEOUT,
        }
    }

    pub fn with_helper_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.helper_timeout = timeout;
        self
    }

    pub fn helper_error(&self, provider: &str) -> Option<String> {
        self.helper_errors.lock().ok()?.get(provider).cloned()
    }

    fn helper_key(&self, provider: &str, command: &str) -> Option<String> {
        if let Ok(cache) = self.helper_keys.lock()
            && let Some((cmd, key, at)) = cache.get(provider)
            && cmd == command
            && at.elapsed() < HELPER_TTL
        {
            return Some(key.clone());
        }
        match run_helper(provider, command, self.helper_timeout) {
            Ok(key) => {
                if let Ok(mut errors) = self.helper_errors.lock() {
                    errors.remove(provider);
                }
                if let Ok(mut cache) = self.helper_keys.lock() {
                    cache.insert(
                        provider.to_string(),
                        (command.to_string(), key.clone(), std::time::Instant::now()),
                    );
                }
                Some(key)
            }
            Err(err) => {
                tracing::warn!(provider, error = %err, "api_key_helper failed");
                if let Ok(mut errors) = self.helper_errors.lock() {
                    errors.insert(provider.to_string(), err);
                }
                None
            }
        }
    }

    pub fn resolve_with(
        &self,
        provider: &str,
        api_key_env: Option<&str>,
        helper: Option<&str>,
    ) -> Option<(String, KeySource)> {
        if let Some(var) = api_key_env
            && let Ok(value) = std::env::var(var)
            && !value.trim().is_empty()
        {
            register_secret(&value);
            return Some((value, KeySource::Env(var.to_string())));
        }
        if let Some(command) = helper.filter(|c| !c.trim().is_empty())
            && let Some(key) = self.helper_key(provider, command)
        {
            return Some((key, KeySource::Helper));
        }
        self.resolve(provider, None)
    }

    pub fn has_with(
        &self,
        provider: &str,
        api_key_env: Option<&str>,
        helper: Option<&str>,
    ) -> Option<KeySource> {
        self.resolve_with(provider, api_key_env, helper)
            .map(|(_, source)| source)
    }

    pub fn system() -> Self {
        let file = crate::paths::auth_file().unwrap_or_else(|| PathBuf::from("auth.json"));
        let only_file = std::env::var(STORE_ENV).is_ok_and(|v| v.eq_ignore_ascii_case("file"));
        if only_file {
            return Self::new(Arc::new(FileOnly), file);
        }
        Self::new(Arc::new(OsKeyring), file)
    }

    pub fn shared() -> Arc<KeyStore> {
        static SHARED: OnceLock<Arc<KeyStore>> = OnceLock::new();
        SHARED.get_or_init(|| Arc::new(KeyStore::system())).clone()
    }

    pub fn backend_label(&self) -> String {
        self.backend.label()
    }

    pub fn file_path(&self) -> &Path {
        self.file.path()
    }

    pub fn store_available(&self) -> bool {
        !matches!(self.backend.get("__zarn_probe__"), Err(StoreError::Unavailable(_)))
    }

    fn note_fallback(&self) {
        self.fallback_used.store(true, Ordering::SeqCst);
    }

    fn notice_marker(&self) -> PathBuf {
        self.file.path().with_extension("json.notified")
    }

    pub fn fallback_notice(&self) -> Option<String> {
        if !self.fallback_used.load(Ordering::SeqCst) {
            return None;
        }
        if self.notice_shown.swap(true, Ordering::SeqCst) {
            return None;
        }
        let marker = self.notice_marker();
        if marker.exists() {
            return None;
        }
        if let Some(dir) = marker.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&marker, "");
        Some(format!(
            "The OS credential store is unavailable, so API keys are kept in {} with owner-only permissions. Remove a key with: zarn logout <provider>",
            self.file.path().display()
        ))
    }

    pub fn resolve(&self, provider: &str, api_key_env: Option<&str>) -> Option<(String, KeySource)> {
        if let Some(var) = api_key_env
            && let Ok(value) = std::env::var(var)
            && !value.trim().is_empty()
        {
            register_secret(&value);
            return Some((value, KeySource::Env(var.to_string())));
        }

        match self.backend.get(provider) {
            Ok(Some(key)) => {
                register_secret(&key);
                return Some((key, KeySource::Store));
            }
            Ok(None) => {}
            Err(StoreError::Unavailable(_)) => self.note_fallback(),
            Err(err) => tracing::warn!(provider, error = %err, "credential store read failed"),
        }

        match self.file.get(provider) {
            Ok(Some(key)) => {
                register_secret(&key);
                self.note_fallback();
                Some((key, KeySource::File))
            }
            Ok(None) => None,
            Err(err) => {
                tracing::warn!(provider, error = %err, "fallback key file unreadable");
                None
            }
        }
    }

    pub fn save(&self, provider: &str, key: &str) -> Result<KeySource, StoreError> {
        let key = key.trim();
        if key.is_empty() {
            return Err(StoreError::Failed("the key is empty".into()));
        }
        register_secret(key);
        match self.backend.set(provider, key) {
            Ok(()) => {
                let _ = self.file.delete(provider);
                Ok(KeySource::Store)
            }
            Err(StoreError::Unavailable(_)) => {
                self.note_fallback();
                self.file.set(provider, key)?;
                Ok(KeySource::File)
            }
            Err(err) => Err(err),
        }
    }

    pub fn remove(&self, provider: &str) -> Result<bool, StoreError> {
        let from_store = match self.backend.delete(provider) {
            Ok(removed) => removed,
            Err(StoreError::Unavailable(_)) => false,
            Err(err) => return Err(err),
        };
        let from_file = self.file.delete(provider)?;
        Ok(from_store || from_file)
    }

    pub fn has(&self, provider: &str, api_key_env: Option<&str>) -> Option<KeySource> {
        self.resolve(provider, api_key_env).map(|(_, source)| source)
    }
}

fn secrets() -> &'static RwLock<Vec<String>> {
    static SECRETS: OnceLock<RwLock<Vec<String>>> = OnceLock::new();
    SECRETS.get_or_init(|| RwLock::new(Vec::new()))
}

pub fn register_secret(secret: &str) {
    let secret = secret.trim();
    if secret.len() < MIN_SECRET_LEN {
        return;
    }
    if let Ok(mut list) = secrets().write()
        && !list.iter().any(|s| s == secret)
    {
        list.push(secret.to_string());
        list.sort_by_key(|s| std::cmp::Reverse(s.len()));
    }
}

fn key_shapes() -> &'static Regex {
    static SHAPES: OnceLock<Regex> = OnceLock::new();
    SHAPES.get_or_init(|| {
        Regex::new(concat!(
            r"\bsk-ant-[A-Za-z0-9_\-]{16,}",
            r"|\bsk-[A-Za-z0-9_\-]{20,}",
            r"|\bAIza[0-9A-Za-z_\-]{30,}",
            r"|\bgsk_[A-Za-z0-9]{20,}",
            r"|(?i:bearer)\s+[A-Za-z0-9._\-]{20,}",
        ))
        .expect("static redaction pattern")
    })
}

pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    if let Ok(list) = secrets().read() {
        for secret in list.iter() {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), REDACTED);
            }
        }
    }
    key_shapes().replace_all(&out, REDACTED).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zarn").join("auth.json");
        (dir, path)
    }

    #[test]
    fn redaction_replaces_registered_secrets_and_key_shapes() {
        register_secret("test-key-not-real-redact-me");
        let text = "failed with test-key-not-real-redact-me and sk-test-key-not-real-abcdefgh";
        let out = redact(text);
        assert!(!out.contains("test-key-not-real-redact-me"));
        assert!(!out.contains("sk-test-key-not-real-abcdefgh"));
        assert_eq!(out.matches(REDACTED).count(), 2);
    }

    #[test]
    fn ordinary_words_ending_in_sk_are_left_alone() {
        let text = "a risk-based-permission-defaults-policy and a disk-cache-eviction-strategy";
        assert_eq!(redact(text), text);
    }

    #[test]
    fn short_strings_are_never_registered_as_secrets() {
        register_secret("abc");
        assert_eq!(redact("abc abc"), "abc abc");
    }

    #[test]
    fn store_errors_redact_their_message() {
        register_secret("test-key-not-real-in-error");
        let err = StoreError::Failed("boom test-key-not-real-in-error".into());
        assert!(!err.to_string().contains("test-key-not-real-in-error"));
    }

    #[cfg(unix)]
    #[test]
    fn the_fallback_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, path) = temp_file();
        let store = FileStore::new(&path);
        store.set("openai", "test-key-not-real").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn deleting_the_last_key_removes_the_fallback_file() {
        let (_dir, path) = temp_file();
        let store = FileStore::new(&path);
        store.set("openai", "test-key-not-real").unwrap();
        assert!(store.delete("openai").unwrap());
        assert!(!path.exists());
    }
}
