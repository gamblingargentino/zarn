use crate::auth::{KeyStore, redact, register_secret};
use crate::catalog;
use crate::layers::Layers;
use crate::ops::{self, ProviderDraft};
use crate::paths;
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

pub const DEFAULT_PORT: u16 = 9276;
pub const FALLBACK_PORTS: std::ops::RangeInclusive<u16> = 9277..=9299;
pub const IDLE: Duration = Duration::from_secs(30 * 60);
pub const NO_BROWSER_ENV: &str = "ZARN_NO_BROWSER";
const COOKIE: &str = "zarn_setup";
const MAX_HEADER: usize = 16 * 1024;
const MAX_BODY: usize = 64 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Saved { provider: String, place: String },
    Removed(String),
    Added(String),
    Updated(String),
    Closed,
}

pub type OnChange = Arc<dyn Fn(Change) + Send + Sync>;

pub struct Options {
    pub port: Option<u16>,
    pub store: Arc<KeyStore>,
    pub project: PathBuf,
    pub user: Option<PathBuf>,
    pub on_change: Option<OnChange>,
    pub idle: Duration,
    pub cache: PathBuf,
}

impl Options {
    pub fn new(store: Arc<KeyStore>) -> Self {
        Options {
            port: None,
            store,
            project: paths::project_config(),
            user: paths::user_config(),
            on_change: None,
            idle: IDLE,
            cache: crate::quota::cache_dir(),
        }
    }
}

pub struct Running {
    pub url: String,
    pub port: u16,
    stop: watch::Sender<bool>,
    stopped: watch::Receiver<bool>,
}

impl Running {
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }

    pub fn is_stopped(&self) -> bool {
        *self.stopped.borrow()
    }

    pub async fn wait(&mut self) {
        while !*self.stopped.borrow_and_update() {
            if self.stopped.changed().await.is_err() {
                return;
            }
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

struct State {
    token: String,
    port: u16,
    store: Arc<KeyStore>,
    project: PathBuf,
    user: Option<PathBuf>,
    flash: Mutex<Option<(String, bool)>>,
    cache: PathBuf,
    on_change: Option<OnChange>,
    stop: watch::Sender<bool>,
    last: Mutex<Instant>,
}

impl State {
    fn layers(&self) -> Result<Layers> {
        Layers::load_from(self.project.clone(), self.user.clone())
    }

    fn flash(&self, text: impl Into<String>, error: bool) {
        *self.flash.lock().unwrap_or_else(|e| e.into_inner()) = Some((text.into(), error));
    }

    fn take_flash(&self) -> Option<(String, bool)> {
        self.flash.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    fn changed(&self, change: Change) {
        if let Some(callback) = &self.on_change {
            callback(change);
        }
    }

    fn touch(&self) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    fn idle_for(&self) -> Duration {
        self.last.lock().unwrap_or_else(|e| e.into_inner()).elapsed()
    }

    fn hosts(&self) -> [String; 3] {
        [
            format!("localhost:{}", self.port),
            format!("127.0.0.1:{}", self.port),
            format!("[::1]:{}", self.port),
        ]
    }
}

pub fn random_token() -> String {
    let mut bytes = [0u8; 16];
    let filled = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .is_ok();
    if !filled {
        use std::hash::{BuildHasher, Hasher};
        for (i, chunk) in bytes.chunks_mut(8).enumerate() {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_usize(i);
            hasher.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
            );
            hasher.write_u32(std::process::id());
            chunk.copy_from_slice(&hasher.finish().to_le_bytes()[..chunk.len()]);
        }
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn bind(port: Option<u16>) -> Result<TcpListener> {
    let candidates: Vec<u16> = match port {
        Some(port) => vec![port],
        None => std::iter::once(DEFAULT_PORT).chain(FALLBACK_PORTS).collect(),
    };
    let mut last = None;
    for candidate in candidates {
        match std::net::TcpListener::bind(("127.0.0.1", candidate)) {
            Ok(listener) => {
                listener.set_nonblocking(true)?;
                return Ok(TcpListener::from_std(listener)?);
            }
            Err(err) => last = Some(err),
        }
    }
    match last {
        Some(err) => Err(err).with_context(|| match port {
            Some(port) => format!("port {port} is busy"),
            None => format!("ports {DEFAULT_PORT} to {} are all busy; pass --port", FALLBACK_PORTS.end()),
        }),
        None => bail!("no port to listen on"),
    }
}

pub fn start(options: Options) -> Result<Running> {
    let listener = bind(options.port)?;
    let port = listener.local_addr()?.port();
    let token = random_token();
    let (stop, stopped) = watch::channel(false);
    let state = Arc::new(State {
        token: token.clone(),
        port,
        store: options.store,
        project: options.project,
        user: options.user,
        flash: Mutex::new(None),
        cache: options.cache,
        on_change: options.on_change,
        stop: stop.clone(),
        last: Mutex::new(Instant::now()),
    });
    let idle = options.idle;
    let mut quit = stopped.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    if let Ok((stream, peer)) = accepted
                        && peer.ip().is_loopback()
                    {
                        let state = state.clone();
                        tokio::spawn(async move {
                            let _ = tokio::time::timeout(READ_TIMEOUT * 2, serve(stream, state)).await;
                        });
                    }
                }
                _ = quit.changed() => {
                    if *quit.borrow() {
                        break;
                    }
                }
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    if state.idle_for() >= idle {
                        let _ = state.stop.send(true);
                        break;
                    }
                }
            }
        }
        state.changed(Change::Closed);
    });
    Ok(Running {
        url: format!("http://localhost:{port}/?token={token}"),
        port,
        stop,
        stopped,
    })
}

pub fn open_browser(url: &str) -> bool {
    if std::env::var_os(NO_BROWSER_ENV).is_some() {
        return false;
    }
    let mut command = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

struct Request {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn cookie(&self) -> Option<String> {
        self.header("cookie")?
            .split(';')
            .filter_map(|part| part.trim().split_once('='))
            .find(|(name, _)| *name == COOKIE)
            .map(|(_, value)| value.to_string())
    }
}

async fn read_request(stream: &mut TcpStream) -> Result<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        if buf.len() > MAX_HEADER {
            bail!("headers too large");
        }
        let n = tokio::time::timeout(READ_TIMEOUT, stream.read(&mut chunk)).await??;
        if n == 0 {
            bail!("connection closed");
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let start = lines.next().unwrap_or_default();
    let mut parts = start.split(' ');
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    if length > MAX_BODY {
        bail!("body too large");
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < length {
        let n = tokio::time::timeout(READ_TIMEOUT, stream.read(&mut chunk)).await??;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(length);
    Ok(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

pub fn form_decode(text: &str) -> Vec<(String, String)> {
    text.split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len()
                && bytes[i + 1].is_ascii_hexdigit()
                && bytes[i + 2].is_ascii_hexdigit() =>
            {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("0");
                out.push(u8::from_str_radix(hex, 16).unwrap_or(b'?'));
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn response(status: &str, extra: &[(&str, String)], body: &str) -> String {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nX-Frame-Options: DENY\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: same-origin\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'\r\n",
        body.len()
    );
    for (name, value) in extra {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(body);
    out
}

fn redirect(extra: &[(&str, String)]) -> String {
    let mut headers = vec![("Location", "/".to_string())];
    headers.extend(extra.iter().cloned());
    response("303 See Other", &headers, "")
}

fn same_site(request: &Request, state: &State) -> bool {
    match request.header("sec-fetch-site") {
        Some("same-origin") => return true,
        Some("none") | None => {}
        Some(_) => return false,
    }
    match request.header("origin") {
        None | Some("null") => true,
        Some(origin) => state.hosts().iter().any(|h| origin == format!("http://{h}")),
    }
}

fn refused_with_link(reason: &str) -> String {
    response(
        "403 Forbidden",
        &[],
        &page(
            "zarn · not allowed",
            &format!(
                "<section><h2>That request was refused</h2><p>{}</p><p><a href=\"/\">Back to the setup page</a></p></section>",
                escape(reason)
            ),
        ),
    )
}

fn refused(reason: &str) -> String {
    response(
        "403 Forbidden",
        &[],
        &page(
            "zarn · not allowed",
            &format!(
                "<section><h2>This page cannot be used from here</h2><p>{}</p><p>Open the link that zarn printed in your terminal, on the same machine.</p></section>",
                escape(reason)
            ),
        ),
    )
}

async fn serve(mut stream: TcpStream, state: Arc<State>) {
    let Ok(request) = read_request(&mut stream).await else {
        return;
    };
    state.touch();
    let reply = route(&request, &state).await;
    let _ = stream.write_all(reply.as_bytes()).await;
    let _ = stream.shutdown().await;
}

async fn route(request: &Request, state: &State) -> String {
    let host = request.header("host").unwrap_or_default();
    if !state.hosts().iter().any(|h| h == host) {
        return refused("the address in the browser is not localhost");
    }
    if request.method == "GET" && request.path == "/" {
        let given = form_decode(&request.query)
            .into_iter()
            .find(|(k, _)| k == "token")
            .map(|(_, v)| v);
        if let Some(given) = given {
            if !same(&given, &state.token) {
                return refused("the link has an old or wrong token; zarn prints a new one each time it opens this page");
            }
            return redirect(&[(
                "Set-Cookie",
                format!("{COOKIE}={}; HttpOnly; SameSite=Strict; Path=/", state.token),
            )]);
        }
    }
    let authorized = request.cookie().is_some_and(|c| same(&c, &state.token));
    if !authorized {
        return refused("this browser has not opened the setup link yet");
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => response("200 OK", &[], &home(state)),
        ("GET", "/favicon.ico") => response("404 Not Found", &[], ""),
        ("POST", path) => {
            if !same_site(request, state) {
                return refused_with_link("the form was sent from another site");
            }
            let form = form_decode(&String::from_utf8_lossy(&request.body));
            let field = |name: &str| {
                form.iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.trim().to_string())
                    .unwrap_or_default()
            };
            if !same(&field("csrf"), &state.token) {
                return refused_with_link("the form is missing its token; reload the page and try again");
            }
            match path {
                "/save" => save(state, &field("provider"), &field("key"), !field("test").is_empty()).await,
                "/custom" => custom(state, &form_value(&form, "name"), &form_value(&form, "base_url"), &form_value(&form, "model"), &field("key"), !field("test").is_empty()).await,
                "/remove" => remove(state, &field("provider")),
                "/use" => use_model(state, &field("provider"), &field("model")),
                "/models" => refresh_models(state, &field("provider")).await,
                "/test" => test(state, &field("provider")).await,
                "/done" => {
                    let _ = state.stop.send(true);
                    return response(
                        "200 OK",
                        &[],
                        &page("zarn · done", "<section><h2>All set</h2><p>The setup page is closed. You can close this tab and go back to the terminal.</p></section>"),
                    );
                }
                _ => return response("404 Not Found", &[], &page("zarn", "<p>Not found.</p>")),
            }
            redirect(&[])
        }
        _ => response("404 Not Found", &[], &page("zarn", "<p>Not found.</p>")),
    }
}

fn form_value(form: &[(String, String)], name: &str) -> String {
    form.iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default()
}

async fn save(state: &State, provider: &str, key: &str, check: bool) {
    if key.is_empty() {
        return state.flash("paste a key first", true);
    }
    register_secret(key);
    let mut layers = match state.layers() {
        Ok(layers) => layers,
        Err(err) => return state.flash(format!("config error: {err:#}"), true),
    };
    let provider = match ops::resolve_provider(&layers.merged(), provider, key) {
        Ok(provider) => provider,
        Err(err) => return state.flash(err, true),
    };
    match ops::connect(&mut layers, &state.store, &state.cache, &provider, key, check).await {
        Ok(done) => {
            state.flash(done.summary(), false);
            state.changed(Change::Saved {
                provider: done.provider.clone(),
                place: done.place,
            });
        }
        Err(err) => state.flash(redact(&format!("{provider}: {err:#}")), true),
    }
}

async fn custom(state: &State, name: &str, base_url: &str, model: &str, key: &str, check: bool) {
    if name.is_empty() || base_url.is_empty() {
        return state.flash("a custom endpoint needs a name and a base URL", true);
    }
    if !catalog::valid_name(name) {
        return state.flash(format!("'{name}' is not a valid name: use letters, digits, '-' and '_'"), true);
    }
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return state.flash("the base URL must start with http:// or https://", true);
    }
    if !key.is_empty() {
        register_secret(key);
    }
    let mut draft = ProviderDraft {
        name: name.to_string(),
        kind: "openai-compat".into(),
        base_url: Some(base_url.trim_end_matches('/').to_string()),
        api_key_env: None,
        model: Some(model.to_string()).filter(|m| !m.is_empty()),
    };
    let mut models = Vec::new();
    if check {
        let mut probe = draft.spec();
        probe.api_key = Some(key.to_string()).filter(|k| !k.is_empty());
        if let Ok(Ok(found)) =
            tokio::time::timeout(ops::TEST_TIMEOUT, zarn_providers::discovery::models(name, &probe)).await
        {
            models = found;
        }
        if draft.model.is_none() {
            draft.model = ops::pick_model(&probe, &models);
        }
        let Some(test) = draft.model.clone() else {
            return state.flash(
                format!("nothing was saved: {base_url} did not list any models; enter a model name"),
                true,
            );
        };
        if let Err(err) = ops::test_provider(&probe, &test).await {
            return state.flash(
                format!("nothing was saved: a test request to {base_url} failed: {}", redact(&err)),
                true,
            );
        }
    }
    let mut layers = match state.layers() {
        Ok(layers) => layers,
        Err(err) => return state.flash(format!("config error: {err:#}"), true),
    };
    if let Err(err) = ops::add_provider(&mut layers, &draft) {
        return state.flash(format!("{err:#}"), true);
    }
    let mut message = format!("{name} added");
    if !models.is_empty() {
        ops::cache_models(&state.cache, name, &draft.spec(), &models);
        message.push_str(&format!(" · {} models found", models.len()));
    }
    if let Some(model) = &draft.model {
        message.push_str(&format!(" · using {model}"));
    }
    if !key.is_empty() {
        match state.store.save(name, key) {
            Ok(source) => message.push_str(&format!(" · key saved to the {}", source.describe(&state.store))),
            Err(err) => return state.flash(format!("{name} was added but the key was not saved: {err}"), true),
        }
    }
    state.flash(message, false);
    state.changed(Change::Added(name.to_string()));
}

fn use_model(state: &State, provider: &str, model: &str) {
    if model.is_empty() {
        return state.flash("pick a model", true);
    }
    let mut layers = match state.layers() {
        Ok(layers) => layers,
        Err(err) => return state.flash(format!("config error: {err:#}"), true),
    };
    match ops::use_model(&mut layers, provider, model) {
        Ok(profile) => {
            state.flash(format!("profile {profile} now uses {provider} · {model}"), false);
            state.changed(Change::Updated(provider.to_string()));
        }
        Err(err) => state.flash(format!("{err:#}"), true),
    }
}

async fn refresh_models(state: &State, provider: &str) {
    let layers = match state.layers() {
        Ok(layers) => layers,
        Err(err) => return state.flash(format!("config error: {err:#}"), true),
    };
    let cfg = layers.merged();
    let spec = match ops::spec_with_key(&cfg, &state.store, provider) {
        Ok(spec) => spec,
        Err(err) => return state.flash(format!("{err:#}"), true),
    };
    match tokio::time::timeout(ops::TEST_TIMEOUT, zarn_providers::discovery::models(provider, &spec)).await {
        Ok(Ok(found)) => {
            ops::cache_models(&state.cache, provider, &spec, &found);
            state.flash(format!("{provider}: {} models found", found.len()), false);
            state.changed(Change::Updated(provider.to_string()));
        }
        Ok(Err(err)) => state.flash(format!("{provider}: could not list models: {}", redact(&err)), true),
        Err(_) => state.flash(format!("{provider}: no answer while listing models"), true),
    }
}

fn remove(state: &State, provider: &str) {
    match ops::logout(&state.store, provider) {
        Ok(true) => {
            state.flash(format!("{provider}: key removed"), false);
            state.changed(Change::Removed(provider.to_string()));
        }
        Ok(false) => state.flash(format!("{provider} had no stored key"), true),
        Err(err) => state.flash(format!("{err:#}"), true),
    }
}

async fn test(state: &State, provider: &str) {
    let layers = match state.layers() {
        Ok(layers) => layers,
        Err(err) => return state.flash(format!("config error: {err:#}"), true),
    };
    match ops::test_named(&layers, &state.store, provider).await {
        Ok(ok) => state.flash(format!("{provider}: {ok}"), false),
        Err(err) => state.flash(format!("{provider}: test failed: {}", redact(&err)), true),
    }
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{}</title><style>{STYLE}</style></head><body><main><header><div class=\"mark\">zarn</div><div class=\"where\">local setup page</div></header>{body}</main></body></html>",
        escape(title)
    )
}

const STYLE: &str = r#"
:root{--bg:#f6f5f2;--card:#fff;--ink:#1d1c1a;--dim:#6b6862;--line:#e3e0da;--accent:#7a5af8;--ok:#1f8a4c;--bad:#c2372c;--field:#fbfaf8}
@media (prefers-color-scheme:dark){:root{--bg:#141413;--card:#1d1d1b;--ink:#ecebe7;--dim:#9b988f;--line:#2f2e2b;--accent:#a28bfa;--ok:#4cc27e;--bad:#ef6a5f;--field:#232321}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:15px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif}
main{max-width:760px;margin:0 auto;padding:32px 16px 64px}
header{display:flex;align-items:baseline;gap:12px;margin-bottom:8px}.mark{font-weight:800;font-size:28px;letter-spacing:-.02em;color:var(--accent)}.where{color:var(--dim)}
p.lead{color:var(--dim);margin:0 0 24px}
section{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:20px;margin:16px 0}
h2{font-size:17px;margin:0 0 12px}
table{width:100%;border-collapse:collapse}td,th{text-align:left;padding:8px 6px;border-bottom:1px solid var(--line);vertical-align:middle}th{color:var(--dim);font-weight:500;font-size:13px}
tr:last-child td{border-bottom:none}.status-ok{color:var(--ok)}.status-bad{color:var(--bad)}.dim{color:var(--dim)}
.actions{display:flex;gap:6px;justify-content:flex-end}.actions form{margin:0}
label{display:block;font-size:13px;color:var(--dim);margin:12px 0 4px}
input[type=text],input[type=password],input[type=url],select{width:100%;padding:10px 12px;border:1px solid var(--line);border-radius:8px;background:var(--field);color:var(--ink);font:inherit}
input:focus,select:focus{outline:2px solid var(--accent);outline-offset:1px}
.check{display:flex;align-items:center;gap:8px;margin:14px 0 4px;color:var(--ink);font-size:14px}.check input{width:auto}
button{font:inherit;border-radius:8px;padding:9px 16px;border:1px solid var(--accent);background:var(--accent);color:#fff;cursor:pointer;font-weight:600}
button.quiet{background:transparent;color:var(--ink);border-color:var(--line);font-weight:500;padding:6px 12px}
button.danger{background:transparent;color:var(--bad);border-color:var(--line);font-weight:500;padding:6px 12px}
.row{margin-top:16px;display:flex;gap:10px;align-items:center}
.flash{padding:12px 16px;border-radius:10px;margin:16px 0;border:1px solid}.flash.ok{border-color:var(--ok);color:var(--ok)}.flash.bad{border-color:var(--bad);color:var(--bad)}
.hint{font-size:13px;color:var(--dim);margin-top:6px}a{color:var(--accent)}
.done{text-align:center;background:transparent;border:none}
form.pick{display:flex;gap:6px;margin:0}form.pick select{padding:6px 8px;min-width:0;max-width:240px}
details{margin-top:14px}summary{cursor:pointer}
"#;

fn home(state: &State) -> String {
    let csrf = format!("<input type=\"hidden\" name=\"csrf\" value=\"{}\">", escape(&state.token));
    let mut body = String::from(
        "<p class=\"lead\">Add, test or remove the API keys zarn uses. This page only runs on this computer. A saved key goes to your OS credential store and is never shown again, here or in any config file.</p>",
    );
    if let Some((text, error)) = state.take_flash() {
        body.push_str(&format!(
            "<div class=\"flash {}\" role=\"status\">{}</div>",
            if error { "bad" } else { "ok" },
            escape(&redact(&text))
        ));
    }
    let layers = state.layers();
    let rows = layers
        .as_ref()
        .map(|l| ops::providers(l, &state.store))
        .unwrap_or_default();
    body.push_str("<section><h2>Your providers</h2>");
    if let Err(err) = &layers {
        body.push_str(&format!("<p class=\"status-bad\">config error: {}</p>", escape(&format!("{err:#}"))));
    }
    if rows.is_empty() {
        body.push_str("<p class=\"dim\">None yet. Add a key below to get started.</p>");
    } else {
        let cfg = layers.as_ref().map(|l| l.merged()).unwrap_or_default();
        body.push_str("<table><tr><th>Name</th><th>Kind</th><th>Key</th><th>Model</th><th></th></tr>");
        for row in &rows {
            let current = ops::model_for(&cfg, &row.name).unwrap_or_default();
            let cached = crate::quota::cached_models(&state.cache, &row.name);
            let mut choices: Vec<String> = ops::chat_models(&cached).iter().map(|m| m.id.clone()).collect();
            if !current.is_empty() && !choices.contains(&current) {
                choices.insert(0, current.clone());
            }
            let fetch = format!(
                "<form method=\"post\" action=\"/models\">{csrf}<input type=\"hidden\" name=\"provider\" value=\"{name}\"><button class=\"quiet\">{label}</button></form>",
                name = escape(&row.name),
                label = if cached.is_empty() { "Fetch models" } else { "Refresh" }
            );
            let model_cell = if choices.is_empty() {
                format!("<span class=\"dim\">{}</span>", if current.is_empty() { "not set".to_string() } else { escape(&current) })
            } else {
                let options: String = choices
                    .iter()
                    .map(|m| {
                        format!(
                            "<option value=\"{0}\"{1}>{0}</option>",
                            escape(m),
                            if *m == current { " selected" } else { "" }
                        )
                    })
                    .collect();
                format!(
                    "<form method=\"post\" action=\"/use\" class=\"pick\">{csrf}<input type=\"hidden\" name=\"provider\" value=\"{name}\"><select name=\"model\" aria-label=\"model for {name}\">{options}</select><button class=\"quiet\">Use</button></form><div class=\"hint\">{count} chat models</div>",
                    name = escape(&row.name),
                    count = choices.len()
                )
            };
            let has_key = row.key.is_some();
            let status = if has_key || !row.needs_key { "status-ok" } else { "status-bad" };
            let default = if row.is_default { " <span class=\"dim\">· default</span>" } else { "" };
            let mut actions = format!(
                "<form method=\"post\" action=\"/test\">{csrf}<input type=\"hidden\" name=\"provider\" value=\"{name}\"><button class=\"quiet\">Test</button></form>",
                name = escape(&row.name)
            );
            if matches!(row.key, Some(crate::auth::KeySource::Store) | Some(crate::auth::KeySource::File)) {
                actions.push_str(&format!(
                    "<form method=\"post\" action=\"/remove\">{csrf}<input type=\"hidden\" name=\"provider\" value=\"{name}\"><button class=\"danger\">Remove key</button></form>",
                    name = escape(&row.name)
                ));
            }
            actions.insert_str(0, &fetch);
            body.push_str(&format!(
                "<tr><td><strong>{}</strong>{default}</td><td class=\"dim\">{}</td><td class=\"{status}\">{}</td><td>{model_cell}</td><td><div class=\"actions\">{actions}</div></td></tr>",
                escape(&row.name),
                escape(&row.kind),
                escape(&row.key_label()),
            ));
        }
        body.push_str("</table>");
    }
    body.push_str("</section>");

    let mut options = String::from("<option value=\"auto\" selected>Detect from the key</option><optgroup label=\"Your providers\">");
    let mut helps = Vec::new();
    for row in rows.iter().filter(|r| r.needs_key || r.kind.contains("openai-compat")) {
        options.push_str(&format!(
            "<option value=\"{0}\">{0} ({1})</option>",
            escape(&row.name),
            escape(&row.kind)
        ));
    }
    options.push_str("</optgroup><optgroup label=\"Add a provider\">");
    for kind in catalog::KINDS.iter().filter(|k| k.needs_key) {
        if let Some(page) = kind.key_page {
            helps.push(format!("<a href=\"{page}\" target=\"_blank\" rel=\"noreferrer noopener\">{}</a>", escape(kind.label)));
        }
        if rows.iter().any(|r| r.name == kind.id) {
            continue;
        }
        options.push_str(&format!(
            "<option value=\"{}\">{}</option>",
            escape(kind.id),
            escape(kind.label)
        ));
    }
    options.push_str("</optgroup>");
    body.push_str(&format!(
        "<section><h2>Add or replace a key</h2><p class=\"dim\">Paste a key. zarn recognises most providers from the key itself, fills in their address, checks the key and loads every model it can use.</p><form method=\"post\" action=\"/save\" autocomplete=\"off\">{csrf}\
<label for=\"key\">API key</label><input id=\"key\" type=\"password\" name=\"key\" autocomplete=\"off\" spellcheck=\"false\" placeholder=\"paste the key\" required>\
<label for=\"provider\">Provider</label><select id=\"provider\" name=\"provider\">{options}</select>\
<label class=\"check\"><input type=\"checkbox\" name=\"test\" value=\"1\" checked> Check the key and load its models before saving</label>\
<div class=\"row\"><button>Save key</button></div></form>\
<details><summary class=\"hint\">Where do I get a key?</summary><div class=\"hint\">{}</div></details></section>",
        helps.join(" · ")
    ));
    body.push_str(&format!(
        "<section><h2>Your own OpenAI-compatible server</h2><p class=\"dim\">Only for servers that are not in the list above: vLLM, a company proxy, a self-hosted gateway. Leave the model empty to use the first one the server lists.</p><form method=\"post\" action=\"/custom\" autocomplete=\"off\">{csrf}\
<label for=\"name\">Name</label><input id=\"name\" type=\"text\" name=\"name\" placeholder=\"my-server\" required>\
<label for=\"base_url\">Base URL</label><input id=\"base_url\" type=\"url\" name=\"base_url\" placeholder=\"http://localhost:8000/v1\" required>\
<label for=\"model\">Model (optional)</label><input id=\"model\" type=\"text\" name=\"model\" placeholder=\"loaded from the server\">\
<label for=\"ckey\">API key (leave empty if none)</label><input id=\"ckey\" type=\"password\" name=\"key\" autocomplete=\"off\" spellcheck=\"false\">\
<label class=\"check\"><input type=\"checkbox\" name=\"test\" value=\"1\" checked> Send one small test request before saving</label>\
<div class=\"row\"><button>Add endpoint</button></div></form></section>"
    ));
    body.push_str(&format!(
        "<section class=\"done\"><form method=\"post\" action=\"/done\">{csrf}<button class=\"quiet\">Done, close this page</button></form><p class=\"hint\">The page also closes by itself after 30 idle minutes.</p></section>"
    ));
    page("zarn · API keys", &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_bodies_decode_plus_and_percent_escapes() {
        assert_eq!(
            form_decode("a=1+2&b=x%2By%26z&c=&d=%E2%9C%93&e=%zz"),
            vec![
                ("a".to_string(), "1 2".to_string()),
                ("b".to_string(), "x+y&z".to_string()),
                ("c".to_string(), String::new()),
                ("d".to_string(), "✓".to_string()),
                ("e".to_string(), "%zz".to_string()),
            ]
        );
    }

    #[test]
    fn html_is_escaped() {
        assert_eq!(escape("<a href=\"x\">'&'</a>"), "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;");
    }

    #[test]
    fn tokens_are_long_random_hex() {
        let a = random_token();
        let b = random_token();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
