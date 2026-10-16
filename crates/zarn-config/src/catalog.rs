use zarn_providers::ProviderSpec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kind {
    pub id: &'static str,
    pub label: &'static str,
    pub backend: &'static str,
    pub default_base_url: Option<&'static str>,
    pub probe_url: Option<&'static str>,
    pub asks_base_url: bool,
    pub needs_key: bool,
    pub env_hint: Option<&'static str>,
    pub models: &'static [&'static str],
    pub key_prefixes: &'static [&'static str],
    pub key_page: Option<&'static str>,
}

const fn hosted(
    id: &'static str,
    label: &'static str,
    base_url: &'static str,
    env_hint: &'static str,
    key_prefixes: &'static [&'static str],
    key_page: &'static str,
    models: &'static [&'static str],
) -> Kind {
    Kind {
        id,
        label,
        backend: "openai-compat",
        default_base_url: Some(base_url),
        probe_url: Some(base_url),
        asks_base_url: false,
        needs_key: true,
        env_hint: Some(env_hint),
        models,
        key_prefixes,
        key_page: Some(key_page),
    }
}

const fn local(id: &'static str, label: &'static str, backend: &'static str, base_url: &'static str) -> Kind {
    Kind {
        id,
        label,
        backend,
        default_base_url: Some(base_url),
        probe_url: None,
        asks_base_url: true,
        needs_key: false,
        env_hint: None,
        models: &[],
        key_prefixes: &[],
        key_page: None,
    }
}

pub const KINDS: &[Kind] = &[
    Kind {
        id: "anthropic",
        label: "Anthropic (Claude)",
        backend: "anthropic",
        default_base_url: None,
        probe_url: Some("https://api.anthropic.com"),
        asks_base_url: false,
        needs_key: true,
        env_hint: Some("ANTHROPIC_API_KEY"),
        models: &["claude-sonnet-5-5", "claude-opus-5-5", "claude-haiku-5-5"],
        key_prefixes: &["sk-ant-"],
        key_page: Some("https://console.anthropic.com/settings/keys"),
    },
    Kind {
        id: "openai",
        label: "OpenAI",
        backend: "openai",
        default_base_url: Some("https://api.openai.com/v1"),
        probe_url: Some("https://api.openai.com/v1"),
        asks_base_url: false,
        needs_key: true,
        env_hint: Some("OPENAI_API_KEY"),
        models: &["gpt-5", "gpt-5-mini"],
        key_prefixes: &["sk-proj-", "sk-svcacct-", "sk-admin-", "sk-"],
        key_page: Some("https://platform.openai.com/api-keys"),
    },
    Kind {
        id: "google",
        label: "Google (Gemini)",
        backend: "google",
        default_base_url: None,
        probe_url: Some("https://generativelanguage.googleapis.com"),
        asks_base_url: false,
        needs_key: true,
        env_hint: Some("GEMINI_API_KEY"),
        models: &["gemini-2.5-pro", "gemini-2.5-flash"],
        key_prefixes: &["AIza", "AQ."],
        key_page: Some("https://aistudio.google.com/apikey"),
    },
    hosted("openrouter", "OpenRouter (hundreds of models, one key)", "https://openrouter.ai/api/v1", "OPENROUTER_API_KEY", &["sk-or-"], "https://openrouter.ai/keys", &[]),
    hosted("groq", "Groq", "https://api.groq.com/openai/v1", "GROQ_API_KEY", &["gsk_"], "https://console.groq.com/keys", &[]),
    hosted("deepseek", "DeepSeek", "https://api.deepseek.com/v1", "DEEPSEEK_API_KEY", &["sk-"], "https://platform.deepseek.com/api_keys", &["deepseek-chat", "deepseek-reasoner"]),
    hosted("mistral", "Mistral", "https://api.mistral.ai/v1", "MISTRAL_API_KEY", &[], "https://console.mistral.ai/api-keys", &[]),
    hosted("xai", "xAI (Grok)", "https://api.x.ai/v1", "XAI_API_KEY", &["xai-"], "https://console.x.ai", &[]),
    hosted("together", "Together AI", "https://api.together.xyz/v1", "TOGETHER_API_KEY", &["tgp_"], "https://api.together.ai/settings/api-keys", &[]),
    hosted("fireworks", "Fireworks AI", "https://api.fireworks.ai/inference/v1", "FIREWORKS_API_KEY", &["fw_"], "https://fireworks.ai/account/api-keys", &[]),
    hosted("cerebras", "Cerebras", "https://api.cerebras.ai/v1", "CEREBRAS_API_KEY", &["csk-"], "https://cloud.cerebras.ai", &[]),
    hosted("moonshot", "Moonshot (Kimi)", "https://api.moonshot.ai/v1", "MOONSHOT_API_KEY", &["sk-"], "https://platform.moonshot.ai/console/api-keys", &[]),
    hosted("qwen", "Alibaba Qwen (DashScope)", "https://dashscope-intl.aliyuncs.com/compatible-mode/v1", "DASHSCOPE_API_KEY", &["sk-"], "https://modelstudio.console.alibabacloud.com", &[]),
    hosted("zai", "Z.ai (GLM)", "https://api.z.ai/api/paas/v4", "ZAI_API_KEY", &[], "https://z.ai/manage-apikey/apikey-list", &["glm-4.6", "glm-4.5-air"]),
    hosted("perplexity", "Perplexity (Sonar)", "https://api.perplexity.ai", "PERPLEXITY_API_KEY", &["pplx-"], "https://www.perplexity.ai/settings/api", &["sonar", "sonar-pro", "sonar-reasoning-pro", "sonar-deep-research"]),
    hosted("nvidia", "NVIDIA NIM", "https://integrate.api.nvidia.com/v1", "NVIDIA_API_KEY", &["nvapi-"], "https://build.nvidia.com", &[]),
    hosted("huggingface", "Hugging Face Inference", "https://router.huggingface.co/v1", "HF_TOKEN", &["hf_"], "https://huggingface.co/settings/tokens", &[]),
    Kind {
        id: "openai-compat",
        label: "Other OpenAI-compatible endpoint (vLLM, a proxy, your own server)",
        backend: "openai-compat",
        default_base_url: None,
        probe_url: None,
        asks_base_url: true,
        needs_key: false,
        env_hint: None,
        models: &[],
        key_prefixes: &[],
        key_page: None,
    },
    local("ollama", "Ollama (local)", "ollama", "http://localhost:11434"),
    local("lmstudio", "LM Studio (local)", "openai-compat", "http://localhost:1234/v1"),
];

pub fn kind(id: &str) -> Option<&'static Kind> {
    let id = match id {
        "claude" => "anthropic",
        "gemini" => "google",
        "grok" => "xai",
        "kimi" => "moonshot",
        "dashscope" => "qwen",
        "glm" => "zai",
        "hf" => "huggingface",
        other => other,
    };
    KINDS.iter().find(|k| k.id == id)
}

pub fn key_page(id: &str) -> Option<&'static str> {
    kind(id)?.key_page
}

fn same_url(a: &str, b: &str) -> bool {
    a.trim_end_matches('/').eq_ignore_ascii_case(b.trim_end_matches('/'))
}

pub fn for_spec(spec: &ProviderSpec) -> Option<&'static Kind> {
    if let Some(base) = spec.base_url.as_deref()
        && let Some(preset) = KINDS.iter().find(|k| {
            k.id != k.backend && k.backend == spec.kind && k.default_base_url.is_some_and(|u| same_url(u, base))
        })
    {
        return Some(preset);
    }
    kind(&spec.kind)
}

pub fn label_for(spec: &ProviderSpec) -> String {
    match for_spec(spec) {
        Some(k) if k.id != spec.kind => format!("{} ({})", k.id, spec.kind),
        _ => spec.kind.clone(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detected {
    One(&'static Kind),
    Several(Vec<&'static Kind>),
    Unknown,
}

fn looks_like_zai(key: &str) -> bool {
    match key.split_once('.') {
        Some((id, secret)) => {
            id.len() == 32
                && id.chars().all(|c| c.is_ascii_hexdigit())
                && !secret.is_empty()
                && secret.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

pub fn detect(key: &str) -> Detected {
    let key = key.trim();
    if looks_like_zai(key) {
        return kind("zai").map(Detected::One).unwrap_or(Detected::Unknown);
    }
    let mut best = 0;
    let mut found: Vec<&'static Kind> = Vec::new();
    for k in KINDS {
        let Some(len) = k.key_prefixes.iter().filter(|p| key.starts_with(*p)).map(|p| p.len()).max() else {
            continue;
        };
        if len > best {
            best = len;
            found.clear();
        }
        if len == best {
            found.push(k);
        }
    }
    match found.len() {
        0 => Detected::Unknown,
        1 => Detected::One(found[0]),
        _ => Detected::Several(found),
    }
}

pub fn needs_key(spec: &ProviderSpec) -> bool {
    match for_spec(spec) {
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

pub fn suggested_for(spec: &ProviderSpec) -> &'static [&'static str] {
    for_spec(spec).map(|k| k.models).unwrap_or(&[])
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
    fn hosted_presets_store_as_openai_compatible_with_their_own_url_and_still_need_a_key() {
        let groq = kind("groq").unwrap();
        assert_eq!(groq.backend, "openai-compat");
        assert_eq!(groq.default_base_url, Some("https://api.groq.com/openai/v1"));
        let spec = ProviderSpec {
            kind: "openai-compat".into(),
            base_url: Some("https://api.groq.com/openai/v1/".into()),
            ..ProviderSpec::default()
        };
        assert_eq!(for_spec(&spec).map(|k| k.id), Some("groq"));
        assert!(needs_key(&spec));
        assert_eq!(label_for(&spec), "groq (openai-compat)");
        let custom = ProviderSpec {
            kind: "openai-compat".into(),
            base_url: Some("http://localhost:9000/v1".into()),
            ..ProviderSpec::default()
        };
        assert!(!needs_key(&custom));
        assert_eq!(kind("grok").unwrap().id, "xai");
        assert!(KINDS.len() >= 18);
        let mut ids: Vec<&str> = KINDS.iter().map(|k| k.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), KINDS.len());
    }

    #[test]
    fn a_key_names_its_provider_when_its_prefix_is_unique() {
        let one = |key: &str| match detect(key) {
            Detected::One(k) => k.id,
            other => panic!("{key}: {other:?}"),
        };
        assert_eq!(one("sk-ant-api03-test-key-not-real"), "anthropic");
        assert_eq!(one("sk-or-v1-test-key-not-real"), "openrouter");
        assert_eq!(one("sk-proj-test-key-not-real"), "openai");
        assert_eq!(one("gsk_test-key-not-real"), "groq");
        assert_eq!(one("AIza-test-key-not-real"), "google");
        assert_eq!(one("AQ.test-key-not-real"), "google");
        assert_eq!(one("xai-test-key-not-real"), "xai");
        assert_eq!(one("pplx-test-key-not-real"), "perplexity");
        assert_eq!(one("nvapi-test-key-not-real"), "nvidia");
        assert_eq!(one("hf_test-key-not-real"), "huggingface");
        assert_eq!(one("0123456789abcdef0123456789abcdef.notreal"), "zai");
        match detect("sk-test-key-not-real") {
            Detected::Several(kinds) => {
                let ids: Vec<&str> = kinds.iter().map(|k| k.id).collect();
                assert_eq!(ids, vec!["openai", "deepseek", "moonshot", "qwen"]);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(detect("test-key-not-real"), Detected::Unknown);
    }

    #[test]
    fn provider_names_are_plain_identifiers() {
        assert!(valid_name("my-groq_2"));
        assert!(!valid_name(""));
        assert!(!valid_name("has space"));
        assert!(!valid_name("dot.ted"));
    }
}
