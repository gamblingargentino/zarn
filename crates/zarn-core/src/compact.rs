use crate::provider::{Event, Provider, ProviderError};
use crate::types::{Message, Request};
use futures::StreamExt;

pub const KEEP_TURNS: usize = 3;
pub const TRIGGER: f32 = 0.8;

const INSTRUCTION: &str = "Summarize the conversation so far for an engineer who will continue it with no other context. Preserve, under headings: the user's stated goal; every file read or written, with its path; decisions made and why; problems still unresolved. Be specific. Do not add anything that was not in the conversation.";

pub fn usage_ratio(provider: &dyn Provider, messages: &[Message]) -> f32 {
    let window = provider.caps().context_window;
    if window == 0 {
        return 0.0;
    }
    let used = provider.count_tokens(messages).unwrap_or(0);
    used as f32 / window as f32
}

pub fn should_compact(provider: &dyn Provider, messages: &[Message]) -> bool {
    usage_ratio(provider, messages) >= TRIGGER
}

fn turn_starts(messages: &[Message]) -> Vec<usize> {
    messages
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m, Message::User(_)))
        .map(|(i, _)| i)
        .collect()
}

pub fn split_point(messages: &[Message], keep_turns: usize) -> Option<usize> {
    let starts = turn_starts(messages);
    if starts.len() <= keep_turns + 1 {
        return None;
    }
    let cut = starts[starts.len() - keep_turns];
    if cut <= starts[0] { None } else { Some(cut) }
}

pub async fn compact(
    provider: &dyn Provider,
    messages: &[Message],
    model: &str,
    keep_turns: usize,
) -> Result<Vec<Message>, ProviderError> {
    let Some(cut) = split_point(messages, keep_turns) else {
        return Ok(messages.to_vec());
    };

    let head = &messages[..cut];
    let tail = &messages[cut..];
    let summary = summarize(provider, head, model).await?;

    let mut out: Vec<Message> = Vec::with_capacity(tail.len() + 3);
    if let Some(system) = messages.first().filter(|m| matches!(m, Message::System(_))) {
        out.push(system.clone());
    }
    if let Some(first_user) = head.iter().find(|m| matches!(m, Message::User(_))) {
        out.push(first_user.clone());
    }
    out.push(Message::System(format!(
        "Summary of the earlier conversation:\n\n{summary}"
    )));
    out.extend_from_slice(tail);
    Ok(out)
}

async fn summarize(
    provider: &dyn Provider,
    head: &[Message],
    model: &str,
) -> Result<String, ProviderError> {
    let transcript = head
        .iter()
        .map(|m| {
            let role = match m {
                Message::System(_) => "system",
                Message::User(_) => "user",
                Message::Assistant { .. } => "assistant",
                Message::ToolResult { .. } => "tool",
            };
            format!("[{role}] {}", m.text())
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let request = Request {
        messages: vec![
            Message::System(INSTRUCTION.to_string()),
            Message::user(transcript),
        ],
        tools: Vec::new(),
        model: model.to_string(),
        max_tokens: 2048,
        temperature: 0.0,
    };

    let mut stream = provider.stream(request).await?;
    let mut summary = String::new();
    while let Some(event) = stream.next().await {
        match event {
            Event::TextDelta(text) => summary.push_str(&text),
            Event::Error(err) => return Err(err),
            _ => {}
        }
    }

    if summary.trim().is_empty() {
        return Err(ProviderError::Other(
            "the model returned an empty summary".into(),
        ));
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Caps;
    use crate::types::StopReason;
    use async_trait::async_trait;
    use futures::stream::BoxStream;

    struct Stub {
        window: u32,
        tokens: u32,
    }

    #[async_trait]
    impl Provider for Stub {
        async fn stream(&self, _req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
            Ok(futures::stream::iter(vec![
                Event::TextDelta("goal: ship it".into()),
                Event::Done(StopReason::EndTurn),
            ])
            .boxed())
        }

        fn caps(&self) -> Caps {
            Caps {
                context_window: self.window,
                ..Caps::default()
            }
        }

        fn count_tokens(&self, _msgs: &[Message]) -> Result<u32, ProviderError> {
            Ok(self.tokens)
        }
    }

    fn convo(turns: usize) -> Vec<Message> {
        let mut messages = vec![Message::System("you are zarn".into())];
        for i in 0..turns {
            messages.push(Message::user(format!("ask {i}")));
            messages.push(Message::assistant(format!("answer {i}")));
        }
        messages
    }

    #[test]
    fn the_trigger_is_eighty_percent_of_the_window() {
        let under = Stub {
            window: 1000,
            tokens: 799,
        };
        let over = Stub {
            window: 1000,
            tokens: 800,
        };
        assert!(!should_compact(&under, &[]));
        assert!(should_compact(&over, &[]));
    }

    #[test]
    fn a_short_conversation_has_nothing_to_cut() {
        assert_eq!(split_point(&convo(3), KEEP_TURNS), None);
        assert_eq!(split_point(&convo(4), KEEP_TURNS), None);
    }

    #[tokio::test]
    async fn compaction_keeps_the_first_user_message_and_the_last_three_turns() {
        let provider = Stub {
            window: 1000,
            tokens: 900,
        };
        let messages = convo(6);
        let out = compact(&provider, &messages, "m", KEEP_TURNS).await.unwrap();

        assert!(matches!(out[0], Message::System(_)));
        assert_eq!(out[1].text(), "ask 0");
        assert!(out[2].text().contains("goal: ship it"));
        assert_eq!(out[3].text(), "ask 3");
        assert_eq!(out.last().unwrap().text(), "answer 5");
    }

    #[tokio::test]
    async fn compaction_never_splits_a_tool_call_from_its_result() {
        let provider = Stub {
            window: 1000,
            tokens: 900,
        };
        let mut messages = convo(5);
        messages.push(Message::user("ask 5"));
        messages.push(Message::Assistant {
            content: Vec::new(),
            tool_calls: vec![crate::types::ToolCall {
                id: "t1".into(),
                name: "read".into(),
                args: serde_json::json!({}),
            }],
        });
        messages.push(Message::ToolResult {
            id: "t1".into(),
            content: "contents".into(),
            is_error: false,
        });

        let out = compact(&provider, &messages, "m", KEEP_TURNS).await.unwrap();

        let orphan = out.iter().enumerate().any(|(i, m)| {
            matches!(m, Message::ToolResult { .. })
                && !matches!(out.get(i.wrapping_sub(1)), Some(Message::Assistant { .. }))
        });
        assert!(!orphan);
    }

    #[tokio::test]
    async fn an_empty_summary_is_an_error_rather_than_silent_data_loss() {
        struct Empty;

        #[async_trait]
        impl Provider for Empty {
            async fn stream(
                &self,
                _req: Request,
            ) -> Result<BoxStream<'static, Event>, ProviderError> {
                Ok(futures::stream::iter(vec![Event::Done(StopReason::EndTurn)]).boxed())
            }

            fn caps(&self) -> Caps {
                Caps::default()
            }

            fn count_tokens(&self, _msgs: &[Message]) -> Result<u32, ProviderError> {
                Ok(0)
            }
        }

        let result = compact(&Empty, &convo(6), "m", KEEP_TURNS).await;
        assert!(result.is_err());
    }
}
