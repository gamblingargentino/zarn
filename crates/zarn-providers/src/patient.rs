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
}
