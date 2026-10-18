use crate::auth::redact;
use futures::StreamExt;
use std::time::Duration;
use zarn_core::provider::{Event, Provider};
use zarn_core::types::{Message, Request};

pub const TIMEOUT: Duration = Duration::from_secs(30);
const INSTRUCTION: &str = "Write a title of three to six words for this conversation. Reply with the title only: no quotes, no punctuation at the end.";

pub fn clean(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line
        .trim_start_matches(['#', '*', ' '])
        .trim_start_matches("Title:")
        .trim()
        .trim_matches(['"', '\'', '`', '*'])
        .trim_end_matches(['.', '!', ':'])
        .trim();
    if line.is_empty() {
        return None;
    }
    let title: String = line.chars().take(60).collect();
    Some(redact(&title))
}

fn excerpt(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

pub async fn generate(
    provider: &dyn Provider,
    model: &str,
    first_user: &str,
    first_reply: &str,
) -> Result<String, String> {
    let mut request = Request::new(
        model,
        vec![
            Message::System(INSTRUCTION.to_string()),
            Message::user(format!(
                "User: {}\n\nAssistant: {}",
                excerpt(first_user, 2000),
                excerpt(first_reply, 2000)
            )),
        ],
    );
    request.max_tokens = 32;
    request.temperature = 0.2;
    let run = async {
        let mut stream = provider.stream(request).await.map_err(|e| e.to_string())?;
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            match event {
                Event::TextDelta(delta) => text.push_str(&delta),
                Event::Error(err) => return Err(err.to_string()),
                Event::Done(_) => break,
                _ => {}
            }
        }
        Ok(text)
    };
    let text = tokio::time::timeout(TIMEOUT, run)
        .await
        .map_err(|_| "the title request timed out".to_string())?
        .map_err(|e| redact(&e))?;
    clean(&text).ok_or_else(|| "the model returned an empty title".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_are_trimmed_to_one_clean_line() {
        assert_eq!(clean("\"Fix the parser.\"\nextra").as_deref(), Some("Fix the parser"));
        assert_eq!(clean("Title: Add retries").as_deref(), Some("Add retries"));
        assert_eq!(clean("   \n  "), None);
        assert_eq!(clean(&"word ".repeat(40)).unwrap().chars().count(), 60);
    }
}
