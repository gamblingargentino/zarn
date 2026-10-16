use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::{LlamaChatMessage, LlamaModel, params::LlamaModelParams};
use llama_cpp_2::sampling::LlamaSampler;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use tokio_stream::wrappers::ReceiverStream;
use zarn_core::provider::{Caps, Event, Provider, ProviderError, ToolSupport};
use zarn_core::types::{Content, Message, Request, StopReason};

static BACKEND: OnceLock<Result<LlamaBackend, String>> = OnceLock::new();

fn backend() -> Result<&'static LlamaBackend, ProviderError> {
    BACKEND
        .get_or_init(|| LlamaBackend::init().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| ProviderError::Other(format!("Failed to initialize llama.cpp: {error}")))
}

pub struct LlamaCppProvider {
    model: Arc<LlamaModel>,
    caps: Caps,
}

impl LlamaCppProvider {
    pub fn new(model_path: impl AsRef<Path>) -> Result<Self, ProviderError> {
        let path = model_path.as_ref();
        if !path.is_file() {
            return Err(ProviderError::BadRequest(format!(
                "llamacpp model file does not exist: {}",
                path.display()
            )));
        }

        let model = LlamaModel::load_from_file(backend()?, path, &LlamaModelParams::default())
            .map_err(|error| ProviderError::Other(format!("Failed to load model: {error}")))?;

        Ok(Self {
            model: Arc::new(model),
            caps: Caps {
                tools: ToolSupport::Prompted,
                streaming: true,
                context_window: 512,
                max_output: 256,
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

fn prompt_for(model: &LlamaModel, messages: &[Message]) -> Result<String, ProviderError> {
    let template = model
        .chat_template(None)
        .map_err(|error| ProviderError::BadRequest(format!("Model has no chat template: {error}")))?;
    let messages = messages
        .iter()
        .map(|message| {
            let (role, content) = match message {
                Message::System(text) => ("system", text.clone()),
                Message::User(items) => ("user", text_content(items)),
                Message::Assistant { content, .. } => ("assistant", text_content(content)),
                Message::ToolResult { content, .. } => ("tool", content.clone()),
            };
            LlamaChatMessage::new(role.to_string(), content)
                .map_err(|error| ProviderError::BadRequest(format!("Invalid chat message: {error}")))
        })
        .collect::<Result<Vec<_>, _>>()?;

    model
        .apply_chat_template(&template, &messages, true)
        .map_err(|error| ProviderError::BadRequest(format!("Failed to format chat prompt: {error}")))
}

fn text_content(items: &[Content]) -> String {
    items
        .iter()
        .filter_map(Content::as_text)
        .collect::<Vec<_>>()
        .join("")
}

#[async_trait]
impl Provider for LlamaCppProvider {
    async fn stream(&self, req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let model = Arc::clone(&self.model);
        let max_tokens = req.max_tokens as usize;
        let temperature = req.temperature;
        let (tx, rx) = tokio::sync::mpsc::channel(128);

        tokio::task::spawn_blocking(move || {
            if let Err(error) = generate_completion(model, &req, max_tokens, temperature, &tx) {
                let _ = tx.blocking_send(Event::Error(error));
            }
        });

        Ok(ReceiverStream::new(rx).boxed())
    }

    fn caps(&self) -> Caps {
        self.caps
    }

    fn count_tokens(&self, msgs: &[Message]) -> Result<u32, ProviderError> {
        let prompt = prompt_for(&self.model, msgs)?;
        let tokens = self.model.vocab().tokenize(prompt.as_bytes(), true, false);
        u32::try_from(tokens.len())
            .map_err(|error| ProviderError::Other(format!("Token count is too large: {error}")))
    }
}

fn generate_completion(
    model: Arc<LlamaModel>,
    req: &Request,
    max_tokens: usize,
    temperature: f32,
    tx: &tokio::sync::mpsc::Sender<Event>,
) -> Result<(), ProviderError> {
    let prompt = prompt_for(&model, &req.messages)?;
    let tokens = model.vocab().tokenize(prompt.as_bytes(), true, false);
    if tokens.is_empty() {
        return Err(ProviderError::BadRequest(
            "The chat template produced an empty prompt".into(),
        ));
    }

    let mut ctx = model
        .new_context(backend()?, LlamaContextParams::default())
        .map_err(|error| ProviderError::Other(format!("Failed to create context: {error}")))?;
    if tokens.len() > ctx.n_ctx() as usize {
        return Err(ProviderError::BadRequest(format!(
            "Prompt has {} tokens, exceeding the llama.cpp context size of {}",
            tokens.len(),
            ctx.n_ctx()
        )));
    }

    let mut batch = LlamaBatch::new(tokens.len(), 1);
    batch
        .add_sequence(&tokens, 0, true)
        .map_err(|error| ProviderError::Other(format!("Failed to prepare prompt: {error}")))?;
    ctx.decode(&mut batch)
        .map_err(|error| ProviderError::Other(format!("Prompt decode failed: {error}")))?;

    let available_tokens = ctx.n_ctx() as usize - tokens.len();
    let max_tokens = max_tokens.min(available_tokens);
    if max_tokens == 0 {
        let _ = tx.blocking_send(Event::Done(StopReason::MaxTokens));
        return Ok(());
    }

    let mut sampler = if temperature <= 0.0 {
        LlamaSampler::greedy()
    } else {
        LlamaSampler::chain_simple([
            LlamaSampler::temp(temperature),
            LlamaSampler::dist(1234),
        ])
    };
    let vocab = model.vocab();
    let mut position = i32::try_from(tokens.len())
        .map_err(|error| ProviderError::BadRequest(format!("Prompt is too long: {error}")))?;

    for index in 0..max_tokens {
        let token = sampler.sample(&ctx, -1);
        if vocab.is_eog(token) {
            let _ = tx.blocking_send(Event::Done(StopReason::EndTurn));
            return Ok(());
        }

        sampler.accept(token);
        let text = String::from_utf8_lossy(&vocab.token_to_piece(token, false, None)).into_owned();
        if !text.is_empty() && tx.blocking_send(Event::TextDelta(text)).is_err() {
            return Ok(());
        }

        if index + 1 == max_tokens {
            let _ = tx.blocking_send(Event::Done(StopReason::MaxTokens));
            return Ok(());
        }

        batch.clear();
        batch
            .add(token, position, &[0], true)
            .map_err(|error| ProviderError::Other(format!("Failed to prepare token: {error}")))?;
        ctx.decode(&mut batch)
            .map_err(|error| ProviderError::Other(format!("Token decode failed: {error}")))?;
        position += 1;
    }

    let _ = tx.blocking_send(Event::Done(StopReason::EndTurn));
    Ok(())
}
