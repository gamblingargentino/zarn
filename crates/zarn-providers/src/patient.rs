use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zarn_core::provider::{Caps, Event, Provider, ProviderError};
use zarn_core::types::{Message, Request};

pub const MAX_WAIT: Duration = Duration::from_secs(65);
const ATTEMPTS: usize = 3;
const BUSY_WAITS: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(5)];

pub fn busy(message: &str) -> bool {
    ["(HTTP 502)", "(HTTP 503)", "(HTTP 529)"].iter().any(|code| message.ends_with(code))
}

pub fn glitch(event: &Event) -> bool {
    matches!(event, Event::Error(ProviderError::Other(m))
        if m == crate::google::MALFORMED_CALL || m == crate::google::UNOFFERED_CALL)
}

pub type WaitSink = Arc<dyn Fn(String) + Send + Sync>;

static SINK: Mutex<Option<WaitSink>> = Mutex::new(None);

pub fn on_wait(sink: Option<WaitSink>) {
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = sink;
}

fn tell(text: String) {
    let sink = SINK.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match sink {
        Some(sink) => sink(text),
        None => tracing::info!("{text}"),
    }
}

pub struct Patient {
    inner: Arc<dyn Provider>,
    max_wait: Duration,
}

impl Patient {
    pub fn wrap(inner: Arc<dyn Provider>) -> Arc<dyn Provider> {
        Self::with_limit(inner, MAX_WAIT)
    }

    pub fn with_limit(inner: Arc<dyn Provider>, max_wait: Duration) -> Arc<dyn Provider> {
        Arc::new(Patient { inner, max_wait })
    }
}

pub fn retry_after(message: &str) -> Option<Duration> {
    let lower = message.to_ascii_lowercase();
    let at = ["retry in", "try again in", "retry after", "retrydelay\":"]
        .iter()
        .find_map(|cue| lower.find(cue).map(|i| i + cue.len()))?;
    let rest = lower[at..].trim_start().trim_start_matches('"');
    let mut total = 0.0;
    let mut found = false;
    let mut rest = rest;
    loop {
        let number: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        let Ok(value) = number.parse::<f64>() else {
            break;
        };
        let after = &rest[number.len()..];
        let (scale, unit_len) = if after.starts_with("ms") {
            (0.001, 2)
        } else if after.starts_with('h') {
            (3600.0, 1)
        } else if after.starts_with('m') {
            (60.0, 1)
        } else if after.starts_with('s') {
            (1.0, 1)
        } else if after.starts_with(" second") {
            (1.0, 0)
        } else {
            break;
        };
        total += value * scale;
        found = true;
        rest = &after[unit_len..];
        if unit_len == 0 {
            break;
        }
    }
    found.then(|| Duration::from_secs_f64(total))
}

const DROPPED_WAITS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(3)];

pub fn dropped(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    !busy(message)
        && !lower.contains("(http ")
        && ["error sending request", "connection", "timed out", "timeout", "reset", "broken pipe", "eof", "dns", "tcp connect", "unexpected end"]
            .iter()
            .any(|cue| lower.contains(cue))
}

pub fn output_cap(message: &str, asked: u32) -> Option<u32> {
    let lower = message.to_ascii_lowercase();
    let about_output = ["max_tokens", "max_completion_tokens", "maxoutputtokens", "max_output_tokens", "output tokens", "completion tokens"]
        .iter()
        .any(|cue| lower.contains(cue));
    let too_big = ["too large", "too many", "must be", "less than", "at most", "exceed", "maximum", "<=", " > ", "range"]
        .iter()
        .any(|cue| lower.contains(cue));
    if !about_output || !too_big {
        return None;
    }
    let numbers = lower
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|n| n.parse::<u32>().ok())
        .filter(|n| (256..asked).contains(n));
    Some(numbers.max().unwrap_or(4096).min(asked.saturating_sub(1)).max(256))
}

pub fn out_of_credit(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.ends_with("(http 402)") || lower.contains("insufficient credits") || lower.contains("exceed your available credits")
}

pub fn no_quota(message: &str) -> bool {
    message.contains("limit: 0")
}

fn human(wait: Duration) -> String {
    let secs = wait.as_secs();
    match (secs / 3600, secs % 3600 / 60) {
        (0, 0) => format!("{secs}s"),
        (0, m) => format!("{m} min"),
        (h, m) => format!("{h} h {m} min"),
    }
}

fn explain(message: String, wait: Option<Duration>, max_wait: Duration) -> ProviderError {
    let detail = message
        .lines()
        .find(|l| l.contains("Quota exceeded for metric"))
        .map(|l| l.trim().trim_start_matches('*').trim().to_string())
        .unwrap_or_else(|| message.clone());
    if no_quota(&message) {
        return ProviderError::RateLimit(format!(
            "this key has no quota for this model (its plan allows 0 requests); pick another model with /model. {detail}"
        ));
    }
    match wait {
        Some(wait) if wait > max_wait => ProviderError::RateLimit(format!(
            "this key's quota for this model is used up and resets in about {}; pick another model with /model. {detail}",
            human(wait)
        )),
        _ => ProviderError::RateLimit(message),
    }
}

#[async_trait]
impl Provider for Patient {
    async fn stream(&self, req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let mut attempt = 1;
        let mut spent = Vec::new();
        let mut req = req;
        let mut capped = false;
        let mut credit_retried = false;
        loop {
            match self.inner.stream(req.clone()).await {
                Ok(mut stream) => {
                    let first = loop {
                        match stream.next().await {
                            Some(usage @ Event::Usage { .. }) => spent.push(usage),
                            other => break other,
                        }
                    };
                    match first {
                        Some(event) if glitch(&event) && attempt < ATTEMPTS => {
                            tell("the model sent a broken tool call; asking again".into());
                            attempt += 1;
                        }
                        Some(Event::Error(ProviderError::Network(message))) if dropped(&message) && attempt < ATTEMPTS => {
                            let wait = DROPPED_WAITS[attempt - 1];
                            tell(format!("the connection to the provider dropped; trying again in {}s", wait.as_secs()));
                            tokio::time::sleep(wait.min(self.max_wait)).await;
                            attempt += 1;
                        }
                        first => {
                            let head = futures::stream::iter(spent.into_iter().chain(first));
                            return Ok(head.chain(stream).boxed());
                        }
                    }
                }
                Err(ProviderError::RateLimit(message)) => {
                    let asked = retry_after(&message);
                    let wait = asked.filter(|w| *w <= self.max_wait && !no_quota(&message));
                    match wait {
                        Some(wait) if attempt < ATTEMPTS => {
                            tell(format!(
                                "rate limited by the provider; trying again in {}s",
                                wait.as_secs_f64().ceil() as u64
                            ));
                            tokio::time::sleep(wait + Duration::from_millis(250)).await;
                            attempt += 1;
                        }
                        _ => return Err(explain(message, asked, self.max_wait)),
                    }
                }
                Err(ProviderError::BadRequest(message)) if !capped && output_cap(&message, req.max_tokens).is_some() => {
                    let cap = output_cap(&message, req.max_tokens).unwrap_or(4096);
                    tracing::info!(from = req.max_tokens, to = cap, "the model allows fewer output tokens; asking again");
                    req.max_tokens = cap;
                    capped = true;
                }
                Err(ProviderError::Other(message)) if out_of_credit(&message) && !credit_retried && req.max_tokens > 1024 => {
                    credit_retried = true;
                    req.max_tokens = (req.max_tokens / 2).max(1024);
                    tell(format!(
                        "the provider says this request would exceed your credits; asking again with room for {} output tokens",
                        req.max_tokens
                    ));
                    tokio::time::sleep(Duration::from_secs(2).min(self.max_wait)).await;
                    attempt += 1;
                }
                Err(ProviderError::Network(message)) if dropped(&message) && attempt < ATTEMPTS => {
                    let wait = DROPPED_WAITS[attempt - 1];
                    tell(format!("the connection to the provider dropped; trying again in {}s", wait.as_secs()));
                    tokio::time::sleep(wait.min(self.max_wait)).await;
                    attempt += 1;
                }
                Err(ProviderError::Network(message)) if busy(&message) && attempt < ATTEMPTS => {
                    let wait = BUSY_WAITS[attempt - 1];
                    tell(format!("the provider is busy; trying again in {}s", wait.as_secs()));
                    tokio::time::sleep(wait.min(self.max_wait)).await;
                    attempt += 1;
                }
                Err(other) => return Err(other),
            }
        }
    }

    fn caps(&self) -> Caps {
        self.inner.caps()
    }

    fn count_tokens(&self, msgs: &[Message]) -> Result<u32, ProviderError> {
        self.inner.count_tokens(msgs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_wait_from_each_providers_wording() {
        let near = |text: &str, secs: f64| {
            let got = retry_after(text).unwrap().as_secs_f64();
            assert!((got - secs).abs() < 0.01, "{text}: {got}");
        };
        near("Quota exceeded. Please retry in 26.564951388s.", 26.565);
        near("Rate limit reached. Please try again in 7.5s.", 7.5);
        near("Please try again in 1m2.5s", 62.5);
        near("try again in 450ms", 0.45);
        near("Please retry after 20 seconds", 20.0);
        near("Please retry in 16h36m26.03s.", 59786.03);
        assert!(retry_after("slow down").is_none());
    }

    struct Scripted {
        errors: Mutex<Vec<ProviderError>>,
        calls: Mutex<usize>,
    }

    #[async_trait]
    impl Provider for Scripted {
        async fn stream(&self, _req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
            *self.calls.lock().unwrap() += 1;
            match self.errors.lock().unwrap().pop() {
                Some(err) => Err(err),
                None => Ok(futures::stream::iter(vec![Event::TextDelta("ok".into())]).boxed()),
            }
        }

        fn caps(&self) -> Caps {
            Caps::default()
        }

        fn count_tokens(&self, _msgs: &[Message]) -> Result<u32, ProviderError> {
            Ok(0)
        }
    }

    fn scripted(errors: Vec<&str>) -> Arc<Scripted> {
        Arc::new(Scripted {
            errors: Mutex::new(errors.into_iter().map(|e| ProviderError::RateLimit(e.into())).collect()),
            calls: Mutex::new(0),
        })
    }

    #[tokio::test]
    async fn waits_out_a_short_rate_limit_and_tries_again() {
        let inner = scripted(vec!["Please retry in 0.01s."]);
        let provider = Patient::with_limit(inner.clone(), Duration::from_secs(5));
        let mut stream = provider.stream(Request::new("m", vec![])).await.unwrap();
        assert_eq!(stream.next().await, Some(Event::TextDelta("ok".into())));
        assert_eq!(*inner.calls.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn gives_up_on_long_waits_and_explains_a_zero_quota() {
        let inner = scripted(vec![
            "You exceeded your current quota.\n* Quota exceeded for metric: free_tier_requests, limit: 20, model: m\nPlease retry in 16h31m1.04s.",
        ]);
        let provider = Patient::with_limit(inner.clone(), Duration::from_secs(5));
        let Err(err) = provider.stream(Request::new("m", vec![])).await else {
            panic!("a long wait is not retried");
        };
        assert_eq!(
            err.to_string(),
            "rate limited: this key's quota for this model is used up and resets in about 16 h 31 min; pick another model with /model. Quota exceeded for metric: free_tier_requests, limit: 20, model: m"
        );
        assert_eq!(*inner.calls.lock().unwrap(), 1);

        let inner = scripted(vec!["Quota exceeded, limit: 0, model: gemini-3.1-pro. Please retry in 0.01s."]);
        let provider = Patient::with_limit(inner.clone(), Duration::from_secs(5));
        let Err(err) = provider.stream(Request::new("m", vec![])).await else {
            panic!("a zero quota is not retried");
        };
        assert!(err.to_string().contains("no quota for this model"), "{err}");
        assert_eq!(*inner.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn a_busy_server_is_tried_again() {
        let inner = Arc::new(Scripted {
            errors: Mutex::new(vec![ProviderError::Network(
                "This model is currently experiencing high demand. (HTTP 503)".into(),
            )]),
            calls: Mutex::new(0),
        });
        let provider = Patient::with_limit(inner.clone(), Duration::from_millis(10));
        assert!(provider.stream(Request::new("m", vec![])).await.is_ok());
        assert_eq!(*inner.calls.lock().unwrap(), 2);
        assert!(!busy("connection refused"));
    }

    #[tokio::test]
    async fn stops_after_three_attempts() {
        let inner = scripted(vec!["retry in 0.01s", "retry in 0.01s", "retry in 0.01s"]);
        let provider = Patient::with_limit(inner.clone(), Duration::from_secs(5));
        assert!(provider.stream(Request::new("m", vec![])).await.is_err());
        assert_eq!(*inner.calls.lock().unwrap(), 3);
    }

    struct Glitchy {
        broken: Mutex<usize>,
        calls: Mutex<usize>,
        speak_first: bool,
    }

    #[async_trait]
    impl Provider for Glitchy {
        async fn stream(&self, _req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
            *self.calls.lock().unwrap() += 1;
            let mut broken = self.broken.lock().unwrap();
            let mut events = vec![Event::Usage { input: 10, output: 1, cached: 0 }];
            if *broken > 0 {
                *broken -= 1;
                if self.speak_first {
                    events.push(Event::TextDelta("partial".into()));
                }
                events.push(Event::Error(ProviderError::Other(crate::google::MALFORMED_CALL.into())));
            } else {
                events.push(Event::TextDelta("ok".into()));
                events.push(Event::Done(zarn_core::types::StopReason::EndTurn));
            }
            Ok(futures::stream::iter(events).boxed())
        }

        fn caps(&self) -> Caps {
            Caps::default()
        }

        fn count_tokens(&self, _msgs: &[Message]) -> Result<u32, ProviderError> {
            Ok(0)
        }
    }

    fn glitchy(broken: usize, speak_first: bool) -> Arc<Glitchy> {
        Arc::new(Glitchy { broken: Mutex::new(broken), calls: Mutex::new(0), speak_first })
    }

    #[tokio::test]
    async fn a_broken_tool_call_is_asked_again_and_its_usage_kept() {
        let inner = glitchy(2, false);
        let provider = Patient::wrap(inner.clone());
        let events: Vec<Event> = provider.stream(Request::new("m", vec![])).await.unwrap().collect().await;
        assert_eq!(*inner.calls.lock().unwrap(), 3);
        assert_eq!(events.iter().filter(|e| matches!(e, Event::Usage { .. })).count(), 3);
        assert!(events.contains(&Event::TextDelta("ok".into())));
        assert!(!events.iter().any(glitch));
    }

    #[tokio::test]
    async fn a_broken_tool_call_that_keeps_coming_is_reported() {
        let inner = glitchy(5, false);
        let provider = Patient::wrap(inner.clone());
        let events: Vec<Event> = provider.stream(Request::new("m", vec![])).await.unwrap().collect().await;
        assert_eq!(*inner.calls.lock().unwrap(), 3);
        assert!(events.last().is_some_and(glitch));
    }

    #[tokio::test]
    async fn a_broken_tool_call_after_text_is_not_repeated() {
        let inner = glitchy(1, true);
        let provider = Patient::wrap(inner.clone());
        let events: Vec<Event> = provider.stream(Request::new("m", vec![])).await.unwrap().collect().await;
        assert_eq!(*inner.calls.lock().unwrap(), 1);
        assert_eq!(events[1], Event::TextDelta("partial".into()));
        assert!(events.last().is_some_and(glitch));
    }

    #[test]
    fn reads_the_allowed_output_size_from_each_providers_wording() {
        let cap = |m: &str| output_cap(m, 16_384);
        assert_eq!(cap("max_tokens: 16384 > 8192, which is the maximum allowed number of output tokens for claude-3-5-sonnet-20240620 (HTTP 400)"), Some(8192));
        assert_eq!(cap("`max_tokens` must be less than or equal to `8192`, the maximum value for `max_tokens` (HTTP 400)"), Some(8192));
        assert_eq!(cap("Invalid max_tokens value, the valid range of max_tokens is [1, 8192] (HTTP 400)"), Some(8192));
        assert_eq!(cap("max_completion_tokens is too large: 16384. This model supports at most 4096 completion tokens"), Some(4096));
        assert_eq!(cap("max_tokens is too large"), Some(4096));
        assert_eq!(cap("This model's maximum context length is 128000 tokens (HTTP 400)"), None);
        assert_eq!(cap("Invalid tool schema (HTTP 400)"), None);
        assert!(dropped("error sending request for url (https://openrouter.ai/api/v1/chat/completions)"));
        assert!(dropped("connection closed before message completed"));
        assert!(!dropped("Service Unavailable (HTTP 503)"));
        assert!(!dropped("Internal error (HTTP 500)"));
    }

    struct Recording {
        asked: Mutex<Vec<u32>>,
        script: Mutex<Vec<Result<Vec<Event>, ProviderError>>>,
    }

    #[async_trait]
    impl Provider for Recording {
        async fn stream(&self, req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
            self.asked.lock().unwrap().push(req.max_tokens);
            match self.script.lock().unwrap().remove(0) {
                Ok(events) => Ok(futures::stream::iter(events).boxed()),
                Err(err) => Err(err),
            }
        }

        fn caps(&self) -> Caps {
            Caps::default()
        }

        fn count_tokens(&self, _msgs: &[Message]) -> Result<u32, ProviderError> {
            Ok(0)
        }
    }

    fn recording(script: Vec<Result<Vec<Event>, ProviderError>>) -> Arc<Recording> {
        Arc::new(Recording { asked: Mutex::new(Vec::new()), script: Mutex::new(script) })
    }

    #[tokio::test]
    async fn asks_again_with_the_output_size_the_model_allows() {
        let ok = vec![Event::TextDelta("ok".into())];
        let inner = recording(vec![
            Err(ProviderError::BadRequest("max_tokens: 16384 > 8192, the maximum (HTTP 400)".into())),
            Ok(ok.clone()),
        ]);
        let provider = Patient::with_limit(inner.clone(), Duration::from_millis(10));
        let mut req = Request::new("m", vec![]);
        req.max_tokens = 16_384;
        let events: Vec<Event> = provider.stream(req).await.unwrap().collect().await;
        assert_eq!(events, ok);
        assert_eq!(*inner.asked.lock().unwrap(), vec![16_384, 8192]);
    }

    #[tokio::test]
    async fn out_of_credit_asks_again_with_less_room_then_gives_up() {
        let credit = || Err(ProviderError::Other("This request would exceed your available credits given your current in-flight requests. Retry after in-flight requests settle, or add credits. (HTTP 402)".into()));
        let inner = recording(vec![credit(), Ok(vec![Event::TextDelta("ok".into())])]);
        let provider = Patient::with_limit(inner.clone(), Duration::from_millis(10));
        let mut req = Request::new("m", vec![]);
        req.max_tokens = 16_384;
        assert!(provider.stream(req.clone()).await.is_ok());
        assert_eq!(*inner.asked.lock().unwrap(), vec![16_384, 8192]);

        let inner = recording(vec![credit(), credit()]);
        let provider = Patient::with_limit(inner.clone(), Duration::from_millis(10));
        let Err(err) = provider.stream(req).await else { panic!("a second refusal ends in an error") };
        assert!(out_of_credit(&err.to_string()));
        assert_eq!(*inner.asked.lock().unwrap(), vec![16_384, 8192], "one smaller retry, then the chain can move on");
    }

    #[tokio::test]
    async fn a_dropped_connection_is_tried_again_before_or_at_the_first_event() {
        let ok = vec![Event::TextDelta("ok".into())];
        let inner = recording(vec![
            Err(ProviderError::Network("error sending request for url (https://x)".into())),
            Ok(vec![Event::Error(ProviderError::Network("connection reset by peer".into()))]),
            Ok(ok.clone()),
        ]);
        let provider = Patient::with_limit(inner.clone(), Duration::from_millis(10));
        let events: Vec<Event> = provider.stream(Request::new("m", vec![])).await.unwrap().collect().await;
        assert_eq!(events, ok);
        assert_eq!(inner.asked.lock().unwrap().len(), 3);
    }
}
