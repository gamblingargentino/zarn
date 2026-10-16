use crate::auth::{KeyStore, redact};
use crate::{Config, catalog, ops};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zarn_providers::ProviderSpec;
use zarn_providers::discovery::{self, Credits, Limits, ModelInfo, Window};

pub const DEFAULT_TTL_MINUTES: u32 = 10;
pub const PROVIDER_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub provider: String,
    pub kind: String,
    pub fetched_at: u64,
    pub models: Result<Vec<ModelInfo>, String>,
    pub limits: Result<Limits, String>,
    pub credits: Result<Credits, String>,
}

impl Report {
    pub fn failed(&self) -> Option<&str> {
        match (&self.models, &self.limits) {
            (Err(e), Err(_)) => Some(e),
            _ => None,
        }
    }

    pub fn models(&self) -> &[ModelInfo] {
        self.models.as_deref().unwrap_or(&[])
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn cache_dir() -> PathBuf {
    crate::paths::data_dir().join("discovery")
}

fn cache_path(dir: &Path, provider: &str) -> PathBuf {
    dir.join(format!("{provider}.json"))
}

pub fn ttl(cfg: &Config) -> Duration {
    Duration::from_secs(
        u64::from(cfg.general.discovery_cache_minutes.unwrap_or(DEFAULT_TTL_MINUTES)) * 60,
    )
}

pub fn load_cached(dir: &Path, provider: &str) -> Option<Report> {
    let text = std::fs::read_to_string(cache_path(dir, provider)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn fresh(report: &Report, ttl: Duration) -> bool {
    now().saturating_sub(report.fetched_at) < ttl.as_secs()
}

pub fn save(dir: &Path, report: &Report) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = cache_path(dir, &report.provider);
    let body = redact(&serde_json::to_string_pretty(report).map_err(std::io::Error::other)?);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

fn clean<T>(result: Result<T, String>) -> Result<T, String> {
    result.map_err(|e| redact(&e))
}

pub async fn fetch(name: &str, spec: &ProviderSpec, model: Option<&str>) -> Report {
    let limits = async {
        match model {
            Some(model) => discovery::limits(name, spec, model).await,
            None => Err(format!("{name}: no model set, so there is nothing to probe")),
        }
    };
    let (models, limits, credits) = tokio::join!(
        discovery::models(name, spec),
        limits,
        discovery::credits(name, spec)
    );
    Report {
        provider: name.to_string(),
        kind: spec.kind.clone(),
        fetched_at: now(),
        models: clean(models),
        limits: clean(limits),
        credits: clean(credits),
    }
}

pub async fn report(
    name: &str,
    spec: &ProviderSpec,
    model: Option<&str>,
    refresh: bool,
    ttl: Duration,
    dir: &Path,
    timeout: Duration,
) -> Report {
    if !refresh
        && let Some(cached) = load_cached(dir, name)
        && fresh(&cached, ttl)
    {
        return cached;
    }
    let fetched = match tokio::time::timeout(timeout, fetch(name, spec, model)).await {
        Ok(report) => report,
        Err(_) => {
            let why = format!("{name}: no answer within {:.1} seconds", timeout.as_secs_f32());
            Report {
                provider: name.to_string(),
                kind: spec.kind.clone(),
                fetched_at: now(),
                models: Err(why.clone()),
                limits: Err(why.clone()),
                credits: Err(why),
            }
        }
    };
    if fetched.failed().is_none()
        && let Err(err) = save(dir, &fetched)
    {
        tracing::warn!(error = %err, "discovery cache not written");
    }
    fetched
}

pub fn eligible(cfg: &Config, store: &KeyStore) -> Vec<(String, ProviderSpec)> {
    cfg.providers
        .keys()
        .filter_map(|name| {
            let spec = ops::spec_with_key(cfg, store, name).ok()?;
            let has_key = spec.api_key.as_deref().is_some_and(|k| !k.is_empty());
            (has_key || !catalog::needs_key(&spec)).then(|| (name.clone(), spec))
        })
        .collect()
}

pub async fn reports(
    cfg: &Config,
    store: &KeyStore,
    only: Option<&str>,
    refresh: bool,
    dir: &Path,
) -> Vec<Report> {
    reports_within(cfg, store, only, refresh, dir, PROVIDER_TIMEOUT).await
}

pub async fn reports_within(
    cfg: &Config,
    store: &KeyStore,
    only: Option<&str>,
    refresh: bool,
    dir: &Path,
    timeout: Duration,
) -> Vec<Report> {
    let ttl = ttl(cfg);
    let targets: Vec<(String, ProviderSpec)> = eligible(cfg, store)
        .into_iter()
        .filter(|(name, _)| only.is_none_or(|o| o == name))
        .collect();
    let jobs = targets.iter().map(|(name, spec)| {
        let model = ops::model_for(cfg, name);
        async move { report(name, spec, model.as_deref(), refresh, ttl, dir, timeout).await }
    });
    futures::future::join_all(jobs).await
}

pub fn cached_models(dir: &Path, provider: &str) -> Vec<ModelInfo> {
    load_cached(dir, provider)
        .map(|r| r.models().to_vec())
        .unwrap_or_default()
}

pub fn context_label(window: Option<u32>) -> String {
    match window {
        Some(n) if n >= 1_000_000 && n % 1_000_000 == 0 => format!("{}M ctx", n / 1_000_000),
        Some(n) if n >= 1_000 => format!("{}k ctx", n / 1_000),
        Some(n) => format!("{n} ctx"),
        None => "ctx unknown".to_string(),
    }
}

fn window(label: &str, w: &Window) -> String {
    let value = |v: &Option<String>| v.clone().unwrap_or_else(|| "unknown".into());
    let mut text = format!("{label} {}/{}", value(&w.remaining), value(&w.limit));
    if let Some(reset) = &w.reset {
        text.push_str(&format!(" (resets {reset})"));
    }
    text
}

pub fn limits_line(limits: &Result<Limits, String>) -> String {
    match limits {
        Ok(Limits::NotExposed) => "not exposed by this provider".into(),
        Ok(Limits::Found { requests, tokens }) => format!(
            "{} · {}",
            window("requests", requests),
            window("tokens", tokens)
        ),
        Err(err) => format!("error: {err}"),
    }
}

pub fn credits_line(credits: &Result<Credits, String>) -> String {
    let money = |v: Option<f64>, c: &Option<String>| match v {
        Some(v) => format!("{v:.2} {}", c.clone().unwrap_or_default()).trim().to_string(),
        None => "unknown".into(),
    };
    match credits {
        Ok(Credits::NotExposed) => "not exposed by this provider".into(),
        Ok(Credits::Found {
            balance,
            limit,
            used,
            currency,
        }) => format!(
            "balance {} · limit {} · used {}",
            money(*balance, currency),
            money(*limit, currency),
            money(*used, currency)
        ),
        Err(err) => format!("error: {err}"),
    }
}

pub fn age(fetched_at: u64) -> String {
    let secs = now().saturating_sub(fetched_at);
    match secs {
        0..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        _ => format!("{}h ago", secs / 3600),
    }
}

pub fn models_line(report: &Report, max: usize) -> String {
    match &report.models {
        Err(err) => format!("error: {err}"),
        Ok(models) if models.is_empty() => "none listed".into(),
        Ok(models) => {
            let mut shown: Vec<String> = models
                .iter()
                .take(max)
                .map(|m| match m.context_window {
                    Some(_) => format!("{} ({})", m.id, context_label(m.context_window)),
                    None => m.id.clone(),
                })
                .collect();
            if models.len() > max {
                shown.push(format!("… {} more", models.len() - max));
            }
            shown.join(", ")
        }
    }
}

pub fn render(reports: &[Report]) -> String {
    if reports.is_empty() {
        return "no provider with a key to query; add one with /login or zarn providers add".into();
    }
    reports
        .iter()
        .map(|r| {
            if let Some(err) = r.failed() {
                return format!("{} ({})  FAILED: {err}", r.provider, r.kind);
            }
            [
                format!("{} ({}) · fetched {}", r.provider, r.kind, age(r.fetched_at)),
                format!("  models   {}", models_line(r, 12)),
                format!("  limits   {}", limits_line(&r.limits)),
                format!("  credits  {}", credits_line(&r.credits)),
            ]
            .join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str, fetched_at: u64) -> Report {
        Report {
            provider: name.into(),
            kind: "openai".into(),
            fetched_at,
            models: Ok(vec![ModelInfo {
                id: "m".into(),
                context_window: Some(128_000),
                ..ModelInfo::default()
            }]),
            limits: Ok(Limits::NotExposed),
            credits: Ok(Credits::NotExposed),
        }
    }

    #[test]
    fn a_cached_report_is_reused_until_it_expires() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &sample("a", now())).unwrap();
        let cached = load_cached(dir.path(), "a").unwrap();
        assert!(fresh(&cached, Duration::from_secs(600)));
        save(dir.path(), &sample("a", now() - 3600)).unwrap();
        assert!(!fresh(&load_cached(dir.path(), "a").unwrap(), Duration::from_secs(600)));
    }

    #[test]
    fn rendering_says_not_exposed_and_never_guesses() {
        let text = render(&[sample("a", now())]);
        assert!(text.contains("m (128k ctx)"));
        assert!(text.contains("limits   not exposed by this provider"));
        assert!(text.contains("credits  not exposed by this provider"));
        assert_eq!(context_label(None), "ctx unknown");
        assert_eq!(context_label(Some(1_000_000)), "1M ctx");
    }
}
