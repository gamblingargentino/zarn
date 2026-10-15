use crate::provider::{Caps, Event, Provider, ProviderError, ToolSupport};
use crate::types::{Message, Request, ToolSpec};
use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use std::sync::Arc;

const OPEN: &str = "<tool_use>";
const CLOSE: &str = "</tool_use>";

pub fn system_prompt(tools: &[ToolSpec]) -> String {
    let mut out = String::from(
        "You can call tools. To call one, emit a block exactly in this form:\n\n<tool_use>\n<name>TOOL_NAME</name>\n<args>{\"arg\": \"value\"}</args>\n</tool_use>\n\nThe args must be a single JSON object. Emit one block per call. Available tools:\n\n<tools>\n",
    );
    for tool in tools {
        out.push_str("<tool>\n<name>");
        out.push_str(&tool.name);
        out.push_str("</name>\n<description>");
        out.push_str(&tool.description);
        out.push_str("</description>\n<schema>");
        out.push_str(&tool.schema.to_string());
        out.push_str("</schema>\n</tool>\n");
    }
    out.push_str("</tools>");
    out
}

fn inner_tag<'a>(block: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(&close)? + start;
    Some(block[start..end].trim())
}

#[derive(Default)]
pub struct BlockParser {
    buffer: String,
    calls: usize,
}

impl BlockParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &str) -> Vec<Event> {
        self.buffer.push_str(chunk);
        let mut events = Vec::new();

        while let Some(start) = self.buffer.find(OPEN) {
            let Some(offset) = self.buffer[start..].find(CLOSE) else {
                break;
            };
            let end = start + offset + CLOSE.len();

            let before: String = self.buffer[..start].to_string();
            if !before.is_empty() {
                events.push(Event::TextDelta(before));
            }

            let block = self.buffer[start..end].to_string();
            events.extend(self.emit_call(&block));
            self.buffer = self.buffer[end..].to_string();
        }

        let safe = self.safe_prefix();
        if safe > 0 {
            let text: String = self.buffer.drain(..safe).collect();
            events.push(Event::TextDelta(text));
        }

        events
    }

    pub fn finish(&mut self) -> Vec<Event> {
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let rest = std::mem::take(&mut self.buffer);
        vec![Event::TextDelta(rest)]
    }

    fn emit_call(&mut self, block: &str) -> Vec<Event> {
        self.calls += 1;
        let name = inner_tag(block, "name").unwrap_or_default().to_string();
        let args = inner_tag(block, "args").unwrap_or("{}").to_string();
        vec![
            Event::ToolCallStart {
                id: format!("prompted_{}", self.calls),
                name,
            },
            Event::ToolCallDelta(args),
            Event::ToolCallEnd,
        ]
    }

    fn safe_prefix(&self) -> usize {
        if let Some(start) = self.buffer.find(OPEN) {
            return start;
        }
        let len = self.buffer.len();
        let keep = OPEN.len().min(len);
        for back in (1..=keep).rev() {
            let tail = &self.buffer[len - back..];
            if OPEN.starts_with(tail) && self.buffer.is_char_boundary(len - back) {
                return len - back;
            }
        }
        len
    }
}

pub struct PromptedProvider {
    inner: Arc<dyn Provider>,
}

impl PromptedProvider {
    pub fn new(inner: Arc<dyn Provider>) -> Self {
        PromptedProvider { inner }
    }

    pub fn wrap(inner: Arc<dyn Provider>) -> Arc<dyn Provider> {
        if inner.caps().tools == ToolSupport::Prompted {
            Arc::new(PromptedProvider::new(inner))
        } else {
            inner
        }
    }
}

fn rewrite(req: Request) -> Request {
    let Request {
        mut messages,
        tools,
        model,
        max_tokens,
        temperature,
    } = req;

    if !tools.is_empty() {
        let injected = system_prompt(&tools);
        match messages.first_mut() {
            Some(Message::System(existing)) => {
                existing.push_str("\n\n");
                existing.push_str(&injected);
            }
            _ => messages.insert(0, Message::System(injected)),
        }
    }

    Request {
        messages,
        tools: Vec::new(),
        model,
        max_tokens,
        temperature,
    }
}

#[async_trait]
impl Provider for PromptedProvider {
    async fn stream(&self, req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let upstream = self.inner.stream(rewrite(req)).await?;

        let stream = futures::stream::unfold(
            (upstream, BlockParser::new(), Vec::new().into_iter(), false),
            |(mut upstream, mut parser, mut queued, mut done)| async move {
                loop {
                    if let Some(event) = queued.next() {
                        return Some((event, (upstream, parser, queued, done)));
                    }
                    if done {
                        return None;
                    }
                    match upstream.next().await {
                        Some(Event::TextDelta(text)) => {
                            queued = parser.push(&text).into_iter();
                        }
                        Some(event) => {
                            let mut batch = Vec::new();
                            if matches!(event, Event::Done(_)) {
                                batch.extend(parser.finish());
                            }
                            batch.push(event);
                            queued = batch.into_iter();
                        }
                        None => {
                            done = true;
                            queued = parser.finish().into_iter();
                        }
                    }
                }
            },
        );

        Ok(stream.boxed())
    }

    fn caps(&self) -> Caps {
        Caps {
            tools: ToolSupport::Native,
            ..self.inner.caps()
        }
    }

    fn count_tokens(&self, msgs: &[Message]) -> Result<u32, ProviderError> {
        self.inner.count_tokens(msgs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&str]) -> Vec<Event> {
        let mut parser = BlockParser::new();
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(parser.push(chunk));
        }
        events.extend(parser.finish());
        events
    }

    fn text_of(events: &[Event]) -> String {
        events
            .iter()
            .filter_map(|e| match e {
                Event::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    fn args_of(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::ToolCallDelta(a) => Some(a.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_whole_block_becomes_one_call() {
        let events = run(&[
            "let me look.\n<tool_use>\n<name>read</name>\n<args>{\"path\":\"a.rs\"}</args>\n</tool_use>",
        ]);
        assert_eq!(text_of(&events), "let me look.\n");
        assert_eq!(args_of(&events), vec!["{\"path\":\"a.rs\"}"]);
        assert_eq!(
            events.iter().filter(|e| **e == Event::ToolCallEnd).count(),
            1
        );
    }

    #[test]
    fn a_block_split_across_chunks_is_reassembled() {
        let events = run(&[
            "<tool_u",
            "se>\n<name>read</name>\n<args>{\"path\"",
            ":\"a.rs\"}</args>\n</tool_use>",
        ]);
        assert_eq!(text_of(&events), "");
        assert_eq!(args_of(&events), vec!["{\"path\":\"a.rs\"}"]);
    }

    #[test]
    fn two_calls_in_one_response_both_come_out() {
        let events = run(&[
            "<tool_use><name>one</name><args>{}</args></tool_use>between<tool_use><name>two</name><args>{\"a\":1}</args></tool_use>",
        ]);
        let names: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                Event::ToolCallStart { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["one", "two"]);
        assert_eq!(text_of(&events), "between");
    }

    #[test]
    fn an_unclosed_block_stays_text_rather_than_vanishing() {
        let events = run(&["here goes <tool_use>\n<name>read</name>\n<args>{}"]);
        assert!(events.iter().all(|e| matches!(e, Event::TextDelta(_))));
        assert_eq!(text_of(&events), "here goes <tool_use>\n<name>read</name>\n<args>{}");
    }

    #[test]
    fn invalid_json_args_are_passed_through_verbatim() {
        let events = run(&["<tool_use><name>read</name><args>{not json}</args></tool_use>"]);
        assert_eq!(args_of(&events), vec!["{not json}"]);
    }

    #[test]
    fn a_block_with_no_args_tag_defaults_to_an_empty_object() {
        let events = run(&["<tool_use><name>list</name></tool_use>"]);
        assert_eq!(args_of(&events), vec!["{}"]);
    }

    #[test]
    fn plain_text_is_not_held_back() {
        let events = run(&["hello ", "world"]);
        assert_eq!(text_of(&events), "hello world");
    }
}
