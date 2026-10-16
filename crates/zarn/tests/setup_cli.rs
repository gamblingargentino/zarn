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
