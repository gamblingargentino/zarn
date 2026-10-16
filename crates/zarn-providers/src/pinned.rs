use async_trait::async_trait;
use futures::stream::BoxStream;
use std::sync::Arc;
use zarn_core::provider::{Caps, Event, Provider, ProviderError};
use zarn_core::types::{Message, Request};

pub struct PinnedModel {
    inner: Arc<dyn Provider>,
    model: String,
}

impl PinnedModel {
    pub fn wrap(inner: Arc<dyn Provider>, model: impl Into<String>) -> Arc<dyn Provider> {
        Arc::new(PinnedModel {
            inner,
            model: model.into(),
        })
    }
}

#[async_trait]
impl Provider for PinnedModel {
    async fn stream(&self, mut req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        req.model = self.model.clone();
        self.inner.stream(req).await
    }

    fn caps(&self) -> Caps {
        self.inner.caps()
    }

    fn count_tokens(&self, msgs: &[Message]) -> Result<u32, ProviderError> {
        self.inner.count_tokens(msgs)
    }
}
