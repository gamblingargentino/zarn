use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const KEY: &str = "test-key-not-real-cli-0123456789";
const NEW_KEY: &str = "test-key-not-real-cli-replaced99";

fn mock_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0usize;
                let mut authorized = false;
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() {
                    return;
                }
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() || line == "\r\n" || line.is_empty() {
                        break;
                    }
                    let lower = line.to_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap_or(0);
                    }
                    if lower.starts_with("authorization:")
                        && (line.contains(KEY) || line.contains(NEW_KEY))
                    {
                        authorized = true;
                    }
                }
                let mut body = vec![0u8; length];
                let _ = reader.read_exact(&mut body);
                let mut stream = stream;
                let response = if request_line.starts_with("GET") {
                    "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok".to_string()
                } else if authorized {
                    let events = "data: {\"choices\":[{\"delta\":{\"content\":\"OK\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{events}",
                        events.len()
                    )
                } else {
                    let body = "{\"error\":{\"message\":\"invalid api key\"}}";
                    format!(
                        "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                let _ = stream.write_all(response.as_bytes());
            });
        }
    });
    port
}

struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("project")).unwrap();
        Home { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn zarn(&self, args: &[&str], stdin: &str) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_zarn"))
            .args(args)
            .current_dir(self.root().join("project"))
            .env("HOME", self.root())
            .env("USERPROFILE", self.root())
            .env("ZARN_CONFIG_DIR", self.root().join("cfg"))
            .env("ZARN_DATA_DIR", self.root().join("data"))
            .env("ZARN_CREDENTIAL_STORE", "file")
            .env("ZARN_LOG", "trace")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .env_remove("ZARN_PROFILE")
            .env_remove("ZARN_MODEL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn files(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push(path);
                }
            }
        }
        let mut out = Vec::new();
        walk(self.root(), &mut out);
        out
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn the_cli_adds_logs_in_tests_and_never_writes_or_prints_the_key() {
    let port = mock_server();
    let home = Home::new();
    let base = format!("http://127.0.0.1:{port}/v1");
    let mut transcript = String::new();

    let fresh = home.zarn(&["--print", "hi"], "");
    assert!(!fresh.status.success());
    assert!(text(&fresh).contains("no provider is configured"));
    assert!(!text(&fresh).contains("Stack backtrace"));

    let added = home.zarn(
        &["providers", "add", "--kind", "openai-compat", "--name", "mock", "--base-url", &base, "--model", "m"],
        &format!("{KEY}\n"),
    );
    transcript.push_str(&text(&added));
    assert!(added.status.success(), "{}", text(&added));
    assert!(text(&added).contains("test: OK"), "{}", text(&added));

    let listed = home.zarn(&["providers", "list"], "");
    transcript.push_str(&text(&listed));
    assert!(text(&listed).contains("key in fallback file"));

    let replaced = home.zarn(&["login", "mock"], &format!("{NEW_KEY}\n"));
    transcript.push_str(&text(&replaced));
    assert!(replaced.status.success(), "{}", text(&replaced));

    let printed = home.zarn(&["--print", "say ok"], "");
    transcript.push_str(&text(&printed));
    assert!(printed.status.success(), "{}", text(&printed));
    assert!(String::from_utf8_lossy(&printed.stdout).contains("OK"));

    let tested = home.zarn(&["providers", "test", "mock"], "");
    transcript.push_str(&text(&tested));
    assert!(tested.status.success());

    let doctor = home.zarn(&["doctor", "--offline"], "");
    transcript.push_str(&text(&doctor));

    for secret in [KEY, NEW_KEY] {
        assert!(!transcript.contains(secret), "a key was printed or logged");
        for file in home.files() {
            if file.file_name().is_some_and(|n| n == "auth.json") {
                continue;
            }
            let body = String::from_utf8_lossy(&std::fs::read(&file).unwrap()).to_string();
            assert!(!body.contains(secret), "a key leaked into {}", file.display());
        }
    }
    let config = std::fs::read_to_string(home.root().join("cfg/config.toml")).unwrap();
    assert!(config.contains("[providers.mock]"));
    assert!(!config.contains("api_key"));

    let logout = home.zarn(&["logout", "mock"], "");
    assert!(logout.status.success());
    assert!(!home.root().join("cfg/auth.json").exists());
    let after = home.zarn(&["providers", "test", "mock"], "");
    assert!(!after.status.success());
}

#[cfg(unix)]
#[test]
fn the_fallback_key_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let home = Home::new();
    let out = home.zarn(&["login", "anthropic"], &format!("{KEY}\n"));
    assert!(text(&out).contains("fallback file"), "{}", text(&out));
    let mode = std::fs::metadata(home.root().join("cfg/auth.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn a_literal_key_in_a_config_file_is_rejected() {
    let home = Home::new();
    std::fs::create_dir_all(home.root().join("cfg")).unwrap();
    std::fs::write(
        home.root().join("cfg/config.toml"),
        format!("[providers.x]\nkind = \"openai\"\napi_key = \"{KEY}\"\n"),
    )
    .unwrap();
    let out = home.zarn(&["providers", "list"], "");
    assert!(!out.status.success());
    let shown = text(&out);
    assert!(shown.contains("inline"));
    assert!(!shown.contains(KEY));
}

#[test]
fn mcp_and_skills_commands_round_trip_through_the_config() {
    let home = Home::new();
    let added = home.zarn(&["mcp", "add", "files", "--", "npx", "-y", "server", "."], "");
    assert!(added.status.success(), "{}", text(&added));
    let listed = home.zarn(&["mcp", "list", "--offline"], "");
    assert!(text(&listed).contains("npx -y server ."));
    assert!(home.zarn(&["mcp", "remove", "files"], "").status.success());
    assert!(text(&home.zarn(&["mcp", "list", "--offline"], "")).contains("no MCP servers"));

    let skill = home.root().join("cfg/skills/pdf");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "---\nname: pdf\ndescription: forms\n---\nbody").unwrap();
    assert!(text(&home.zarn(&["skills", "list"], "")).contains("[on ] pdf"));
    assert!(home.zarn(&["skills", "disable", "pdf"], "").status.success());
    assert!(text(&home.zarn(&["skills", "list"], "")).contains("[off] pdf"));
}
