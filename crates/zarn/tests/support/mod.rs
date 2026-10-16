#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct Seen {
    pub line: String,
    pub headers: String,
    pub body: String,
}

pub type Handler = Arc<dyn Fn(&Seen) -> (u16, String, String) + Send + Sync>;

pub struct Mock {
    pub port: u16,
    pub seen: Arc<Mutex<Vec<Seen>>>,
}

impl Mock {
    pub fn start(handler: Handler) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let handler = handler.clone();
                let log = log.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() {
                        return;
                    }
                    let mut headers = String::new();
                    let mut length = 0usize;
                    loop {
                        let mut h = String::new();
                        if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                            break;
                        }
                        if let Some(v) = h.to_lowercase().strip_prefix("content-length:") {
                            length = v.trim().parse().unwrap_or(0);
                        }
                        headers.push_str(&h);
                    }
                    let mut body = vec![0u8; length];
                    let _ = reader.read_exact(&mut body);
                    let request = Seen {
                        line,
                        headers,
                        body: String::from_utf8_lossy(&body).to_string(),
                    };
                    let (status, content_type, payload) = handler(&request);
                    log.lock().unwrap().push(request);
                    let mut stream = stream;
                    let response = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                });
            }
        });
        Mock { port, seen }
    }

    pub fn openai(text: &str) -> Self {
        let text = text.to_string();
        Self::start(Arc::new(move |_| (200, "text/event-stream".into(), sse(&text))))
    }

    pub fn status(code: u16) -> Self {
        Self::start(Arc::new(move |_| {
            (code, "application/json".into(), "{\"error\":{\"message\":\"nope\"}}".into())
        }))
    }

    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    pub fn models(&self) -> Vec<String> {
        self.requests()
            .iter()
            .filter_map(|r| serde_json::from_str::<serde_json::Value>(&r.body).ok())
            .filter_map(|v| v.get("model").and_then(|m| m.as_str()).map(str::to_string))
            .collect()
    }
}

pub fn sse(text: &str) -> String {
    let content = serde_json::to_string(text).unwrap();
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{content}}}}}]}}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":10,\"completion_tokens\":2}}}}\n\ndata: [DONE]\n\n"
    )
}

pub fn tool_call_sse(id: &str, name: &str, args: &serde_json::Value) -> String {
    let args = serde_json::to_string(&args.to_string()).unwrap();
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"{id}\",\"function\":{{\"name\":\"{name}\",\"arguments\":{args}}}}}]}}}}]}}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}],\"usage\":{{\"prompt_tokens\":5,\"completion_tokens\":8}}}}\n\ndata: [DONE]\n\n"
    )
}

pub struct Home {
    pub dir: tempfile::TempDir,
}

impl Home {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("project")).unwrap();
        std::fs::create_dir_all(dir.path().join("cfg")).unwrap();
        Home { dir }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn project(&self) -> PathBuf {
        self.root().join("project")
    }

    pub fn config(&self, body: &str) {
        std::fs::write(self.root().join("cfg/config.toml"), body).unwrap();
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_zarn"));
        command
            .args(args)
            .current_dir(self.project())
            .env("HOME", self.root())
            .env("USERPROFILE", self.root())
            .env("ZARN_CONFIG_DIR", self.root().join("cfg"))
            .env("ZARN_DATA_DIR", self.root().join("data"))
            .env("ZARN_CREDENTIAL_STORE", "file")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .env_remove("ZARN_PROFILE")
            .env_remove("ZARN_MODEL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    pub fn run(&self, args: &[&str], stdin: &str) -> Output {
        let mut child = self.command(args).spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}
