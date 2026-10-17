use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zarn_config::update;

const NEW_BINARY: &[u8] = b"pretend this is zarn 9.9.9";

async fn github(sums: &str) -> MockServer {
    let mock = MockServer::start().await;
    let name = update::asset_name(&update::target());
    let release = serde_json::json!({
        "tag_name": "v9.9.9",
        "html_url": format!("{}/releases/v9.9.9", mock.uri()),
        "assets": [
            { "name": name, "browser_download_url": format!("{}/download/{name}", mock.uri()) },
            { "name": "SHA256SUMS", "browser_download_url": format!("{}/download/SHA256SUMS", mock.uri()) },
        ],
    });
    Mock::given(method("GET"))
        .and(path("/repos/acme/zarn/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/download/{name}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(NEW_BINARY))
        .mount(&mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/download/SHA256SUMS"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sums.replace("{name}", &name)))
        .mount(&mock)
        .await;
    mock
}

fn point_at(mock: &MockServer) {
    unsafe {
        std::env::set_var(update::API_ENV, mock.uri());
        std::env::set_var(update::REPO_ENV, "acme/zarn");
    }
}

#[tokio::test]
async fn updates_replace_the_binary_only_when_the_checksum_matches() {
    let good = format!("{}  {{name}}\n", update::sha256_hex(NEW_BINARY));
    let mock = github(&good).await;
    point_at(&mock);
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("zarn");
    std::fs::write(&exe, b"old zarn").unwrap();

    let same = update::update("9.9.9", &update::target(), &exe).await.unwrap();
    assert_eq!(same, update::Outcome::UpToDate("9.9.9".into()));
    assert_eq!(std::fs::read(&exe).unwrap(), b"old zarn");

    let done = update::update("0.1.0", &update::target(), &exe).await.unwrap();
    assert!(done.summary().contains("0.1.0 → 9.9.9"), "{}", done.summary());
    assert_eq!(std::fs::read(&exe).unwrap(), NEW_BINARY);

    let seen = dir.path().join("seen.json");
    let text = update::notice_at("0.1.0", &seen, Duration::from_secs(3600)).await.unwrap();
    assert!(text.contains("zarn 9.9.9 is available"), "{text}");
    let before = mock.received_requests().await.unwrap().len();
    assert!(update::notice_at("0.1.0", &seen, Duration::from_secs(3600)).await.is_some());
    assert_eq!(mock.received_requests().await.unwrap().len(), before, "a fresh check is cached for a day");
    assert!(update::notice_at("9.9.9", &seen, Duration::from_secs(3600)).await.is_none());

    drop(mock);
    let bad = github(&format!("{}  {{name}}\n", "0".repeat(64))).await;
    point_at(&bad);
    std::fs::write(&exe, b"old zarn").unwrap();
    let err = update::update("0.1.0", &update::target(), &exe).await.unwrap_err();
    assert!(err.to_string().contains("does not match its published checksum"), "{err}");
    assert_eq!(std::fs::read(&exe).unwrap(), b"old zarn", "a bad download never replaces the binary");
}
