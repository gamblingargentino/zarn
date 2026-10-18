mod support;

use std::io::{BufRead, BufReader, Read, Write};
use std::process::Stdio;
use support::*;

fn http(port: u16, method: &str, path: &str, token: &str, body: &str) -> String {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost:{port}\r\nCookie: zarn_setup={token}\r\nOrigin: http://localhost:{port}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn zarn_setup_serves_the_key_page_until_done() {
    let home = Home::new();
    let mut child = home
        .command(&["setup", "--no-browser", "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let url = lines
        .by_ref()
        .map_while(Result::ok)
        .find_map(|l| l.trim().strip_prefix("http://localhost:").map(str::to_string))
        .expect("a localhost link");
    let (port, query) = url.split_once('/').unwrap();
    let port: u16 = port.parse().unwrap();
    let token = query.split("token=").nth(1).unwrap().to_string();

    let saved = http(port, "POST", "/save", &token, &format!("csrf={token}&provider=google&key=test-key-not-real"));
    assert!(saved.starts_with("HTTP/1.1 303"), "{saved}");
    let said = lines.by_ref().map_while(Result::ok).find(|l| l.starts_with("google:")).unwrap();
    assert!(said.contains("key saved to the"), "{said}");
    assert!(!said.contains("test-key-not-real"));

    let done = http(port, "POST", "/done", &token, &format!("csrf={token}"));
    assert!(done.contains("All set"));
    let rest: Vec<String> = lines.map_while(Result::ok).collect();
    assert!(rest.iter().any(|l| l.contains("setup page is closed")), "{rest:?}");
    assert!(child.wait().unwrap().success());
    let config = std::fs::read_to_string(home.root().join("cfg/config.toml")).unwrap_or_default();
    assert!(config.contains("[providers.google]"), "{config}");
    assert!(!config.contains("test-key-not-real"));
}

#[test]
fn zarn_login_works_out_the_provider_from_the_key_and_loads_its_models() {
    let mock = Mock::start(std::sync::Arc::new(|seen: &Seen| {
        if seen.line.starts_with("GET /v1/models") {
            (200, "application/json".into(), "{\"data\":[{\"id\":\"claude-a\",\"max_input_tokens\":200000},{\"id\":\"claude-b\"}]}".into())
        } else {
            (404, "application/json".into(), "{}".into())
        }
    }));
    let home = Home::new();
    home.config(&format!(
        "[providers.claude]\nkind = \"anthropic\"\nbase_url = \"{}\"\n\n[profiles.default]\nprovider = \"claude\"\n",
        mock.base()
    ));
    let key = "sk-ant-test-key-not-real";
    let out = home.run(&["login"], &format!("{key}\n"));
    let text = stdout(&out);
    assert!(out.status.success(), "{text}{}", stderr(&out));
    assert!(text.contains("key for claude saved to"), "{text}");
    assert!(text.contains("check: OK · 2 models · using claude-a"), "{text}");
    assert!(!text.contains(key));
    assert!(mock.requests().iter().any(|r| r.headers.to_lowercase().contains(&format!("x-api-key: {key}"))));
    let config = std::fs::read_to_string(home.root().join("cfg/config.toml")).unwrap();
    assert!(config.contains("model = \"claude-a\""), "{config}");

    let unknown = home.run(&["login"], "test-key-not-real\n");
    assert!(!unknown.status.success());
    assert!(stderr(&unknown).contains("zarn login <provider>"), "{}", stderr(&unknown));
}
