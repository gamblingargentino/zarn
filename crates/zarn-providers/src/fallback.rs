use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use std::sync::Arc;
use zarn_core::provider::{Caps, Event, Provider, ProviderError};
use zarn_core::types::{Message, Request};

pub struct FallbackProvider {
    providers: Vec<Arc<dyn Provider>>,
}

impl FallbackProvider {
    pub fn new(providers: Vec<Arc<dyn Provider>>) -> Self {
        FallbackProvider { providers }
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

        for (index, provider) in self.providers.iter().enumerate() {
            let is_last = index == last;

            let stream = match provider.stream(req.clone()).await {
                Ok(stream) => stream,
                Err(err) => {
                    if err.is_transient() && !is_last {
                        tracing::warn!(index, error = %err, "provider failed, trying next");
                        seen = Some(err);
                        continue;
                    }
                    return Err(err);
                }
            };

            let (first, rest) = stream.into_future().await;
            match first {
                None => return Ok(rest),
                Some(Event::Error(err)) if err.is_transient() && !is_last => {
                    tracing::warn!(index, error = %err, "provider errored on open, trying next");
                    seen = Some(err);
                    continue;
                }
                Some(event) => {
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

    #[tokio::test]
    async fn an_empty_chain_reports_rather_than_hanging() {
        let chain = FallbackProvider::new(vec![]);
        assert!(chain.stream(req()).await.is_err());
    }
}
