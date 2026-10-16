use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use std::sync::{Arc, Mutex};
use zarn_core::provider::{Caps, Event, Provider, ProviderError};
use zarn_core::types::{Message, Request};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Switch {
    pub failed: Vec<(String, String)>,
    pub answered: String,
}

impl Switch {
    pub fn notice(&self) -> String {
        let failed = self
            .failed
            .iter()
            .map(|(name, why)| format!("{name} {why}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{failed}, answered by {}", self.answered)
    }
}

pub fn reason(err: &ProviderError) -> &'static str {
    match err {
        ProviderError::RateLimit(_) => "rate-limited",
        ProviderError::Auth(_) => "rejected the key",
        ProviderError::Network(_) => "unreachable",
        ProviderError::BadRequest(_) => "refused the request",
        ProviderError::Other(_) => "failed",
    }
}

pub type SwitchSink = Arc<dyn Fn(&Switch) + Send + Sync>;

pub struct FallbackProvider {
    providers: Vec<Arc<dyn Provider>>,
    names: Vec<String>,
    sink: Option<SwitchSink>,
    last: Mutex<Option<usize>>,
}

impl FallbackProvider {
    pub fn new(providers: Vec<Arc<dyn Provider>>) -> Self {
        let names = (0..providers.len()).map(|i| format!("provider {}", i + 1)).collect();
        FallbackProvider {
            providers,
            names,
            sink: None,
            last: Mutex::new(None),
        }
    }

    pub fn named(entries: Vec<(String, Arc<dyn Provider>)>) -> Self {
        let (names, providers) = entries.into_iter().unzip();
        FallbackProvider {
            providers,
            names,
            sink: None,
            last: Mutex::new(None),
        }
    }

    pub fn with_sink(mut self, sink: SwitchSink) -> Self {
        self.sink = Some(sink);
        self
    }

    fn answered(&self, index: usize, failed: Vec<(String, String)>) {
        let Ok(mut last) = self.last.lock() else {
            return;
        };
        let previous = last.replace(index);
        if index == 0 || previous == Some(index) {
            return;
        }
        drop(last);
        if let Some(sink) = &self.sink {
            sink(&Switch {
                failed,
                answered: self.names[index].clone(),
            });
        }
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    fn primary(&self) -> Option<&Arc<dyn Provider>> {
        self.providers.first()
    }
}

#[async_trait]
impl Provider for FallbackProvider {
    async fn stream(&self, req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let last = self.providers.len().saturating_sub(1);
        let mut seen: Option<ProviderError> = None;
        let mut failed: Vec<(String, String)> = Vec::new();

        for (index, provider) in self.providers.iter().enumerate() {
            let is_last = index == last;

            let stream = match provider.stream(req.clone()).await {
                Ok(stream) => stream,
                Err(err) => {
                    if err.is_transient() && !is_last {
                        tracing::warn!(index, error = %err, "provider failed, trying next");
                        failed.push((self.names[index].clone(), reason(&err).to_string()));
                        seen = Some(err);
                        continue;
                    }
                    return Err(err);
                }
            };

            let (first, rest) = stream.into_future().await;
            match first {
                None => {
                    self.answered(index, failed);
                    return Ok(rest);
                }
                Some(Event::Error(err)) if err.is_transient() && !is_last => {
                    tracing::warn!(index, error = %err, "provider errored on open, trying next");
                    failed.push((self.names[index].clone(), reason(&err).to_string()));
                    seen = Some(err);
                    continue;
                }
                Some(event) => {
                    if !matches!(event, Event::Error(_)) {
                        self.answered(index, failed);
                    }
                    return Ok(futures::stream::once(async move { event })
                        .chain(rest)
                        .boxed());
                }
            }
        }

        Err(seen.unwrap_or_else(|| ProviderError::Other("no providers configured".into())))
    }

    fn caps(&self) -> Caps {
        self.primary().map(|p| p.caps()).unwrap_or_default()
    }

    fn count_tokens(&self, msgs: &[Message]) -> Result<u32, ProviderError> {
        match self.primary() {
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
        assert_eq!(*seen, vec!["anthropic rate-limited, answered by openai".to_string()]);
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
        assert_eq!(seen.lock().unwrap().len(), 2);
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
}
