mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::*;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zarn_config::layers::Layers;
use zarn_config::webkeys::{self, Change, Options};

const GOOD: &str = "test-key-not-real";
const BAD: &str = "test-key-not-real-rejected";

struct Page {
    _dir: tempfile::TempDir,
    _keys: tempfile::TempDir,
    project: std::path::PathBuf,
    user: std::path::PathBuf,
    store: Arc<zarn_config::auth::KeyStore>,
    running: webkeys::Running,
    base: String,
    token: String,
    changes: Arc<Mutex<Vec<Change>>>,
    client: reqwest::Client,
    mock: MockServer,
}

async fn open() -> Page {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", format!("Bearer {BAD}").as_str()))
        .respond_with(ResponseTemplate::new(401).set_body_string("{\"error\":{\"message\":\"invalid key\"}}"))
        .with_priority(1)
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(reply_body("OK"))
                .insert_header("content-type", "text/event-stream"),
        )
        .with_priority(2)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("authorization", format!("Bearer {BAD}").as_str()))
        .respond_with(ResponseTemplate::new(401).set_body_string("{\"error\":{\"message\":\"invalid key\"}}"))
        .with_priority(1)
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "{\"data\":[{\"id\":\"m\"},{\"id\":\"m-large\",\"context_length\":128000},{\"id\":\"text-embedding-3\"}]}",
        ))
        .with_priority(2)
        .mount(&mock)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project.toml");
    let user = dir.path().join("user.toml");
    std::fs::write(
        &user,
        format!(
            "[providers.mock]\nkind = \"openai-compat\"\nbase_url = \"{0}\"\n\n[providers.claude]\nkind = \"anthropic\"\nbase_url = \"{0}\"\n\n[profiles.default]\nprovider = \"mock\"\nmodel = \"m\"\n",
            mock.uri()
        ),
    )
    .unwrap();
    let (keys, store) = store();
    let changes = Arc::new(Mutex::new(Vec::new()));
    let seen = changes.clone();
    let running = webkeys::start(Options {
        port: Some(0),
        store: store.clone(),
        project: project.clone(),
        user: Some(user.clone()),
        on_change: Some(Arc::new(move |c| seen.lock().unwrap().push(c))),
        idle: Duration::from_secs(60),
        cache: dir.path().join("discovery"),
    })
    .unwrap();
    let token = running.url.split("token=").nth(1).unwrap().to_string();
    assert_eq!(running.url, format!("http://localhost:{}/?token={token}", running.port));
    Page {
        _dir: dir,
        _keys: keys,
        project,
        user,
        store,
        base: format!("http://127.0.0.1:{}", running.port),
        running,
        token,
        changes,
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap(),
        mock,
    }
}

impl Page {
    fn cookie(&self) -> String {
        format!("zarn_setup={}", self.token)
    }

    async fn home(&self) -> String {
        let response = self
            .client
            .get(format!("{}/", self.base))
            .header("cookie", self.cookie())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response.text().await.unwrap()
    }

    async fn post(&self, path: &str, fields: &[(&str, &str)]) -> reqwest::Response {
        let mut body: Vec<(String, String)> = fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        body.push(("csrf".into(), self.token.clone()));
        let encoded: Vec<String> = body
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect();
        self.client
            .post(format!("{}{path}", self.base))
            .header("cookie", self.cookie())
            .header("origin", self.base.clone())
            .header("content-type", "application/x-www-form-urlencoded")
            .body(encoded.join("&"))
            .send()
            .await
            .unwrap()
    }
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            b' ' => "+".to_string(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

#[tokio::test]
async fn the_link_sets_a_cookie_and_nothing_works_without_it_or_from_another_host() {
    let page = open().await;
    let first = page
        .client
        .get(format!("{}/?token={}", page.base, page.token))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 303);
    let cookie = first.headers().get("set-cookie").unwrap().to_str().unwrap().to_string();
    assert!(cookie.starts_with(&page.cookie()) && cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"), "{cookie}");

    let wrong = page.client.get(format!("{}/?token=0000", page.base)).send().await.unwrap();
    assert_eq!(wrong.status(), 403);
    let bare = page.client.get(format!("{}/", page.base)).send().await.unwrap();
    assert_eq!(bare.status(), 403);
    let rebound = page
        .client
        .get(format!("{}/", page.base))
        .header("host", format!("attacker.example:{}", page.running.port))
        .header("cookie", page.cookie())
        .send()
        .await
        .unwrap();
    assert_eq!(rebound.status(), 403, "a DNS-rebinding host is refused");

    let home = page.home().await;
    assert!(home.contains("Your providers") && home.contains("mock"));
    let headers = page
        .client
        .get(format!("{}/", page.base))
        .header("cookie", page.cookie())
        .send()
        .await
        .unwrap();
    let csp = headers.headers().get("content-security-policy").unwrap().to_str().unwrap();
    assert!(csp.contains("default-src 'none'") && csp.contains("frame-ancestors 'none'"));
    assert_eq!(headers.headers().get("cache-control").unwrap(), "no-store");

    let cross = page
        .client
        .post(format!("{}/save", page.base))
        .header("cookie", page.cookie())
        .header("origin", "http://attacker.example")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("provider=mock&key={GOOD}&csrf={}", page.token))
        .send()
        .await
        .unwrap();
    assert_eq!(cross.status(), 403);
    let no_csrf = page
        .client
        .post(format!("{}/save", page.base))
        .header("cookie", page.cookie())
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("provider=mock&key={GOOD}"))
        .send()
        .await
        .unwrap();
    assert_eq!(no_csrf.status(), 403);
    assert!(page.store.has("mock", None).is_none(), "no refused request stored a key");
}

#[tokio::test]
async fn a_key_is_checked_saved_and_its_models_loaded_and_it_is_never_shown_back() {
    let page = open().await;
    let saved = page.post("/save", &[("provider", "mock"), ("key", GOOD), ("test", "1")]).await;
    assert_eq!(saved.status(), 303);
    let home = page.home().await;
    assert!(home.contains("mock: key saved to the"), "{home}");
    assert!(home.contains("3 models found") && home.contains("using m"), "{home}");
    assert!(home.contains("<option value=\"m-large\">m-large</option>"), "{home}");
    assert!(!home.contains("<option value=\"text-embedding-3\""), "embedding models are not offered for chat");
    assert!(!home.contains(GOOD), "the key never comes back to the browser");
    assert!(page.store.has("mock", None).is_some());
    let sent = page.mock.received_requests().await.unwrap();
    assert!(sent.iter().any(|r| r.url.path() == "/models" && r.headers.get("authorization").is_some_and(|v| v == &format!("Bearer {GOOD}"))));
    assert!(matches!(&page.changes.lock().unwrap()[..], [Change::Saved { provider, .. }] if provider == "mock"));
    let cached = zarn_config::quota::cached_models(&page._dir.path().join("discovery"), "mock");
    assert_eq!(cached.len(), 3);

    let rejected = page.post("/save", &[("provider", "mock"), ("key", BAD), ("test", "1")]).await;
    assert_eq!(rejected.status(), 303);
    let home = page.home().await;
    assert!(home.contains("the key was not saved"), "{home}");
    assert!(!home.contains(BAD));
    let (stored, _) = page.store.resolve("mock", None).unwrap();
    assert_eq!(stored, GOOD, "a key that fails its check does not replace the working one");

    page.post("/use", &[("provider", "mock"), ("model", "m-large")]).await;
    assert!(page.home().await.contains("profile default now uses mock · m-large"));
    let cfg = Layers::load_from(page.project.clone(), Some(page.user.clone())).unwrap().merged();
    assert_eq!(cfg.profiles["default"].model.as_deref(), Some("m-large"));
    assert!(page.changes.lock().unwrap().contains(&Change::Updated("mock".into())));

    let removed = page.post("/remove", &[("provider", "mock")]).await;
    assert_eq!(removed.status(), 303);
    assert!(page.store.has("mock", None).is_none());
    assert!(page.home().await.contains("mock: key removed"));
}

#[tokio::test]
async fn a_key_is_matched_to_its_provider_and_an_ambiguous_one_goes_nowhere() {
    let page = open().await;
    let claude_key = "sk-ant-test-key-not-real";
    page.post("/save", &[("provider", "auto"), ("key", claude_key), ("test", "1")]).await;
    let home = page.home().await;
    assert!(home.contains("claude: key saved to the"), "{home}");
    assert!(page.store.has("claude", None).is_some(), "the configured anthropic provider got the key");
    let sent = page.mock.received_requests().await.unwrap();
    assert!(sent.iter().any(|r| r.headers.get("x-api-key").is_some_and(|v| v == claude_key)));

    let before = page.mock.received_requests().await.unwrap().len();
    page.post("/save", &[("provider", "auto"), ("key", "sk-test-key-not-real")]).await;
    let home = page.home().await;
    assert!(home.contains("could belong to OpenAI, DeepSeek, Moonshot (Kimi), Alibaba Qwen (DashScope)"), "{home}");
    page.post("/save", &[("provider", "auto"), ("key", "test-key-not-real")]).await;
    assert!(page.home().await.contains("cannot tell which provider"));
    assert_eq!(page.mock.received_requests().await.unwrap().len(), before, "an unrecognised key is sent nowhere");

    page.post("/models", &[("provider", "mock")]).await;
    assert!(page.home().await.contains("mock: 3 models found"));
    let home = page.home().await;
    assert!(home.contains("Detect from the key") && home.contains("OpenRouter") && home.contains("Groq") && home.contains("xAI"));
}

#[tokio::test]
async fn a_new_catalog_provider_and_a_custom_endpoint_are_added_to_the_config() {
    let page = open().await;
    page.post("/save", &[("provider", "google"), ("key", GOOD)]).await;
    let cfg = Layers::load_from(page.project.clone(), Some(page.user.clone())).unwrap().merged();
    assert_eq!(cfg.providers.get("google").map(|p| p.kind.as_str()), Some("google"));
    assert!(page.store.has("google", None).is_some());
    assert!(!std::fs::read_to_string(&page.user).unwrap().contains(GOOD), "keys never land in a config file");

    page.post(
        "/custom",
        &[("name", "selfhosted"), ("base_url", &page.mock.uri()), ("key", GOOD), ("test", "1")],
    )
    .await;
    let home = page.home().await;
    assert!(home.contains("selfhosted added · 3 models found · using m"), "{home}");
    let cfg = Layers::load_from(page.project.clone(), Some(page.user.clone())).unwrap().merged();
    assert_eq!(cfg.providers.get("selfhosted").and_then(|p| p.base_url.clone()), Some(page.mock.uri()));

    page.post("/save", &[("provider", "groq"), ("key", "gsk_test-key-not-real")]).await;
    let cfg = Layers::load_from(page.project.clone(), Some(page.user.clone())).unwrap().merged();
    let groq = cfg.providers.get("groq").expect("groq added from its preset");
    assert_eq!(groq.kind, "openai-compat");
    assert_eq!(groq.base_url.as_deref(), Some("https://api.groq.com/openai/v1"));

    page.post("/custom", &[("name", "../x"), ("base_url", "http://h")]).await;
    assert!(page.home().await.contains("not a valid name"));
}

#[tokio::test]
async fn done_closes_the_server() {
    let mut page = open().await;
    let done = page.post("/done", &[]).await;
    assert_eq!(done.status(), 200);
    assert!(done.text().await.unwrap().contains("All set"));
    tokio::time::timeout(Duration::from_secs(5), page.running.wait()).await.unwrap();
    assert!(page.running.is_stopped());
    for _ in 0..50 {
        if page.changes.lock().unwrap().contains(&Change::Closed) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(page.changes.lock().unwrap().contains(&Change::Closed));
    assert!(page.client.get(format!("{}/", page.base)).send().await.is_err());
}

#[tokio::test]
async fn the_default_port_is_a_fixed_four_digit_number() {
    let (_keys, store) = store();
    let running = webkeys::start(Options {
        idle: Duration::from_secs(5),
        ..Options::new(store)
    })
    .unwrap();
    assert!((webkeys::DEFAULT_PORT..=*webkeys::FALLBACK_PORTS.end()).contains(&running.port));
    assert_eq!(webkeys::DEFAULT_PORT, 9276);
    running.stop();
}

#[tokio::test]
async fn forms_work_with_the_headers_a_real_browser_sends() {
    let page = open().await;
    page.post("/save", &[("provider", "mock"), ("key", GOOD)]).await;
    let home = page
        .client
        .get(format!("{}/", page.base))
        .header("cookie", page.cookie())
        .send()
        .await
        .unwrap();
    assert_eq!(home.headers().get("referrer-policy").unwrap(), "same-origin", "no-referrer makes browsers send Origin: null");
    let browser_post = |origin: &'static str, site: &'static str| {
        page.client
            .post(format!("{}/remove", page.base))
            .header("cookie", page.cookie())
            .header("origin", origin)
            .header("sec-fetch-site", site)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(format!("provider=mock&csrf={}", page.token))
            .send()
    };
    let cross = browser_post("http://attacker.example", "cross-site").await.unwrap();
    assert_eq!(cross.status(), 403);
    assert!(cross.text().await.unwrap().contains("Back to the setup page"));
    assert!(page.store.has("mock", None).is_some());
    let removed = browser_post("null", "same-origin").await.unwrap();
    assert_eq!(removed.status(), 303, "a same-origin form with Origin: null is the browser's own page");
    assert!(page.store.has("mock", None).is_none());
}
