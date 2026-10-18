use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zarn_core::provider::{Caps, Event, Provider, ProviderError};
use zarn_core::types::{Message, Request};

pub const STRIKES: u32 = 2;
pub const REST: Duration = Duration::from_secs(5 * 60);
pub const LONG_REST: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Switch {
    pub failed: Vec<(String, String)>,
    pub answered: String,
    pub resting: Vec<(String, u64)>,
}

impl Switch {
    pub fn notice(&self) -> String {
        let failed = self
            .failed
            .iter()
            .map(|(name, why)| format!("{name} {why}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut text = if failed.is_empty() {
            format!("back on {}", self.answered)
        } else {
            format!("{failed}, answered by {}", self.answered)
        };
        for (name, minutes) in &self.resting {
            text.push_str(&format!("; {name} sits out for {minutes} min"));
        }
        text
    }
}

pub fn reason(err: &ProviderError) -> &'static str {
    let text = err.to_string().to_ascii_lowercase();
    if crate::patient::out_of_credit(&text) {
        return "is out of credit";
    }
    if too_long(&text) {
        return "cannot fit this conversation";
    }
    if missing_model(&text) {
        return "does not offer this model";
    }
    match err {
        ProviderError::RateLimit(_) => "rate-limited",
        ProviderError::Auth(_) => "rejected the key",
        ProviderError::Network(_) => "unreachable",
        ProviderError::BadRequest(_) => "refused the request",
        ProviderError::Other(_) => "failed",
    }
}

pub fn too_long(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("(http 413)")
        || [
            "context length",
            "context window",
            "context_length",
            "maximum context",
            "prompt is too long",
            "input is too long",
            "too many tokens",
            "too many input tokens",
            "reduce the length",
            "request too large",
            "exceeds the context",
            "input token count",
        ]
        .iter()
        .any(|cue| lower.contains(cue))
}

pub fn missing_model(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("(http 404)")
        || [
            "model not found",
            "model_not_found",
            "does not exist",
            "no endpoints found",
            "is not a valid model",
            "unknown model",
            "decommissioned",
            "no longer available",
            "not supported for generatecontent",
        ]
        .iter()
        .any(|cue| lower.contains(cue))
}

pub fn switchable(err: &ProviderError) -> bool {
    if err.is_transient() {
        return true;
    }
    let text = err.to_string();
    crate::patient::out_of_credit(&text) || too_long(&text) || missing_model(&text)
}

fn lasting(err: &ProviderError) -> bool {
    let text = err.to_string();
    matches!(err, ProviderError::Auth(_))
        || crate::patient::out_of_credit(&text)
        || crate::patient::no_quota(&text)
        || missing_model(&text)
}

#[derive(Clone, Debug, Default)]
struct Health {
    strikes: u32,
    until: Option<Instant>,
}

pub type SwitchSink = Arc<dyn Fn(&Switch) + Send + Sync>;

pub type Rests = Arc<Mutex<std::collections::HashMap<String, Instant>>>;

pub struct FallbackProvider {
    providers: Vec<Arc<dyn Provider>>,
    names: Vec<String>,
    sink: Option<SwitchSink>,
    last: Mutex<Option<usize>>,
    health: Mutex<Vec<Health>>,
    rest: Duration,
    shared: Option<Rests>,
}

impl FallbackProvider {
    pub fn new(providers: Vec<Arc<dyn Provider>>) -> Self {
        let names = (0..providers.len()).map(|i| format!("provider {}", i + 1)).collect();
        Self::build(providers, names)
    }

    pub fn named(entries: Vec<(String, Arc<dyn Provider>)>) -> Self {
        let (names, providers) = entries.into_iter().unzip();
        Self::build(providers, names)
    }

    fn build(providers: Vec<Arc<dyn Provider>>, names: Vec<String>) -> Self {
        let health = vec![Health::default(); providers.len()];
        FallbackProvider {
            providers,
            names,
            sink: None,
            last: Mutex::new(None),
            health: Mutex::new(health),
            rest: REST,
            shared: None,
        }
    }

    pub fn with_rests(mut self, shared: Rests) -> Self {
        let now = Instant::now();
        if let (Ok(map), Ok(mut health)) = (shared.lock(), self.health.lock()) {
            for (i, name) in self.names.iter().enumerate() {
                if let Some(until) = map.get(name).filter(|t| **t > now) {
                    health[i].until = Some(*until);
                }
            }
        }
        self.shared = Some(shared);
        self
    }

    pub fn active(&self) -> Option<String> {
        self.ready().first().filter(|i| **i != 0).map(|i| self.names[*i].clone())
    }

    pub fn with_sink(mut self, sink: SwitchSink) -> Self {
        self.sink = Some(sink);
        self
    }

    pub fn with_rest(mut self, rest: Duration) -> Self {
        self.rest = rest;
        self
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn resting(&self) -> Vec<(String, Duration)> {
        let now = Instant::now();
        let health = self.health.lock().map(|h| h.clone()).unwrap_or_default();
        health
            .iter()
            .enumerate()
            .filter_map(|(i, h)| {
                h.until
                    .filter(|t| *t > now)
                    .map(|t| (self.names[i].clone(), t - now))
            })
            .collect()
    }

    fn answered(&self, index: usize, failed: Vec<(String, String)>, resting: Vec<(String, u64)>) {
        let Ok(mut last) = self.last.lock() else {
            return;
        };
        let previous = last.replace(index);
        let moved = match previous {
            None => index != 0,
            Some(p) => p != index,
        };
        drop(last);
        if !moved && resting.is_empty() {
            return;
        }
        if let Some(sink) = &self.sink {
            sink(&Switch {
                failed,
                answered: self.names[index].clone(),
                resting,
            });
        }
    }

    fn ready(&self) -> Vec<usize> {
        let now = Instant::now();
        let health = self.health.lock().map(|h| h.clone()).unwrap_or_default();
        let ready: Vec<usize> = (0..self.providers.len())
            .filter(|i| health.get(*i).is_none_or(|h| h.until.is_none_or(|t| t <= now)))
            .collect();
        if ready.is_empty() { (0..self.providers.len()).collect() } else { ready }
    }

    fn strike(&self, index: usize, err: &ProviderError) -> Option<(String, u64)> {
        let mut health = self.health.lock().ok()?;
        let entry = health.get_mut(index)?;
        entry.strikes += 1;
        let rest = if lasting(err) {
            Some(LONG_REST.max(self.rest))
        } else if entry.strikes >= STRIKES {
            Some(self.rest)
        } else {
            None
        }?;
        if self.providers.len() < 2 {
            return None;
        }
        let until = Instant::now() + rest;
        entry.until = Some(until);
        entry.strikes = 0;
        if let Some(shared) = &self.shared
            && let Ok(mut map) = shared.lock()
        {
            map.insert(self.names[index].clone(), until);
        }
        Some((self.names[index].clone(), rest.as_secs().div_ceil(60)))
    }

    fn recovered(&self, index: usize) {
        if let Ok(mut health) = self.health.lock()
            && let Some(entry) = health.get_mut(index)
        {
            *entry = Health::default();
        }
        if let Some(shared) = &self.shared
            && let Some(name) = self.names.get(index)
            && let Ok(mut map) = shared.lock()
        {
            map.remove(name);
        }
    }

    fn current(&self) -> Option<&Arc<dyn Provider>> {
        self.ready().first().and_then(|i| self.providers.get(*i))
    }

    fn fitting(&self, order: Vec<usize>, messages: &[Message]) -> (Vec<usize>, Vec<usize>) {
        let fits = |i: &usize| {
            let window = self.providers[*i].caps().context_window;
            window == 0 || self.providers[*i].count_tokens(messages).unwrap_or(0) < window
        };
        if order.iter().all(fits) || !order.iter().any(fits) {
            return (order, Vec::new());
        }
        order.into_iter().partition(fits)
    }
}

#[async_trait]
impl Provider for FallbackProvider {
    async fn stream(&self, req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let (order, small) = self.fitting(self.ready(), &req.messages);
        let mut seen: Option<ProviderError> = None;
        let mut failed: Vec<(String, String)> = small
            .iter()
            .map(|i| (self.names[*i].clone(), "is too small for this conversation".to_string()))
            .collect();
        let mut resting = Vec::new();
        let last = order.len().saturating_sub(1);

        for (position, index) in order.iter().copied().enumerate() {
            let is_last = position == last;
            let provider = &self.providers[index];

            let outcome = match provider.stream(req.clone()).await {
                Err(err) => Err(err),
                Ok(stream) => {
                    let (first, rest) = stream.into_future().await;
                    match first {
                        Some(Event::Error(err)) if switchable(&err) => Err(err),
                        first => Ok((first, rest)),
                    }
                }
            };

            match outcome {
                Ok((first, rest)) => {
                    if !matches!(first, Some(Event::Error(_))) {
                        self.recovered(index);
                        self.answered(index, failed, resting);
                    }
                    return Ok(match first {
                        None => rest,
                        Some(event) => futures::stream::once(async move { event }).chain(rest).boxed(),
                    });
                }
                Err(err) if switchable(&err) => {
                    tracing::debug!(index, error = %err, "model failed, trying the next one");
                    resting.extend(self.strike(index, &err));
                    failed.push((self.names[index].clone(), reason(&err).to_string()));
                    if is_last {
                        return Err(err);
                    }
                    seen = Some(err);
                }
                Err(err) => return Err(err),
            }
        }

        Err(seen.unwrap_or_else(|| ProviderError::Other("no providers configured".into())))
    }

    fn caps(&self) -> Caps {
        self.current().map(|p| p.caps()).unwrap_or_default()
    }

    fn count_tokens(&self, msgs: &[Message]) -> Result<u32, ProviderError> {
        match self.current() {
            Some(p) => p.count_tokens(msgs),
            None => Err(ProviderError::Other("no providers configured".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zarn_core::types::StopReason;

    struct Fixed {
        open: Option<ProviderError>,
        first_event: Option<Event>,
    }

    impl Fixed {
        fn failing(err: ProviderError) -> Arc<dyn Provider> {
            Arc::new(Fixed {
                open: Some(err),
                first_event: None,
            })
        }

        fn working(text: &str) -> Arc<dyn Provider> {
            Arc::new(Fixed {
                open: None,
                first_event: Some(Event::TextDelta(text.to_string())),
            })
        }

        fn erroring(err: ProviderError) -> Arc<dyn Provider> {
            Arc::new(Fixed {
                open: None,
                first_event: Some(Event::Error(err)),
            })
        }
    }

    #[async_trait]
    impl Provider for Fixed {
        async fn stream(&self, _req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
            if let Some(err) = &self.open {
                return Err(err.clone());
            }
            let first = self.first_event.clone();
            Ok(futures::stream::iter(
                first
                    .into_iter()
                    .chain(std::iter::once(Event::Done(StopReason::EndTurn))),
            )
            .boxed())
        }

        fn caps(&self) -> Caps {
            Caps::default()
        }

        fn count_tokens(&self, _msgs: &[Message]) -> Result<u32, ProviderError> {
            Ok(0)
        }
    }

    fn req() -> Request {
        Request::new("m", vec![Message::user("hi")])
    }

    #[tokio::test]
    async fn advances_past_a_rate_limited_provider() {
        let chain = FallbackProvider::new(vec![
            Fixed::failing(ProviderError::RateLimit("slow down".into())),
            Fixed::working("second"),
        ]);
        let events: Vec<Event> = chain.stream(req()).await.unwrap().collect().await;
        assert_eq!(events[0], Event::TextDelta("second".into()));
    }

    #[tokio::test]
    async fn does_not_advance_on_a_bad_request() {
        let chain = FallbackProvider::new(vec![
            Fixed::failing(ProviderError::BadRequest("bad model".into())),
            Fixed::working("second"),
        ]);
        let err = chain.stream(req()).await.err().unwrap();
        assert!(matches!(err, ProviderError::BadRequest(_)));
    }

    #[tokio::test]
    async fn advances_when_the_first_event_is_a_transient_error() {
        let chain = FallbackProvider::new(vec![
            Fixed::erroring(ProviderError::Network("reset".into())),
            Fixed::working("second"),
        ]);
        let events: Vec<Event> = chain.stream(req()).await.unwrap().collect().await;
        assert_eq!(events[0], Event::TextDelta("second".into()));
    }

    #[tokio::test]
    async fn the_last_provider_surfaces_its_own_error() {
        let chain = FallbackProvider::new(vec![Fixed::failing(ProviderError::Network(
            "down".into(),
        ))]);
        let err = chain.stream(req()).await.err().unwrap();
        assert!(matches!(err, ProviderError::Network(_)));
    }

    fn recorder() -> (SwitchSink, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = seen.clone();
        let sink: SwitchSink = Arc::new(move |switch: &Switch| {
            sink_seen.lock().unwrap().push(switch.notice());
        });
        (sink, seen)
    }

    #[tokio::test]
    async fn a_switch_names_both_providers_once_per_switch_not_per_request() {
        let (sink, seen) = recorder();
        let chain = FallbackProvider::named(vec![
            ("anthropic".into(), Fixed::failing(ProviderError::RateLimit("slow".into()))),
            ("openai".into(), Fixed::working("hi")),
        ])
        .with_sink(sink);
        for _ in 0..3 {
            let _: Vec<Event> = chain.stream(req()).await.unwrap().collect().await;
        }
        let seen = seen.lock().unwrap();
        assert_eq!(
            *seen,
            vec![
                "anthropic rate-limited, answered by openai".to_string(),
                "anthropic rate-limited, answered by openai; anthropic sits out for 5 min".to_string(),
            ],
            "the first switch and the rest are reported; later turns skip the resting model quietly"
        );
    }

    #[tokio::test]
    async fn the_primary_answering_raises_no_notice() {
        let (sink, seen) = recorder();
        let chain = FallbackProvider::named(vec![
            ("a".into(), Fixed::working("hi")),
            ("b".into(), Fixed::working("there")),
        ])
        .with_sink(sink);
        let _: Vec<Event> = chain.stream(req()).await.unwrap().collect().await;
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_second_switch_after_recovery_is_reported_again() {
        let (sink, seen) = recorder();
        let flaky = Arc::new(Toggle {
            fail: std::sync::atomic::AtomicBool::new(true),
        });
        let chain = FallbackProvider::named(vec![
            ("a".into(), flaky.clone() as Arc<dyn Provider>),
            ("b".into(), Fixed::working("x")),
        ])
        .with_sink(sink);
        let _: Vec<Event> = chain.stream(req()).await.unwrap().collect().await;
        flaky.fail.store(false, std::sync::atomic::Ordering::SeqCst);
        let _: Vec<Event> = chain.stream(req()).await.unwrap().collect().await;
        flaky.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        let _: Vec<Event> = chain.stream(req()).await.unwrap().collect().await;
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert_eq!(seen[1], "back on a");
    }

    struct Toggle {
        fail: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl Provider for Toggle {
        async fn stream(&self, _req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(ProviderError::Network("down".into()));
            }
            Ok(futures::stream::iter(vec![
                Event::TextDelta("a".into()),
                Event::Done(StopReason::EndTurn),
            ])
            .boxed())
        }

        fn caps(&self) -> Caps {
            Caps::default()
        }

        fn count_tokens(&self, _msgs: &[Message]) -> Result<u32, ProviderError> {
            Ok(0)
        }
    }

    #[tokio::test]
    async fn an_empty_chain_reports_rather_than_hanging() {
        let chain = FallbackProvider::new(vec![]);
        assert!(chain.stream(req()).await.is_err());
    }

    struct Sized {
        window: u32,
        tokens: u32,
        fail: Mutex<Option<ProviderError>>,
        calls: std::sync::atomic::AtomicUsize,
        text: &'static str,
    }

    impl Sized {
        fn new(window: u32, tokens: u32, fail: Option<ProviderError>, text: &'static str) -> Arc<Sized> {
            Arc::new(Sized { window, tokens, fail: Mutex::new(fail), calls: Default::default(), text })
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Provider for Sized {
        async fn stream(&self, _req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(err) = self.fail.lock().unwrap().clone() {
                return Err(err);
            }
            Ok(futures::stream::iter(vec![Event::TextDelta(self.text.into()), Event::Done(StopReason::EndTurn)]).boxed())
        }

        fn caps(&self) -> Caps {
            Caps { context_window: self.window, ..Caps::default() }
        }

        fn count_tokens(&self, _msgs: &[Message]) -> Result<u32, ProviderError> {
            Ok(self.tokens)
        }
    }

    async fn answer(chain: &FallbackProvider) -> String {
        let events: Vec<Event> = chain.stream(req()).await.unwrap().collect().await;
        match &events[0] {
            Event::TextDelta(text) => text.clone(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn switches_on_credit_size_and_missing_model_errors_but_not_on_a_bad_tool_schema() {
        let yes = [
            ProviderError::Other("This request would exceed your available credits (HTTP 402)".into()),
            ProviderError::BadRequest("This model's maximum context length is 32768 tokens (HTTP 400)".into()),
            ProviderError::Other("Request too large (HTTP 413)".into()),
            ProviderError::BadRequest("No endpoints found for deepseek/old (HTTP 404)".into()),
            ProviderError::BadRequest("models/gemini-2.5-pro is not found for API version v1beta, or is not supported for generateContent (HTTP 404)".into()),
            ProviderError::RateLimit("slow down".into()),
            ProviderError::Network("error sending request".into()),
        ];
        for err in &yes {
            assert!(switchable(err), "{err}");
        }
        assert!(!switchable(&ProviderError::BadRequest("Invalid schema for function 'write' (HTTP 400)".into())));
        assert_eq!(reason(&yes[0]), "is out of credit");
        assert_eq!(reason(&yes[1]), "cannot fit this conversation");
        assert_eq!(reason(&yes[3]), "does not offer this model");
    }

    #[tokio::test]
    async fn a_model_out_of_credit_sits_out_at_once_and_comes_back_after_its_rest() {
        let (sink, seen) = recorder();
        let broke = Sized::new(100_000, 10, Some(ProviderError::Other("insufficient credits (HTTP 402)".into())), "first");
        let spare = Sized::new(100_000, 10, None, "second");
        let chain = FallbackProvider::named(vec![
            ("openrouter:deepseek".into(), broke.clone() as Arc<dyn Provider>),
            ("google:gemini".into(), spare.clone() as Arc<dyn Provider>),
        ])
        .with_sink(sink);
        assert_eq!(answer(&chain).await, "second");
        assert_eq!(answer(&chain).await, "second");
        assert_eq!(broke.calls(), 1, "a model with no credit is not asked again during its rest");
        assert_eq!(chain.resting().len(), 1);
        assert!(chain.resting()[0].1 > Duration::from_secs(25 * 60));
        assert_eq!(
            seen.lock().unwrap()[0],
            "openrouter:deepseek is out of credit, answered by google:gemini; openrouter:deepseek sits out for 30 min"
        );
    }

    #[tokio::test]
    async fn two_failures_in_a_row_rest_a_model_and_it_is_tried_again_later() {
        let flaky = Sized::new(100_000, 10, Some(ProviderError::RateLimit("slow".into())), "first");
        let spare = Sized::new(100_000, 10, None, "second");
        let chain = FallbackProvider::named(vec![
            ("a".into(), flaky.clone() as Arc<dyn Provider>),
            ("b".into(), spare.clone() as Arc<dyn Provider>),
        ])
        .with_rest(Duration::from_millis(50));
        assert_eq!(answer(&chain).await, "second");
        assert_eq!(answer(&chain).await, "second");
        assert_eq!(answer(&chain).await, "second");
        assert_eq!(flaky.calls(), 2, "after two strikes the model rests");
        assert_eq!(chain.caps().context_window, 100_000);
        tokio::time::sleep(Duration::from_millis(80)).await;
        *flaky.fail.lock().unwrap() = None;
        assert_eq!(answer(&chain).await, "first", "after its rest the first model is tried again");
        assert!(chain.resting().is_empty());
    }

    #[tokio::test]
    async fn a_model_too_small_for_the_conversation_is_skipped_and_caps_follow_the_active_model() {
        let (sink, seen) = recorder();
        let small = Sized::new(8_000, 20_000, None, "small");
        let big = Sized::new(200_000, 20_000, None, "big");
        let chain = FallbackProvider::named(vec![
            ("small".into(), small.clone() as Arc<dyn Provider>),
            ("big".into(), big.clone() as Arc<dyn Provider>),
        ])
        .with_sink(sink);
        assert_eq!(answer(&chain).await, "big");
        assert_eq!(small.calls(), 0, "a model that cannot fit the conversation is not asked");
        assert_eq!(seen.lock().unwrap()[0], "small is too small for this conversation, answered by big");

        let tiny = Sized::new(8_000, 20_000, None, "tiny");
        let only = FallbackProvider::named(vec![("tiny".into(), tiny.clone() as Arc<dyn Provider>), ("tiny2".into(), Sized::new(8_000, 20_000, None, "tiny2") as Arc<dyn Provider>)]);
        assert_eq!(answer(&only).await, "tiny", "when nothing fits, the chain still tries in order");
    }

    #[tokio::test]
    async fn a_single_model_never_rests() {
        let solo = Sized::new(100_000, 10, Some(ProviderError::Other("insufficient credits (HTTP 402)".into())), "x");
        let chain = FallbackProvider::named(vec![("solo".into(), solo.clone() as Arc<dyn Provider>)]);
        assert!(chain.stream(req()).await.is_err());
        assert!(chain.stream(req()).await.is_err());
        assert_eq!(solo.calls(), 2);
        assert!(chain.resting().is_empty());
    }
}
