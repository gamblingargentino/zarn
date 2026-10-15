use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use llama_cpp_2::llama_model::LlamaModel;
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use zarn_core::provider::{Caps, Event, Provider, ProviderError, ToolSupport};
use zarn_core::types::{Message, Request, StopReason};

pub struct LlamaCppProvider {
    model: Arc<LlamaModel>,
    caps: Caps,
}

impl LlamaCppProvider {
    pub fn new(model_path: impl Into<String>) -> Result<Self, ProviderError> {
        let path = model_path.into();
        let model = LlamaModel::new(&path)
            .map_err(|e| ProviderError::Other(format!("Failed to load model: {e}")))?;

        Ok(LlamaCppProvider {
            model: Arc::new(model),
            caps: Caps {
                tools: ToolSupport::Prompted,
                streaming: true,
                ..Caps::default()
            },
        })
    }

    pub fn with_caps(mut self, caps: Caps) -> Self {
        self.caps = Caps {
            tools: ToolSupport::Prompted,
            streaming: true,
            ..caps
        };
        self
    }
}

#[async_trait]
impl Provider for LlamaCppProvider {
    async fn stream(&self, req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let model = Arc::clone(&self.model);
        let max_tokens = req.max_tokens as usize;

        let prompt = req
            .messages
            .iter()
            .map(|m| m.text())
            .collect::<Vec<_>>()
            .join("\n");

        let (tx, rx) = tokio::sync::mpsc::channel(128);

        tokio::task::spawn_blocking(move || {
            let _ = generate_completion(model, &prompt, max_tokens, tx).await;
        });

        Ok(ReceiverStream::new(rx).boxed())
    }

    fn caps(&self) -> Caps {
        self.caps
    }

    fn count_tokens(&self, msgs: &[Message]) -> Result<u32, ProviderError> {
        let bpe = tiktoken_rs::cl100k_base()
            .map_err(|e| ProviderError::Other(format!("tokenizer unavailable: {e}")))?;
        let text = msgs.iter().map(Message::text).collect::<Vec<_>>().join("\n");
        Ok(bpe.encode_with_special_tokens(&text).len() as u32)
    }
}

async fn generate_completion(
    model: Arc<LlamaModel>,
    prompt: &str,
    max_tokens: usize,
    tx: tokio::sync::mpsc::Sender<Event>,
) -> Result<(), ProviderError> {
    let mut ctx = model
        .create_context(Default::default())
        .map_err(|e| ProviderError::Other(format!("Failed to create context: {e}")))?;

    let tokens = model
        .tokenize(prompt, true)
        .map_err(|e| ProviderError::Other(format!("Tokenization failed: {e}")))?;

    if tokens.is_empty() {
        let _ = tx.send(Event::Done(StopReason::EndTurn)).await;
        return Ok(());
    }

    for token in &tokens {
        ctx.decode(*token)
            .map_err(|e| ProviderError::Other(format!("Decode failed: {e}")))?;
    }

    for i in 0..max_tokens {
        let next_token = ctx
            .sample_next_token(Default::default())
            .map_err(|e| ProviderError::Other(format!("Sampling failed: {e}")))?;

        if next_token == model.token_eos() {
            let _ = tx.send(Event::Done(StopReason::EndTurn)).await;
            return Ok(());
        }

        let text = model
            .token_to_str(next_token)
            .map_err(|e| ProviderError::Other(format!("Token to string failed: {e}")))?;

        if !text.is_empty() {
            if tx.send(Event::TextDelta(text)).await.is_err() {
                return Ok(());
            }
        }

        ctx.decode(next_token)
            .map_err(|e| ProviderError::Other(format!("Decode failed: {e}")))?;

        if i >= max_tokens - 1 {
            let _ = tx.send(Event::Done(StopReason::MaxTokens)).await;
            return Ok(());
        }
    }

    let _ = tx.send(Event::Done(StopReason::EndTurn)).await;
    Ok(())
}
