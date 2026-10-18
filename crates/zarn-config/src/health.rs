use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use zarn_core::provider::{Caps, Event, Provider, ProviderError};
use zarn_core::types::{Message, Request};

#[derive(Clone, Debug, Default)]
pub struct Health {
    pub last_test: Option<(bool, Instant)>,
    pub latency: Option<Duration>,
    pub failures: u32,
    pub requests: u32,
}

fn registry() -> &'static Mutex<HashMap<String, Health>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Health>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn update(name: &str, edit: impl FnOnce(&mut Health)) {
    if let Ok(mut map) = registry().lock() {
        edit(map.entry(name.to_string()).or_default());
    }
}

pub fn record_request(name: &str, latency: Duration, ok: bool) {
    update(name, |h| {
        h.requests += 1;
        if ok {
            h.latency = Some(latency);
        } else {
            h.failures += 1;
        }
    });
}

pub fn record_test(name: &str, latency: Duration, ok: bool) {
    update(name, |h| {
        h.last_test = Some((ok, Instant::now()));
        if ok {
            h.latency = Some(latency);
        } else {
            h.failures += 1;
        }
    });
}

pub fn get(name: &str) -> Option<Health> {
    registry().lock().ok()?.get(name).cloned()
}

fn ago(at: Instant) -> String {
    let secs = at.elapsed().as_secs();
    match secs {
        0..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        _ => format!("{}h ago", secs / 3600),
    }
}

pub fn summary(name: &str) -> String {
    let Some(h) = get(name) else {
        return "not used yet".into();
    };
    let test = match h.last_test {
        Some((true, at)) => format!("test ok {}", ago(at)),
        Some((false, at)) => format!("test failed {}", ago(at)),
        None => "not tested".into(),
    };
    let latency = h
        .latency
        .map(|d| format!("{} ms", d.as_millis()))
        .unwrap_or_else(|| "no latency yet".into());
    let failures = match h.failures {
        1 => "1 failure since start".to_string(),
        n => format!("{n} failures since start"),
    };
    format!("{test} · {latency} · {failures}")
}

pub struct Measured {
    name: String,
    inner: Arc<dyn Provider>,
}

impl Measured {
    pub fn wrap(name: impl Into<String>, inner: Arc<dyn Provider>) -> Arc<dyn Provider> {
        Arc::new(Measured {
            name: name.into(),
            inner,
        })
    }
}

#[async_trait]
impl Provider for Measured {
    async fn stream(&self, req: Request) -> Result<BoxStream<'static, Event>, ProviderError> {
        let started = Instant::now();
        let stream = match self.inner.stream(req).await {
            Ok(stream) => stream,
            Err(err) => {
                record_request(&self.name, started.elapsed(), false);
                return Err(err);
            }
        };
        let (first, rest) = stream.into_future().await;
        let ok = !matches!(first, Some(Event::Error(_)));
        record_request(&self.name, started.elapsed(), ok);
        Ok(futures::stream::iter(first).chain(rest).boxed())
    }

    fn caps(&self) -> Caps {
        self.inner.caps()
    }

    fn count_tokens(&self, msgs: &[Message]) -> Result<u32, ProviderError> {
        self.inner.count_tokens(msgs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_shows_test_age_latency_and_failures() {
        assert_eq!(summary("health-never-used"), "not used yet");
        record_request("health-a", Duration::from_millis(420), true);
        record_request("health-a", Duration::from_millis(900), false);
        record_test("health-a", Duration::from_millis(300), true);
        assert_eq!(summary("health-a"), "test ok 0s ago · 300 ms · 1 failure since start");
    }
}
