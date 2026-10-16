use zarn_core::types::{Content, Message};

pub fn markdown(title: Option<&str>, model: &str, messages: &[Message]) -> String {
    let mut out = format!("# {}\n\n", title.unwrap_or("zarn conversation"));
    if !model.is_empty() {
        out.push_str(&format!("Model: `{model}`\n\n"));
    }
    for message in messages {
        match message {
            Message::System(_) => {}
            Message::User(parts) => {
                out.push_str("## User\n\n");
                for part in parts {
                    match part {
                        Content::Text(text) => out.push_str(&format!("{}\n\n", text.trim_end())),
                        Content::Image { mime, data } => {
                            out.push_str(&format!("_[image: {mime}, {} bytes]_\n\n", data.len()))
                        }
                    }
                }
            }
            Message::Assistant { content, tool_calls } => {
                let text: String = content.iter().filter_map(Content::as_text).collect::<Vec<_>>().join("\n");
                if !text.trim().is_empty() || !tool_calls.is_empty() {
                    out.push_str("## Assistant\n\n");
                }
                if !text.trim().is_empty() {
                    out.push_str(&format!("{}\n\n", text.trim_end()));
                }
                for call in tool_calls {
                    let args = serde_json::to_string_pretty(&call.args).unwrap_or_default();
                    out.push_str(&format!("### Tool call: `{}`\n\n{}\n\n", call.name, fenced(&args, "json")));
                }
            }
            Message::ToolResult { content, is_error, .. } => {
                let label = if *is_error { "Tool error" } else { "Tool result" };
                out.push_str(&format!("### {label}\n\n{}\n\n", fenced(content.trim_end(), "")));
            }
        }
    }
    zarn_config::auth::redact(out.trim_end()) + "\n"
}

fn fenced(body: &str, lang: &str) -> String {
    let mut fence = "```".to_string();
    while body.contains(&fence) {
        fence.push('`');
    }
    format!("{fence}{lang}\n{body}\n{fence}")
}

pub fn last_reply(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(|m| match m {
        Message::Assistant { .. } if !m.text().trim().is_empty() => Some(m.text()),
        _ => None,
    })
}

pub fn clipboard_command(
    os: &str,
    env: impl Fn(&str) -> Option<String>,
    on_path: impl Fn(&str) -> bool,
) -> Option<Vec<String>> {
    let candidates: Vec<Vec<&str>> = match os {
        "macos" => vec![vec!["pbcopy"]],
        "windows" => vec![vec!["clip.exe"], vec!["clip"]],
        _ => {
            let mut list = Vec::new();
            if env("WAYLAND_DISPLAY").is_some() {
                list.push(vec!["wl-copy"]);
            }
            if env("DISPLAY").is_some() {
                list.push(vec!["xclip", "-selection", "clipboard"]);
                list.push(vec!["xsel", "--clipboard", "--input"]);
            }
            list.push(vec!["clip.exe"]);
            list
        }
    };
    candidates
        .into_iter()
        .find(|c| on_path(c[0]))
        .map(|c| c.into_iter().map(str::to_string).collect())
}

pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

pub fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

pub fn pipe_to(command: &[String], text: &str) -> Result<(), String> {
    use std::io::Write;
    let mut child = std::process::Command::new(&command[0])
        .args(&command[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{} exited with {status}", command[0]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zarn_core::types::ToolCall;

    #[test]
    fn export_renders_turns_and_redacts_keys() {
        zarn_config::auth::register_secret("test-key-not-real-export-1234");
        let messages = vec![
            Message::System("system".into()),
            Message::user("use test-key-not-real-export-1234 please"),
            Message::Assistant {
                content: vec![Content::Text("running it".into())],
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "bash".into(),
                    args: serde_json::json!({ "command": "echo ```" }),
                }],
            },
            Message::ToolResult { id: "1".into(), content: "```".into(), is_error: false },
            Message::assistant("done"),
        ];
        let md = markdown(Some("Demo"), "m1", &messages);
        assert!(md.starts_with("# Demo\n\nModel: `m1`\n\n## User\n\n"));
        assert!(!md.contains("test-key-not-real-export-1234"));
        assert!(md.contains(zarn_config::auth::REDACTED));
        assert!(md.contains("### Tool call: `bash`\n\n````json"));
        assert!(md.contains("### Tool result\n\n````\n```\n````"));
        assert!(md.ends_with("## Assistant\n\ndone\n"));
        assert!(!md.contains("system"));
        assert_eq!(last_reply(&messages).as_deref(), Some("done"));
    }

    #[test]
    fn the_clipboard_command_depends_on_the_platform() {
        let none = |_: &str| None;
        let all = |_: &str| true;
        assert_eq!(clipboard_command("macos", none, all).unwrap(), vec!["pbcopy"]);
        let wayland = |k: &str| (k == "WAYLAND_DISPLAY").then(|| "w".to_string());
        assert_eq!(clipboard_command("linux", wayland, all).unwrap(), vec!["wl-copy"]);
        let x = |k: &str| (k == "DISPLAY").then(|| ":0".to_string());
        let only_xsel = |c: &str| c == "xsel";
        assert_eq!(clipboard_command("linux", x, only_xsel).unwrap(), vec!["xsel", "--clipboard", "--input"]);
        assert_eq!(clipboard_command("linux", none, |_: &str| false), None);
    }

    #[test]
    fn osc52_carries_base64() {
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(base64(b"hi!"), "aGkh");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(osc52("hello"), "\x1b]52;c;aGVsbG8=\x07");
    }
}
