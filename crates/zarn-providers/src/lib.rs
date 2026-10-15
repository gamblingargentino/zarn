pub mod fallback;
pub mod openai;

pub use fallback::FallbackProvider;
pub use openai::OpenAiCompatProvider;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use zarn_core::provider::{Caps, Provider, ProviderError, ToolSupport};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ProviderSpec {
    pub kind: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub context_window: Option<u32>,
    #[serde(default)]
    pub max_output: Option<u32>,
    #[serde(default)]
    pub vision: Option<bool>,
    #[serde(default)]
    pub tools: Option<String>,
    #[serde(flatten)]
    #[serde(default)]
    pub extra: HashMap<String, String>,
}

impl ProviderSpec {
    fn caps(&self, base: Caps) -> Caps {
        Caps {
            tools: match self.tools.as_deref() {
                Some("native") => ToolSupport::Native,
                Some("prompted") => ToolSupport::Prompted,
                Some("none") => ToolSupport::None,
                _ => base.tools,
            },
            vision: self.vision.unwrap_or(base.vision),
            context_window: self.context_window.unwrap_or(base.context_window),
            max_output: self.max_output.unwrap_or(base.max_output),
            ..base
        }
    }
}

pub fn build(spec: &ProviderSpec) -> Result<Arc<dyn Provider>, ProviderError> {
    match spec.kind.as_str() {
        "openai" | "openai-compat" => {
            let base_url = spec
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            let api_key = spec.api_key.clone().unwrap_or_default();
            Ok(Arc::new(
                OpenAiCompatProvider::new(base_url, api_key).with_caps(spec.caps(Caps::default())),
            ))
        }
        other => Err(ProviderError::BadRequest(format!(
            "unknown provider kind: {other}"
        ))),
    }
}

pub fn build_chain(specs: &[ProviderSpec]) -> Result<Arc<dyn Provider>, ProviderError> {
    let providers = specs.iter().map(build).collect::<Result<Vec<_>, _>>()?;
    match providers.len() {
        0 => Err(ProviderError::Other("no providers configured".into())),
        1 => Ok(providers.into_iter().next().unwrap()),
        _ => Ok(Arc::new(FallbackProvider::new(providers))),
    }
}
