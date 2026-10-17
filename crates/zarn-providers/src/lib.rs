pub mod anthropic;
pub mod discovery;
pub mod fallback;
pub mod google;
pub mod openai;
pub mod ollama;
pub mod patient;
pub mod pinned;
pub mod exec;

#[cfg(feature = "local")]
pub mod llamacpp;

pub use anthropic::AnthropicProvider;
pub use fallback::{FallbackProvider, Switch, SwitchSink};
pub use google::GoogleProvider;
pub use openai::OpenAiCompatProvider;
pub use ollama::OllamaProvider;
pub use patient::Patient;
pub use pinned::PinnedModel;
pub use exec::ExecProvider;

#[cfg(feature = "local")]
pub use llamacpp::LlamaCppProvider;

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use zarn_core::prompted::PromptedProvider;
use zarn_core::provider::{Caps, Provider, ProviderError, ToolSupport};

pub fn error_message(body: &str) -> String {
    let trimmed = body.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        let value = match &value {
            serde_json::Value::Array(items) => items.first().cloned().unwrap_or(serde_json::Value::Null),
            other => other.clone(),
        };
        let found = ["/error/message", "/message", "/detail", "/error/detail", "/error_message"]
            .iter()
            .find_map(|path| value.pointer(path).and_then(|v| v.as_str()))
            .or_else(|| value.get("error").and_then(|v| v.as_str()));
        if let Some(message) = found {
            return message.trim().to_string();
        }
    }
    let mut short: String = trimmed.chars().take(300).collect();
    if trimmed.chars().count() > 300 {
        short.push('…');
    }
    short
}

pub fn affordable_tokens(message: &str) -> Option<u32> {
    let at = message.find("can only afford")? + "can only afford".len();
    let digits: String = message[at..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

#[derive(Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProviderSpec {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing)]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_helper: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<String>,
    #[serde(flatten)]
    #[serde(default)]
    pub extra: std::collections::BTreeMap<String, String>,
}

impl std::fmt::Debug for ProviderSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderSpec")
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .field("api_key_env", &self.api_key_env)
            .field("api_key_helper", &self.api_key_helper)
            .field("context_window", &self.context_window)
            .field("max_output", &self.max_output)
            .field("vision", &self.vision)
            .field("tools", &self.tools)
            .field("extra", &self.extra)
            .finish()
    }
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
    Ok(Patient::wrap(PromptedProvider::wrap(build_raw(spec)?)))
}

fn build_raw(spec: &ProviderSpec) -> Result<Arc<dyn Provider>, ProviderError> {
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
        "anthropic" | "claude" => {
            let api_key = spec.api_key.clone().unwrap_or_default();
            let mut provider = AnthropicProvider::new(api_key);
            if let Some(base_url) = &spec.base_url {
                provider = provider.with_base_url(base_url.clone());
            }
            Ok(Arc::new(provider.with_caps(spec.caps(Caps {
                caching: true,
                vision: true,
                ..Caps::default()
            }))))
        }
        "google" | "gemini" => {
            let api_key = spec.api_key.clone().unwrap_or_default();
            let mut provider = GoogleProvider::new(api_key);
            if let Some(base_url) = &spec.base_url {
                provider = provider.with_base_url(base_url.clone());
            }
            Ok(Arc::new(provider.with_caps(spec.caps(Caps {
                vision: true,
                ..Caps::default()
            }))))
        }
        "ollama" => {
            let base_url = spec
                .base_url
                .clone()
                .unwrap_or_else(|| "http://localhost:11434".to_string());
            Ok(Arc::new(OllamaProvider::new(base_url).with_caps(spec.caps(Caps {
                tools: ToolSupport::Prompted,
                ..Caps::default()
            }))))
        }
        "exec" => {
            let command = spec.extra.get("command").ok_or_else(|| {
                ProviderError::BadRequest("exec provider requires 'command' in extra".into())
            })?;
            let args: Vec<String> = spec
                .extra
                .get("args")
                .map(|a| {
                    a.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            Ok(Arc::new(
                ExecProvider::new(command, args).with_caps(spec.caps(Caps::default())),
            ))
        }
        #[cfg(feature = "local")]
        "llamacpp" => {
            let model_path = spec
                .extra
                .get("model_path")
                .or(spec.base_url.as_ref())
                .ok_or_else(|| {
                    ProviderError::BadRequest(
                        "llamacpp provider requires 'model_path' with a GGUF model file".into(),
                    )
                })?;
            let provider = llamacpp::LlamaCppProvider::new(model_path)?;
            Ok(Arc::new(provider.with_caps(spec.caps(Caps {
                tools: ToolSupport::Prompted,
                ..Caps::default()
            }))))
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

#[cfg(test)]
mod tests {
    use super::*;
    use zarn_core::provider::ToolSupport;

    fn spec(kind: &str) -> ProviderSpec {
        ProviderSpec {
            kind: kind.to_string(),
            ..ProviderSpec::default()
        }
    }

    #[test]
    fn a_prompted_backend_is_wrapped_so_tool_specs_reach_the_prompt() {
        let provider = build(&spec("ollama")).unwrap();
        assert_eq!(provider.caps().tools, ToolSupport::Native);
    }

    #[test]
    fn a_native_backend_is_not_wrapped() {
        let provider = build(&spec("openai")).unwrap();
        assert_eq!(provider.caps().tools, ToolSupport::Native);
    }

    #[test]
    fn every_adapter_kind_the_factory_documents_can_be_built() {
        for kind in [
            "openai",
            "openai-compat",
            "anthropic",
            "claude",
            "google",
            "gemini",
            "ollama",
        ] {
            assert!(build(&spec(kind)).is_ok(), "{kind} failed to build");
        }
    }

    #[cfg(feature = "local")]
    #[test]
    fn llamacpp_requires_a_model_path() {
        assert!(matches!(
            build(&spec("llamacpp")),
            Err(ProviderError::BadRequest(_))
        ));
    }

    #[test]
    fn anthropic_reports_caching_so_cost_tracking_sees_cached_tokens() {
        assert!(build(&spec("anthropic")).unwrap().caps().caching);
    }

    #[test]
    fn debug_output_never_shows_the_key() {
        let spec = ProviderSpec {
            kind: "openai".into(),
            api_key: Some("test-key-not-real-debug".into()),
            ..ProviderSpec::default()
        };
        assert!(!format!("{spec:?}").contains("test-key-not-real-debug"));
    }

    #[test]
    fn an_unknown_kind_is_a_bad_request() {
        assert!(matches!(
            build(&spec("nope")),
            Err(ProviderError::BadRequest(_))
        ));
    }

    #[test]
    fn exec_without_a_command_is_refused() {
        assert!(build(&spec("exec")).is_err());
    }

    #[test]
    fn a_single_spec_is_not_wrapped_in_a_fallback_chain() {
        assert!(build_chain(&[spec("openai")]).is_ok());
        assert!(build_chain(&[]).is_err());
    }
}
