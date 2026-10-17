#![cfg(unix)]

mod support;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;
use support::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zarn_tui::onboarding::Step;
use zarn_tui::state::Mode;

async fn http(url: &str, method: &str, path: &str, cookie: Option<&str>, body: &str) -> String {
    let rest = url.strip_prefix("http://localhost:").unwrap();
    let port: u16 = rest.split('/').next().unwrap().parse().unwrap();
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: localhost:{port}\r\nContent-Length: {}\r\n", body.len());
    if let Some(cookie) = cookie {
        request.push_str(&format!("Cookie: zarn_setup={cookie}\r\nContent-Type: application/x-www-form-urlencoded\r\nOrigin: http://localhost:{port}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    out
}

fn token(url: &str) -> String {
    url.split("token=").nth(1).unwrap().to_string()
}

#[tokio::test]
async fn first_run_setup_can_happen_in_the_browser() {
    let mock = echo_server("hello from the browser setup").await;
    let env = Env::new("").await;
    let mut c = env.controller();
    assert!(matches!(c.app().mode, Mode::Onboarding(_)));
    assert!(screen(&c, 100, 30).join("\n").contains("Set up in your browser"));
    c.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let url = c.key_page().expect("the setup page is running").to_string();
    assert!(url.starts_with("http://localhost:92"), "{url}");
    match &c.app().mode {
        Mode::Onboarding(flow) => assert!(matches!(flow.step, Step::Browser(_))),
        _ => panic!("left onboarding too early"),
    }
    assert!(screen(&c, 100, 30).join("\n").contains("http://localhost:"));

    let token = token(&url);
    let first = http(&url, "GET", &format!("/?token={token}"), None, "").await;
    assert!(first.starts_with("HTTP/1.1 303"), "{first}");
    let body = format!(
        "csrf={token}&name=mock&base_url={}&model=m&key=&test=1",
        mock.uri().replace(':', "%3A").replace('/', "%2F")
    );
    let saved = http(&url, "POST", "/custom", Some(&token), &body).await;
    assert!(saved.starts_with("HTTP/1.1 303"), "{saved}");

    wait_for(&mut c, Duration::from_secs(10), |c| matches!(c.app().mode, Mode::Chat)).await;
    assert!(matches!(c.app().mode, Mode::Chat));
    assert!(c.app().not_ready.is_none(), "{:?}", c.app().not_ready);
    assert!(notices(&c).iter().any(|n| n.contains("mock: endpoint added from the setup page")));
    turn(&mut c, "hi").await;
    assert_eq!(assistant(&c).last().map(String::as_str), Some("hello from the browser setup"));

    let done = http(&url, "POST", "/done", Some(&token), &format!("csrf={token}")).await;
    assert!(done.contains("All set"));
    wait_for(&mut c, Duration::from_secs(10), |c| notices(c).iter().any(|n| n.contains("setup page is closed"))).await;
    assert!(notices(&c).iter().any(|n| n.contains("setup page is closed")));
    assert!(c.key_page().is_none());
}

#[tokio::test]
async fn login_web_opens_the_page_and_a_saved_key_applies_at_once() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();
    c.app_mut().set_input("/login ");
    assert!(c.app().popup_matches().iter().any(|e| e.name == "web"));
    c.app_mut().set_input("");
    c.type_line("/login web");
    let url = c.key_page().expect("the setup page is running").to_string();
    assert!(notices(&c).iter().any(|n| n.contains(&url)));
    c.type_line("/login web");
    assert_eq!(c.key_page(), Some(url.as_str()), "a second /login web reuses the open page");

    let token = token(&url);
    let saved = http(&url, "POST", "/save", Some(&token), &format!("csrf={token}&provider=mock&key=test-key-not-real")).await;
    assert!(saved.starts_with("HTTP/1.1 303"), "{saved}");
    wait_for(&mut c, Duration::from_secs(10), |c| notices(c).iter().any(|n| n.contains("key saved from the setup page"))).await;
    assert!(env.store.has("mock", None).is_some());
    let page = http(&url, "GET", "/", Some(&token), "").await;
    assert!(page.contains("mock: key saved"));
    assert!(!page.contains("test-key-not-real"));
    assert!(!screen(&c, 100, 30).join("\n").contains("test-key-not-real"));
}

#[tokio::test]
async fn login_auto_recognises_the_key_checks_it_and_loads_the_models() {
    let chat = echo_server("ok").await;
    let models = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/models"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(
            "{\"data\":[{\"id\":\"claude-a\"},{\"id\":\"claude-b\"},{\"id\":\"claude-embed\"}]}",
        ))
        .mount(&models)
        .await;
    let env = Env::new(&format!(
        "{}\n[providers.claude]\nkind = \"anthropic\"\nbase_url = \"{}\"\n",
        provider_config(&chat.uri(), ""),
        models.uri()
    ))
    .await;
    let mut c = env.controller();
    c.type_line("/login auto");
    assert!(screen(&c, 100, 30).join("\n").contains("API key (any provider)"));
    c.paste("sk-ant-test-key-not-real");
    c.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    wait_for(&mut c, Duration::from_secs(10), |c| notices(c).iter().any(|n| n.contains("models found"))).await;
    let said = notices(&c);
    assert!(said.iter().any(|n| n.contains("claude: key saved to the") && n.contains("2 models found")), "{said:?}");
    assert!(env.store.has("claude", None).is_some());
    assert!(!screen(&c, 100, 30).join("\n").contains("sk-ant-test-key-not-real"));
    c.app_mut().set_input("/login ");
    assert!(c.app().popup_matches().iter().any(|e| e.name == "auto"));

    c.app_mut().set_input("");
    c.type_line("/login auto");
    c.paste("sk-test-key-not-real");
    c.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(notices(&c).last().unwrap().contains("could belong to OpenAI"), "{:?}", notices(&c));
}
