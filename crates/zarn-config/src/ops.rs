use crate::auth::{KeySource, KeyStore, redact};
use crate::catalog;
use crate::layers::{GeneralField, Layers, Scope};
use crate::{Config, ProfileSpec};
use anyhow::{Context, Result, anyhow, bail};
use futures::StreamExt;
use std::time::Duration;
use zarn_core::provider::Event;
use zarn_core::types::{Message, Request};
use zarn_providers::ProviderSpec;
use zarn_providers::discovery::ModelInfo;
use zarn_tools::mcp::ServerConfig;
use zarn_tools::permissions::Policy;

pub const TEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderDraft {
    pub name: String,
    pub kind: String,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub model: Option<String>,
}

impl ProviderDraft {
    pub fn spec(&self) -> ProviderSpec {
        let preset = catalog::kind(&self.kind);
        let default_base = preset.and_then(|k| k.default_base_url).map(str::to_string);
        ProviderSpec {
            kind: preset.map(|k| k.backend.to_string()).unwrap_or_else(|| self.kind.clone()),
            base_url: self
                .base_url
                .clone()
                .filter(|u| !u.trim().is_empty())
                .or(default_base),
            api_key_env: self.api_key_env.clone().filter(|v| !v.trim().is_empty()),
            ..ProviderSpec::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderRow {
    pub name: String,
    pub kind: String,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub key: Option<KeySource>,
    pub needs_key: bool,
    pub scope: Scope,
    pub is_default: bool,
}

impl ProviderRow {
    pub fn key_label(&self) -> String {
        match (&self.key, self.needs_key) {
            (Some(KeySource::Env(var)), _) => format!("key from ${var}"),
            (Some(KeySource::Helper), _) => "key from api_key_helper".into(),
            (Some(KeySource::Store), _) => "key in credential store".into(),
            (Some(KeySource::File), _) => "key in fallback file".into(),
            (None, true) => "NO KEY".into(),
            (None, false) => "no key needed".into(),
        }
    }
}

pub fn providers(layers: &Layers, store: &KeyStore) -> Vec<ProviderRow> {
    let cfg = layers.merged();
    let default = cfg
        .profiles
        .get(&cfg.default_profile())
        .and_then(|p| p.provider.clone());
    cfg.providers
        .iter()
        .map(|(name, spec)| ProviderRow {
            name: name.clone(),
            kind: catalog::label_for(spec),
            base_url: spec.base_url.clone(),
            api_key_env: spec.api_key_env.clone(),
            key: store.has_with(name, spec.api_key_env.as_deref(), spec.api_key_helper.as_deref()),
            needs_key: catalog::needs_key(spec),
            scope: layers.provider_scope(name),
            is_default: default.as_deref() == Some(name.as_str()),
        })
        .collect()
}

fn check_name(name: &str) -> Result<()> {
    if !catalog::valid_name(name) {
        bail!("'{name}' is not a valid provider name: use letters, digits, '-' and '_'");
    }
    Ok(())
}

pub fn add_provider(layers: &mut Layers, draft: &ProviderDraft) -> Result<Scope> {
    check_name(&draft.name)?;
    if draft.kind.trim().is_empty() {
        bail!("a provider needs a kind");
    }
    if catalog::kind(&draft.kind).is_none() && draft.kind != "exec" {
        bail!("unknown provider kind '{}'", draft.kind);
    }
    let scope = layers.provider_scope(&draft.name);
    let mut spec = draft.spec();
    if let Some(existing) = layers.layer(scope).providers.get(&draft.name) {
        spec.context_window = existing.context_window;
        spec.max_output = existing.max_output;
        spec.vision = existing.vision;
        spec.tools = existing.tools.clone();
        spec.extra = existing.extra.clone();
    }
    layers
        .layer_mut(scope)
        .providers
        .insert(draft.name.clone(), spec);

    let merged = layers.merged();
    let profile = merged.default_profile();
    let needs_primary = merged
        .profiles
        .get(&profile)
        .is_none_or(|p| p.provider.is_none());
    let mut profile_scope = None;
    if needs_primary {
        let ps = layers.profile_scope(&profile);
        let entry = layers
            .layer_mut(ps)
            .profiles
            .entry(profile.clone())
            .or_default();
        entry.provider = Some(draft.name.clone());
        if let Some(model) = draft.model.clone().filter(|m| !m.trim().is_empty()) {
            entry.model = Some(model);
        }
        profile_scope = Some(ps);
    }

    layers.save(scope)?;
    if let Some(ps) = profile_scope
        && ps != scope
    {
        layers.save(ps)?;
    }
    Ok(scope)
}

pub fn update_provider(
    layers: &mut Layers,
    name: &str,
    edit: impl FnOnce(&mut ProviderSpec),
) -> Result<Scope> {
    let scope = layers.provider_scope(name);
    let mut spec = layers
        .merged()
        .providers
        .get(name)
        .cloned()
        .with_context(|| format!("no provider named '{name}'"))?;
    edit(&mut spec);
    spec.api_key = None;
    layers
        .layer_mut(scope)
        .providers
        .insert(name.to_string(), spec);
    layers.save(scope)?;
    Ok(scope)
}

fn unlink_provider(profile: &mut ProfileSpec, name: &str) {
    if let Some(fallback) = profile.fallback.as_mut() {
        fallback.retain(|f| f != name);
    }
    if profile.provider.as_deref() == Some(name) {
        let next = profile
            .fallback
            .as_mut()
            .and_then(|f| (!f.is_empty()).then(|| f.remove(0)));
        profile.provider = next;
        profile.model = None;
    }
    if profile.fallback.as_ref().is_some_and(|f| f.is_empty()) {
        profile.fallback = None;
    }
}

pub fn remove_provider(layers: &mut Layers, store: &KeyStore, name: &str) -> Result<bool> {
    if !layers.merged().providers.contains_key(name) {
        bail!("no provider named '{name}'");
    }
    for scope in Scope::ALL {
        let layer = layers.layer_mut(scope);
        layer.providers.remove(name);
        for profile in layer.profiles.values_mut() {
            unlink_provider(profile, name);
        }
        layer
            .profiles
            .retain(|_, p| p.provider.is_some() || p.model.is_some() || p.fallback.is_some());
    }
    for scope in Scope::ALL {
        if layers.path(scope).is_some_and(|p| p.exists()) {
            layers.save(scope)?;
        }
    }
    Ok(store.remove(name)?)
}

pub fn login(layers: &mut Layers, store: &KeyStore, name: &str, key: &str) -> Result<KeySource> {
    check_name(name)?;
    if !layers.merged().providers.contains_key(name) {
        let Some(kind) = catalog::kind(name) else {
            bail!(
                "no provider named '{name}'. Add it first with `zarn providers add`, or log in to one of: {}",
                catalog::KINDS
                    .iter()
                    .map(|k| k.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        };
        add_provider(
            layers,
            &ProviderDraft {
                name: name.to_string(),
                kind: kind.id.to_string(),
                model: kind.models.first().map(|m| m.to_string()),
                ..ProviderDraft::default()
            },
        )?;
    }
    Ok(store.save(name, key)?)
}

pub fn logout(store: &KeyStore, name: &str) -> Result<bool> {
    Ok(store.remove(name)?)
}

pub fn spec_with_key(cfg: &Config, store: &KeyStore, name: &str) -> Result<ProviderSpec> {
    let mut spec = cfg
        .providers
        .get(name)
        .cloned()
        .with_context(|| format!("no provider named '{name}'"))?;
    spec.api_key = cfg.resolve_key(store, name).map(|(key, _)| key);
    Ok(spec)
}

pub fn model_for(cfg: &Config, name: &str) -> Option<String> {
    let default = cfg.profiles.get(&cfg.default_profile());
    if let Some(profile) = default
        && profile.provider.as_deref() == Some(name)
        && let Some(model) = &profile.model
    {
        return Some(model.clone());
    }
    cfg.profiles
        .values()
        .find(|p| p.provider.as_deref() == Some(name) && p.model.is_some())
        .and_then(|p| p.model.clone())
        .or_else(|| {
            cfg.providers
                .get(name)
                .and_then(|spec| catalog::suggested_for(spec).first())
                .map(|m| m.to_string())
        })
}

pub async fn test_provider(spec: &ProviderSpec, model: &str) -> Result<String, String> {
    if catalog::needs_key(spec) && spec.api_key.as_deref().is_none_or(str::is_empty) {
        return Err("no API key is stored for this provider".into());
    }
    if model.trim().is_empty() {
        return Err("no model is set, so there is nothing to test against".into());
    }
    let provider = crate::guard::guarded_chain(std::slice::from_ref(spec))
        .map_err(|e| redact(&e.to_string()))?;
    let mut request = Request::new(model, vec![Message::user("Reply with the single word OK.")]);
    request.max_tokens = 16;

    let run = async {
        let mut stream = provider.stream(request).await.map_err(|e| e.to_string())?;
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            match event {
                Event::TextDelta(delta) => text.push_str(&delta),
                Event::Error(err) => return Err(err.to_string()),
                Event::Done(_) => break,
                _ => {}
            }
        }
        Ok(text)
    };

    match tokio::time::timeout(TEST_TIMEOUT, run).await {
        Err(_) => Err(format!(
            "no response within {} seconds",
            TEST_TIMEOUT.as_secs()
        )),
        Ok(Err(err)) => Err(redact(&err)),
        Ok(Ok(text)) => {
            let reply = text.trim();
            if reply.is_empty() {
                Ok(format!("OK: {model} answered"))
            } else {
                let short: String = reply.chars().take(40).collect();
                Ok(format!("OK: {model} replied \"{}\"", redact(&short)))
            }
        }
    }
}

pub async fn list_models(spec: &ProviderSpec) -> Vec<String> {
    let mut models: Vec<String> = catalog::suggested_for(spec)
        .iter()
        .map(|m| m.to_string())
        .collect();
    let usable = !catalog::needs_key(spec) || spec.api_key.as_deref().is_some_and(|k| !k.is_empty());
    if usable
        && let Ok(Ok(found)) =
            tokio::time::timeout(Duration::from_secs(10), zarn_providers::discovery::models("setup", spec)).await
    {
        for model in found {
            if !models.contains(&model.id) {
                models.push(model.id);
            }
        }
    }
    models
}

const NOT_CHAT: &[&str] = &[
    "embed", "whisper", "tts", "dall-e", "image", "moderation", "audio", "rerank", "guard", "transcribe",
    "speech", "realtime", "search-preview", "omni-moderation", "babbage", "davinci",
];

pub fn is_chat_model(id: &str) -> bool {
    let id = id.to_lowercase();
    !NOT_CHAT.iter().any(|word| id.contains(word))
}

pub fn chat_models(found: &[ModelInfo]) -> Vec<&ModelInfo> {
    found.iter().filter(|m| is_chat_model(&m.id)).collect()
}

pub fn pick_model(spec: &ProviderSpec, found: &[ModelInfo]) -> Option<String> {
    let suggested = catalog::suggested_for(spec);
    if let Some(model) = suggested.iter().find(|m| found.iter().any(|f| f.id == **m)) {
        return Some(model.to_string());
    }
    chat_models(found)
        .first()
        .map(|m| m.id.clone())
        .or_else(|| suggested.first().map(|m| m.to_string()))
}

fn rejected(err: &str) -> bool {
    err.contains("the key was rejected") || err.contains("not allowed")
}

pub async fn check_key(name: &str, spec: &ProviderSpec, hint: Option<&str>) -> Result<Vec<ModelInfo>, String> {
    let listed = tokio::time::timeout(TEST_TIMEOUT, zarn_providers::discovery::models(name, spec))
        .await
        .unwrap_or_else(|_| Err(format!("{name}: no answer within {} seconds", TEST_TIMEOUT.as_secs())));
    match listed {
        Ok(found) if !found.is_empty() => Ok(found),
        Err(err) if rejected(&err) => Err(redact(&err)),
        _ => {
            let model = hint
                .filter(|m| !m.is_empty())
                .map(str::to_string)
                .or_else(|| catalog::suggested_for(spec).first().map(|m| m.to_string()))
                .unwrap_or_default();
            if model.is_empty() {
                return Err(format!(
                    "{name}: could not list models with this key, and there is no known model to test it against"
                ));
            }
            test_provider(spec, &model).await.map(|_| {
                let mut known: Vec<ModelInfo> = catalog::suggested_for(spec)
                    .iter()
                    .map(|m| ModelInfo {
                        id: m.to_string(),
                        ..ModelInfo::default()
                    })
                    .collect();
                if !known.iter().any(|m| m.id == model) {
                    known.insert(
                        0,
                        ModelInfo {
                            id: model.clone(),
                            ..ModelInfo::default()
                        },
                    );
                }
                known
            })
        }
    }
}

pub fn cache_models(dir: &std::path::Path, name: &str, spec: &ProviderSpec, models: &[ModelInfo]) {
    let mut report = crate::quota::load_cached(dir, name).unwrap_or(crate::quota::Report {
        provider: name.to_string(),
        kind: spec.kind.clone(),
        fetched_at: 0,
        models: Ok(Vec::new()),
        limits: Err("not probed yet".into()),
        credits: Err("not probed yet".into()),
    });
    report.kind = spec.kind.clone();
    report.fetched_at = crate::quota::now();
    report.models = Ok(models.to_vec());
    let _ = crate::quota::save(dir, &report);
}

#[derive(Clone, Debug, PartialEq)]
pub struct Connected {
    pub provider: String,
    pub place: String,
    pub models: Vec<ModelInfo>,
    pub model: Option<String>,
    pub checked: bool,
}

impl Connected {
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("{}: key saved to the {}", self.provider, self.place)];
        if self.checked {
            parts.push(match self.models.len() {
                0 => "the key works".to_string(),
                1 => "1 model found".to_string(),
                n => format!("{n} models found"),
            });
        }
        if let Some(model) = &self.model {
            parts.push(format!("using {model}"));
        }
        parts.join(" · ")
    }
}

pub fn spec_for(cfg: &Config, provider: &str) -> Result<ProviderSpec> {
    if let Some(spec) = cfg.providers.get(provider) {
        return Ok(spec.clone());
    }
    let kind = catalog::kind(provider).with_context(|| format!("unknown provider '{provider}'"))?;
    Ok(ProviderDraft {
        name: kind.id.to_string(),
        kind: kind.id.to_string(),
        ..ProviderDraft::default()
    }
    .spec())
}

pub async fn connect(
    layers: &mut Layers,
    store: &KeyStore,
    cache: &std::path::Path,
    provider: &str,
    key: &str,
    check: bool,
) -> Result<Connected> {
    crate::auth::register_secret(key);
    let provider = catalog::kind(provider)
        .filter(|_| !layers.merged().providers.contains_key(provider))
        .map(|k| k.id.to_string())
        .unwrap_or_else(|| provider.to_string());
    let spec = spec_for(&layers.merged(), &provider)?;
    let mut probe = spec.clone();
    probe.api_key = Some(key.to_string());
    let models = if check {
        check_key(&provider, &probe, model_for(&layers.merged(), &provider).as_deref())
            .await
            .map_err(|err| anyhow!("the key was not saved: {}", redact(&err)))?
    } else {
        Vec::new()
    };
    let source = login(layers, store, &provider, key)?;
    let model = adopt_models(layers, cache, &provider, &spec, &models)?;
    Ok(Connected {
        provider,
        place: source.describe(store),
        models,
        model,
        checked: check,
    })
}

pub fn resolve_provider(cfg: &Config, provider: &str, key: &str) -> Result<String, String> {
    if !provider.is_empty() && provider != "auto" {
        return Ok(provider.to_string());
    }
    let configured = |kind: &catalog::Kind| {
        cfg.providers
            .iter()
            .find(|(_, spec)| catalog::for_spec(spec).is_some_and(|k| k.id == kind.id))
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| kind.id.to_string())
    };
    match catalog::detect(key) {
        catalog::Detected::One(kind) => Ok(configured(kind)),
        catalog::Detected::Several(kinds) => {
            let names: Vec<&str> = kinds.iter().map(|k| k.label).collect();
            Err(format!(
                "this key could belong to {}; pick the provider from the list so the key only goes to that one",
                names.join(", ")
            ))
        }
        catalog::Detected::Unknown => Err(
            "zarn cannot tell which provider this key is for from its format; pick the provider from the list".into(),
        ),
    }
}

pub fn adopt_models(
    layers: &mut Layers,
    cache: &std::path::Path,
    provider: &str,
    spec: &ProviderSpec,
    models: &[ModelInfo],
) -> Result<Option<String>> {
    if !models.is_empty() {
        cache_models(cache, provider, spec, models);
    }
    let cfg = layers.merged();
    let profile = cfg.default_profile();
    let current = cfg.profiles.get(&profile).cloned().unwrap_or_default();
    if current.provider.as_deref() != Some(provider) {
        return Ok(None);
    }
    let keep = current
        .model
        .clone()
        .filter(|m| models.is_empty() || models.iter().any(|f| f.id == *m));
    Ok(match keep {
        Some(m) => Some(m),
        None => match pick_model(spec, models) {
            Some(pick) => {
                set_profile_model(layers, &profile, &pick)?;
                Some(pick)
            }
            None => current.model,
        },
    })
}

pub fn use_model(layers: &mut Layers, provider: &str, model: &str) -> Result<String> {
    let cfg = layers.merged();
    if !cfg.providers.contains_key(provider) {
        bail!("no provider named '{provider}'");
    }
    let profile = cfg.default_profile();
    if cfg.profiles.get(&profile).and_then(|p| p.provider.as_deref()) != Some(provider) {
        set_profile_provider(layers, &profile, provider)?;
    }
    set_profile_model(layers, &profile, model)?;
    Ok(profile)
}

pub async fn model_choices(name: &str, spec: &ProviderSpec) -> Vec<(String, Option<u32>)> {
    let dir = crate::quota::cache_dir();
    let mut found: Vec<(String, Option<u32>)> = crate::quota::cached_models(&dir, name)
        .into_iter()
        .map(|m| (m.id, m.context_window))
        .collect();
    let usable = !catalog::needs_key(spec) || spec.api_key.as_deref().is_some_and(|k| !k.is_empty());
    if found.is_empty()
        && usable
        && let Ok(Ok(listed)) =
            tokio::time::timeout(Duration::from_secs(8), zarn_providers::discovery::models(name, spec)).await
    {
        found = listed.into_iter().map(|m| (m.id, m.context_window)).collect();
    }
    for model in catalog::suggested_models(&spec.kind).iter().rev() {
        if !found.iter().any(|(id, _)| id == model) {
            found.insert(0, (model.to_string(), None));
        }
    }
    found
}

pub async fn test_named(layers: &Layers, store: &KeyStore, name: &str) -> Result<String, String> {
    let cfg = layers.merged();
    let spec = spec_with_key(&cfg, store, name).map_err(|e| e.to_string())?;
    let model = model_for(&cfg, name).unwrap_or_default();
    let started = std::time::Instant::now();
    let result = test_provider(&spec, &model).await;
    crate::health::record_test(name, started.elapsed(), result.is_ok());
    result
}

fn save_profile(layers: &mut Layers, profile: &str, edit: impl FnOnce(&mut ProfileSpec)) -> Result<()> {
    let scope = layers.profile_scope(profile);
    let mut spec = layers
        .layer(scope)
        .profiles
        .get(profile)
        .cloned()
        .unwrap_or_default();
    edit(&mut spec);
    layers
        .layer_mut(scope)
        .profiles
        .insert(profile.to_string(), spec);
    layers.save(scope)?;
    Ok(())
}

pub fn set_profile_model(layers: &mut Layers, profile: &str, model: &str) -> Result<()> {
    let model = model.trim();
    if model.is_empty() {
        bail!("the model name is empty");
    }
    save_profile(layers, profile, |p| p.model = Some(model.to_string()))
}

pub fn set_profile_provider(layers: &mut Layers, profile: &str, provider: &str) -> Result<()> {
    if !layers.merged().providers.contains_key(provider) {
        bail!("no provider named '{provider}'");
    }
    save_profile(layers, profile, |p| {
        if let Some(fallback) = p.fallback.as_mut() {
            fallback.retain(|f| f != provider);
        }
        p.provider = Some(provider.to_string());
    })
}

pub fn set_fallback(layers: &mut Layers, profile: &str, order: Vec<String>) -> Result<()> {
    let cfg = layers.merged();
    for name in &order {
        if !cfg.providers.contains_key(name) {
            bail!("no provider named '{name}'");
        }
    }
    save_profile(layers, profile, |p| {
        p.fallback = (!order.is_empty()).then_some(order);
    })
}

pub fn switch_provider(layers: &mut Layers, provider: &str) -> Result<String> {
    let cfg = layers.merged();
    let profile = cfg.default_profile();
    let current = cfg.profiles.get(&profile).and_then(|p| p.provider.clone());
    set_profile_provider(layers, &profile, provider)?;
    if current.as_deref() != Some(provider) {
        let model = model_for(&cfg, provider).or_else(|| {
            let spec = cfg.providers.get(provider)?;
            pick_model(spec, &crate::quota::cached_models(&crate::quota::cache_dir(), provider))
        });
        match model {
            Some(model) => set_profile_model(layers, &profile, &model)?,
            None => save_profile(layers, &profile, |p| p.model = None)?,
        }
    }
    Ok(profile)
}

pub fn set_default_profile(layers: &mut Layers, profile: &str) -> Result<()> {
    if !layers.merged().profiles.contains_key(profile) {
        bail!("no profile named '{profile}'");
    }
    set_general(layers, GeneralField::DefaultProfile, |g| {
        g.default_profile = Some(profile.to_string())
    })
}

pub fn add_profile(layers: &mut Layers, profile: &str) -> Result<()> {
    check_name(profile)?;
    if layers.merged().profiles.contains_key(profile) {
        bail!("profile '{profile}' already exists");
    }
    let cfg = layers.merged();
    let template = cfg.profiles.get(&cfg.default_profile()).cloned().unwrap_or_default();
    layers
        .user
        .profiles
        .insert(profile.to_string(), template);
    layers.save(Scope::User)?;
    Ok(())
}

pub fn add_permission_rule(
    layers: &mut Layers,
    scope: Scope,
    policy: Policy,
    rule: &str,
) -> Result<()> {
    zarn_tools::permissions::parse_rule(rule).map_err(anyhow::Error::msg)?;
    let rules = &mut layers.layer_mut(scope).permissions.rules;
    let list = match policy {
        Policy::Allow => &mut rules.allow,
        Policy::Ask => &mut rules.ask,
        Policy::Deny => &mut rules.deny,
    };
    if !list.iter().any(|r| r == rule) {
        list.push(rule.to_string());
    }
    layers.save(scope)?;
    Ok(())
}

pub fn remove_permission_rule(layers: &mut Layers, policy: Policy, rule: &str) -> Result<()> {
    let mut found = false;
    for scope in Scope::ALL {
        let rules = &mut layers.layer_mut(scope).permissions.rules;
        let list = match policy {
            Policy::Allow => &mut rules.allow,
            Policy::Ask => &mut rules.ask,
            Policy::Deny => &mut rules.deny,
        };
        let before = list.len();
        list.retain(|r| r != rule);
        if list.len() != before {
            found = true;
            layers.save(scope)?;
        }
    }
    if !found {
        bail!("no rule {rule}");
    }
    Ok(())
}

pub fn rule_scope(layers: &Layers, rule: &str) -> Scope {
    layers.find(|c| c.permissions.rules.list().iter().any(|(_, r)| r == rule))
}

pub fn set_permission(layers: &mut Layers, tool: &str, policy: Policy) -> Result<Scope> {
    let scope = layers.permission_scope(tool);
    layers
        .layer_mut(scope)
        .permissions
        .insert(tool.to_string(), policy);
    layers.save(scope)?;
    Ok(scope)
}

pub fn set_general(
    layers: &mut Layers,
    field: GeneralField,
    edit: impl FnOnce(&mut crate::General),
) -> Result<()> {
    let scope = layers.general_scope(field);
    edit(&mut layers.layer_mut(scope).general);
    layers.save(scope)?;
    Ok(())
}

pub fn set_spend_cap(layers: &mut Layers, day: bool, usd: Option<f64>) -> Result<Scope> {
    if let Some(v) = usd
        && (!v.is_finite() || v <= 0.0)
    {
        bail!("a spend cap must be a positive number of dollars, or off");
    }
    let scope = layers.limits_scope(day);
    let limits = &mut layers.layer_mut(scope).limits;
    if day {
        limits.day_usd = usd;
    } else {
        limits.session_usd = usd;
    }
    layers.save(scope)?;
    Ok(scope)
}

pub fn parse_cap(text: &str) -> Result<Option<f64>> {
    let text = text.trim().trim_start_matches('$');
    match text {
        "off" | "none" | "" => Ok(None),
        n => n
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v > 0.0)
            .map(Some)
            .ok_or_else(|| anyhow!("'{n}' is not a positive dollar amount; use a number like 5 or 2.50, or off")),
    }
}

pub fn set_routing_enabled(layers: &mut Layers, enabled: bool) -> Result<Scope> {
    let scope = layers.routing_scope();
    layers.layer_mut(scope).routing.enabled = Some(enabled);
    layers.save(scope)?;
    Ok(scope)
}

pub fn set_role(
    layers: &mut Layers,
    role: &str,
    provider: Option<&str>,
    model: Option<&str>,
) -> Result<Scope> {
    if !crate::ROLES.contains(&role) {
        bail!("'{role}' is not a routing role; use one of {}", crate::ROLES.join(", "));
    }
    if let Some(provider) = provider
        && !layers.merged().providers.contains_key(provider)
    {
        bail!("no provider named '{provider}'");
    }
    let scope = layers.role_scope(role);
    let entry = layers
        .layer_mut(scope)
        .routing
        .roles
        .entry(role.to_string())
        .or_default();
    entry.provider = provider.map(str::to_string);
    entry.model = model.map(str::trim).filter(|m| !m.is_empty()).map(str::to_string);
    layers.save(scope)?;
    Ok(scope)
}

pub fn clear_role(layers: &mut Layers, role: &str) -> Result<()> {
    let mut found = false;
    for scope in Scope::ALL {
        if layers.layer_mut(scope).routing.roles.remove(role).is_some() {
            found = true;
            layers.save(scope)?;
        }
    }
    if !found {
        bail!("routing role '{role}' is not set");
    }
    Ok(())
}

pub fn role_label(cfg: &Config, role: &str) -> String {
    match cfg.routing.roles.get(role) {
        Some(spec) if spec.provider.is_some() || spec.model.is_some() => format!(
            "{} · {}",
            spec.provider.as_deref().unwrap_or("main provider"),
            spec.model.as_deref().unwrap_or("its default model")
        ),
        _ => "main model".to_string(),
    }
}

pub fn set_skill_enabled(layers: &mut Layers, name: &str, enabled: bool) -> Result<Scope> {
    let scope = layers.skill_scope(name);
    let layer = layers.layer_mut(scope);
    if enabled {
        layer.skills.remove(name);
        if scope == Scope::Project && layers.user.skills.get(name) == Some(&false) {
            layers.project.skills.insert(name.to_string(), true);
        }
    } else {
        layer.skills.insert(name.to_string(), false);
    }
    layers.save(scope)?;
    Ok(scope)
}

pub fn mcp_add(
    layers: &mut Layers,
    name: &str,
    server: ServerConfig,
    scope: Option<Scope>,
) -> Result<Scope> {
    check_name(name)?;
    if server.command.is_none() && server.url.is_none() {
        bail!("an MCP server needs a command or a url");
    }
    let scope = scope.unwrap_or_else(|| layers.mcp_scope(name));
    let previous = layers.layer_mut(scope).mcp.insert(name.to_string(), server);
    if let Err(err) = layers.save(scope) {
        let layer = &mut layers.layer_mut(scope).mcp;
        match previous {
            Some(old) => {
                layer.insert(name.to_string(), old);
            }
            None => {
                layer.remove(name);
            }
        }
        return Err(err);
    }
    Ok(scope)
}

pub fn mcp_remove(layers: &mut Layers, name: &str) -> Result<()> {
    let mut found = false;
    for scope in Scope::ALL {
        if layers.layer_mut(scope).mcp.remove(name).is_some() {
            found = true;
            layers.save(scope)?;
        }
    }
    if !found {
        return Err(anyhow!("no MCP server named '{name}'"));
    }
    Ok(())
}

pub fn parse_server(words: &[String]) -> Result<ServerConfig> {
    let Some(first) = words.first() else {
        bail!("give a command to run, or a url");
    };
    if first.starts_with("http://") || first.starts_with("https://") {
        return Ok(ServerConfig {
            url: Some(first.clone()),
            ..ServerConfig::default()
        });
    }
    Ok(ServerConfig {
        command: Some(first.clone()),
        args: words[1..].to_vec(),
        ..ServerConfig::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::MemoryBackend;
    use std::sync::Arc;

    fn fixture() -> (tempfile::TempDir, Layers, KeyStore) {
        let dir = tempfile::tempdir().unwrap();
        let layers =
            Layers::load_from(dir.path().join("p/config.toml"), Some(dir.path().join("u/config.toml")))
                .unwrap();
        let store = KeyStore::new(Arc::new(MemoryBackend::new()), dir.path().join("u/auth.json"));
        (dir, layers, store)
    }

    fn draft(name: &str, kind: &str) -> ProviderDraft {
        ProviderDraft {
            name: name.into(),
            kind: kind.into(),
            model: Some("m1".into()),
            ..ProviderDraft::default()
        }
    }

    #[test]
    fn the_first_provider_becomes_the_default_profile() {
        let (_d, mut layers, store) = fixture();
        assert_eq!(add_provider(&mut layers, &draft("a", "openai")).unwrap(), Scope::User);
        let cfg = layers.merged();
        assert_eq!(cfg.profiles["default"].provider.as_deref(), Some("a"));
        assert_eq!(cfg.profiles["default"].model.as_deref(), Some("m1"));
        assert_eq!(
            cfg.providers["a"].base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        let rows = providers(&layers, &store);
        assert!(rows[0].is_default);
        assert_eq!(rows[0].key_label(), "NO KEY");
    }

    #[test]
    fn a_second_provider_does_not_steal_the_default() {
        let (_d, mut layers, _store) = fixture();
        add_provider(&mut layers, &draft("a", "openai")).unwrap();
        add_provider(&mut layers, &draft("b", "anthropic")).unwrap();
        assert_eq!(
            layers.merged().profiles["default"].provider.as_deref(),
            Some("a")
        );
    }

    #[test]
    fn removing_the_primary_promotes_the_first_fallback_and_drops_the_key() {
        let (_d, mut layers, store) = fixture();
        add_provider(&mut layers, &draft("a", "openai")).unwrap();
        add_provider(&mut layers, &draft("b", "anthropic")).unwrap();
        set_fallback(&mut layers, "default", vec!["b".into()]).unwrap();
        store.save("a", "test-key-not-real").unwrap();

        remove_provider(&mut layers, &store, "a").unwrap();
        let cfg = layers.merged();
        assert!(!cfg.providers.contains_key("a"));
        assert_eq!(cfg.profiles["default"].provider.as_deref(), Some("b"));
        assert!(cfg.profiles["default"].fallback.is_none());
        assert!(store.resolve("a", None).is_none());
    }

    #[test]
    fn login_to_a_known_kind_creates_the_provider() {
        let (_d, mut layers, store) = fixture();
        let source = login(&mut layers, &store, "anthropic", "test-key-not-real").unwrap();
        assert_eq!(source, KeySource::Store);
        assert_eq!(layers.merged().providers["anthropic"].kind, "anthropic");
        assert!(login(&mut layers, &store, "mystery", "test-key-not-real").is_err());
    }

    #[test]
    fn a_project_permission_is_edited_in_the_project_file() {
        let (d, mut layers, _store) = fixture();
        layers.project.permissions.insert("bash".into(), Policy::Deny);
        layers.save(Scope::Project).unwrap();
        assert_eq!(set_permission(&mut layers, "bash", Policy::Ask).unwrap(), Scope::Project);
        assert_eq!(set_permission(&mut layers, "read", Policy::Allow).unwrap(), Scope::User);
        let project = std::fs::read_to_string(d.path().join("p/config.toml")).unwrap();
        assert!(project.contains("bash = \"ask\""));
        assert!(!project.contains("read"));
    }

    #[test]
    fn disabling_and_enabling_a_skill_persists() {
        let (_d, mut layers, _store) = fixture();
        set_skill_enabled(&mut layers, "pdf", false).unwrap();
        assert!(!layers.merged().skill_enabled("pdf"));
        set_skill_enabled(&mut layers, "pdf", true).unwrap();
        assert!(layers.merged().skill_enabled("pdf"));
    }

    #[test]
    fn a_server_line_is_a_command_or_a_url() {
        let stdio = parse_server(&["npx".into(), "-y".into(), "srv".into()]).unwrap();
        assert_eq!(stdio.command.as_deref(), Some("npx"));
        assert_eq!(stdio.args, vec!["-y", "srv"]);
        let http = parse_server(&["https://example.com/mcp".into()]).unwrap();
        assert_eq!(http.url.as_deref(), Some("https://example.com/mcp"));
    }

    #[test]
    fn routing_edits_round_trip_and_validate() {
        let (_d, mut layers, _store) = fixture();
        add_provider(&mut layers, &draft("a", "openai")).unwrap();
        set_routing_enabled(&mut layers, true).unwrap();
        set_role(&mut layers, "compact", Some("a"), Some("small")).unwrap();
        let cfg = layers.merged();
        assert!(cfg.routing.enabled());
        assert_eq!(cfg.routing.roles["compact"].model.as_deref(), Some("small"));
        assert_eq!(role_label(&cfg, "compact"), "a · small");
        assert_eq!(role_label(&cfg, "title"), "main model");
        assert!(set_role(&mut layers, "compact", Some("ghost"), None).is_err());
        assert!(set_role(&mut layers, "nonsense", Some("a"), None).is_err());
        clear_role(&mut layers, "compact").unwrap();
        assert!(!layers.merged().routing.roles.contains_key("compact"));
    }

    #[test]
    fn a_role_naming_a_missing_provider_is_rejected_naming_both() {
        let cfg: Config = toml::from_str(
            "[routing]\nenabled = true\n\n[routing.roles.explore]\nprovider = \"groq\"\nmodel = \"llama\"\n",
        )
        .unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("explore") && err.contains("groq"), "{err}");
    }

    #[test]
    fn routing_merges_across_layers_with_the_project_winning() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("p.toml");
        let u = dir.path().join("u.toml");
        std::fs::write(&p, "[providers.a]\nkind = \"openai\"\n\n[routing.roles.compact]\nmodel = \"project-small\"\n").unwrap();
        std::fs::write(&u, "[providers.b]\nkind = \"openai\"\n\n[routing]\nenabled = true\n\n[routing.roles.compact]\nprovider = \"b\"\nmodel = \"user-small\"\n\n[routing.roles.title]\nprovider = \"a\"\n").unwrap();
        let cfg = Layers::load_from(p, Some(u)).unwrap().merged();
        assert!(cfg.routing.enabled());
        assert_eq!(cfg.routing.roles["compact"].model.as_deref(), Some("project-small"));
        assert_eq!(cfg.routing.roles["compact"].provider.as_deref(), Some("b"));
        assert_eq!(cfg.routing.roles["title"].provider.as_deref(), Some("a"));
    }

    #[test]
    fn rules_are_saved_to_the_chosen_scope_and_removed_from_wherever_they_live() {
        let (d, mut layers, _store) = fixture();
        add_permission_rule(&mut layers, Scope::Project, Policy::Allow, "bash(git diff:*)").unwrap();
        add_permission_rule(&mut layers, Scope::User, Policy::Deny, "bash(rm -rf:*)").unwrap();
        let project = std::fs::read_to_string(d.path().join("p/config.toml")).unwrap();
        assert!(project.contains("allow = [\"bash(git diff:*)\"]"), "{project}");
        assert_eq!(rule_scope(&layers, "bash(rm -rf:*)"), Scope::User);
        assert!(add_permission_rule(&mut layers, Scope::User, Policy::Allow, "bash(oops").is_err());
        remove_permission_rule(&mut layers, Policy::Allow, "bash(git diff:*)").unwrap();
        assert!(layers.merged().permissions.rules.allow.is_empty());
        assert!(remove_permission_rule(&mut layers, Policy::Allow, "bash(git diff:*)").is_err());
    }

    #[tokio::test]
    async fn testing_a_provider_without_a_key_says_so_without_a_request() {
        let spec = ProviderSpec {
            kind: "anthropic".into(),
            ..ProviderSpec::default()
        };
        let err = test_provider(&spec, "m").await.unwrap_err();
        assert!(err.contains("no API key"));
    }
}
