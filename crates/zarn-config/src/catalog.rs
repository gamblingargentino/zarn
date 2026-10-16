use zarn_providers::ProviderSpec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kind {
    pub id: &'static str,
    pub label: &'static str,
    pub default_base_url: Option<&'static str>,
    pub probe_url: Option<&'static str>,
    pub asks_base_url: bool,
    pub needs_key: bool,
    pub env_hint: Option<&'static str>,
    pub models: &'static [&'static str],
}

pub const KINDS: &[Kind] = &[
    Kind {
        id: "anthropic",
        label: "Anthropic (Claude)",
        default_base_url: None,
        probe_url: Some("https://api.anthropic.com"),
        asks_base_url: false,
        needs_key: true,
        env_hint: Some("ANTHROPIC_API_KEY"),
        models: &["claude-sonnet-5-5", "claude-opus-5-5", "claude-haiku-5-5"],
    },
    Kind {
        id: "openai",
        label: "OpenAI",
        default_base_url: Some("https://api.openai.com/v1"),
        probe_url: Some("https://api.openai.com/v1"),
        asks_base_url: false,
        needs_key: true,
        env_hint: Some("OPENAI_API_KEY"),
        models: &["gpt-5", "gpt-5-mini"],
    },
    Kind {
        id: "google",
        label: "Google (Gemini)",
        default_base_url: None,
        probe_url: Some("https://generativelanguage.googleapis.com"),
        asks_base_url: false,
        needs_key: true,
        env_hint: Some("GEMINI_API_KEY"),
        models: &["gemini-2.5-pro", "gemini-2.5-flash"],
    },
    Kind {
        id: "openai-compat",
        label: "OpenAI-compatible (Groq, OpenRouter, Together, vLLM, LM Studio)",
        default_base_url: None,
        probe_url: None,
        asks_base_url: true,
        needs_key: false,
        env_hint: None,
        models: &[],
    },
    Kind {
        id: "ollama",
        label: "Ollama (local)",
        default_base_url: Some("http://localhost:11434"),
        probe_url: None,
        asks_base_url: true,
        needs_key: false,
        env_hint: None,
        models: &[],
    },
];

pub fn kind(id: &str) -> Option<&'static Kind> {
    let id = match id {
        "claude" => "anthropic",
        "gemini" => "google",
        other => other,
    };
    KINDS.iter().find(|k| k.id == id)
}

pub fn key_page(id: &str) -> Option<&'static str> {
    match kind(id)?.id {
        "anthropic" => Some("https://console.anthropic.com/settings/keys"),
        "openai" => Some("https://platform.openai.com/api-keys"),
        "google" => Some("https://aistudio.google.com/apikey"),
        _ => None,
    }
}

pub fn needs_key(spec: &ProviderSpec) -> bool {
    match kind(&spec.kind) {
        Some(k) if k.id == "openai" => spec
            .base_url
            .as_deref()
            .is_none_or(|url| url.contains("api.openai.com")),
        Some(k) => k.needs_key,
        None => false,
    }
}

pub fn probe_url(spec: &ProviderSpec) -> Option<String> {
    spec.base_url
        .clone()
        .or_else(|| kind(&spec.kind).and_then(|k| k.probe_url.map(str::to_string)))
}

pub fn suggested_models(kind_id: &str) -> &'static [&'static str] {
    kind(kind_id).map(|k| k.models).unwrap_or(&[])
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve_to_their_canonical_kind() {
        assert_eq!(kind("claude").unwrap().id, "anthropic");
        assert_eq!(kind("gemini").unwrap().id, "google");
        assert!(kind("nope").is_none());
    }

    #[test]
    fn openai_against_a_local_server_needs_no_key() {
        let spec = ProviderSpec {
            kind: "openai".into(),
            base_url: Some("http://localhost:1234/v1".into()),
            ..ProviderSpec::default()
        };
        assert!(!needs_key(&spec));
        let hosted = ProviderSpec {
            kind: "openai".into(),
            ..ProviderSpec::default()
        };
        assert!(needs_key(&hosted));
    }

    #[test]
    fn provider_names_are_plain_identifiers() {
        assert!(valid_name("my-groq_2"));
        assert!(!valid_name(""));
        assert!(!valid_name("has space"));
        assert!(!valid_name("dot.ted"));
    }
}
