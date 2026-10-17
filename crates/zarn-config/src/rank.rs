use futures::StreamExt;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zarn_core::provider::{Event, Provider};
use zarn_core::types::{Message, Request, ToolSpec};

pub const TIMEOUT: Duration = Duration::from_secs(45);
pub const MAX_CANDIDATES: usize = 12;

#[derive(Clone, Debug, PartialEq)]
pub struct Probe {
    pub label: String,
    pub answered: bool,
    pub tools: bool,
    pub first: Option<Duration>,
    pub total: Duration,
    pub error: Option<String>,
    pub context: Option<u32>,
}

impl Probe {
    pub fn score(&self) -> u32 {
        if !self.answered {
            return 0;
        }
        let speed = match self.first.unwrap_or(self.total).as_millis() {
            0..=800 => 30,
            801..=2000 => 22,
            2001..=5000 => 12,
            5001..=15000 => 5,
            _ => 1,
        };
        let room = match self.context.unwrap_or(0) {
            0 => 5,
            1..=32_000 => 2,
            32_001..=128_000 => 8,
            _ => 10,
        };
        40 + if self.tools { 20 } else { 0 } + speed + room
    }

    pub fn verdict(&self) -> String {
        match (&self.error, self.answered, self.tools) {
            (Some(err), _, _) => err.clone(),
            (None, true, true) => "works, calls tools".into(),
            (None, true, false) => "answers, but did not call the tool".into(),
            (None, false, _) => "returned nothing".into(),
        }
    }
}

fn add_tool() -> ToolSpec {
    ToolSpec {
        name: "add".into(),
        description: "Add two numbers and return the sum.".into(),
        schema: json!({
            "type": "object",
            "properties": { "a": { "type": "number" }, "b": { "type": "number" } },
            "required": ["a", "b"]
        }),
    }
}

pub fn request(model: &str) -> Request {
    let mut req = Request::new(
        model,
        vec![
            Message::System("You are being checked for tool use. Answer by calling a tool, not with text.".into()),
            Message::user("Use the add tool to add 2 and 3."),
        ],
    );
    req.tools = vec![add_tool()];
    req.max_tokens = 256;
    req.temperature = 0.0;
    req
}

fn called_add(name: &str, args: &str) -> bool {
    if name != "add" {
        return false;
    }
    let Ok(value) = serde_json::from_str::<Value>(args.trim()) else {
        return false;
    };
    let num = |k: &str| value.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str()?.trim().parse().ok()));
    matches!((num("a"), num("b")), (Some(a), Some(b)) if (a + b - 5.0).abs() < 1e-9)
}

pub async fn probe(label: String, provider: Arc<dyn Provider>, model: &str, context: Option<u32>) -> Probe {
    let started = Instant::now();
    let mut out = Probe {
        label,
        answered: false,
        tools: false,
        first: None,
        total: Duration::ZERO,
        error: None,
        context,
    };
    let run = async {
        let mut stream = provider.stream(request(model)).await.map_err(|e| e.to_string())?;
        let mut name = String::new();
        let mut args = String::new();
        while let Some(event) = stream.next().await {
            if out.first.is_none() && matches!(event, Event::TextDelta(_) | Event::ToolCallStart { .. }) {
                out.first = Some(started.elapsed());
            }
            match event {
                Event::TextDelta(text) if !text.trim().is_empty() => out.answered = true,
                Event::ToolCallStart { name: called, .. } => {
                    out.answered = true;
                    name = called;
                    args.clear();
                }
                Event::ToolCallDelta(delta) => args.push_str(&delta),
                Event::ToolCallEnd => out.tools |= called_add(&name, &args),
                Event::Error(err) => return Err(err.to_string()),
                Event::Done(_) => break,
                _ => {}
            }
        }
        out.tools |= called_add(&name, &args);
        Ok(())
    };
    match tokio::time::timeout(TIMEOUT, run).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => out.error = Some(crate::auth::redact(&shorten(&err))),
        Err(_) => out.error = Some(format!("no answer within {}s", TIMEOUT.as_secs())),
    }
    out.total = started.elapsed();
    if out.error.is_some() {
        out.answered = false;
        out.tools = false;
    }
    out
}

fn shorten(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() > 90 {
        format!("{}…", line.chars().take(89).collect::<String>())
    } else {
        line.to_string()
    }
}

pub fn ranked(mut probes: Vec<Probe>) -> Vec<Probe> {
    probes.sort_by(|a, b| {
        b.score()
            .cmp(&a.score())
            .then(a.first.unwrap_or(a.total).cmp(&b.first.unwrap_or(b.total)))
            .then(a.label.cmp(&b.label))
    });
    probes
}

fn secs(d: Option<Duration>) -> String {
    d.map(|d| format!("{:.1}s", d.as_secs_f64())).unwrap_or_else(|| "-".into())
}

pub fn table(probes: &[Probe]) -> String {
    let width = probes.iter().map(|p| p.label.chars().count()).max().unwrap_or(5).max(5);
    let mut lines = vec![format!("{:>2}  {:<width$}  {:>5}  {:>5}  {:>6}  {}", "#", "model", "score", "first", "window", "result")];
    for (i, p) in probes.iter().enumerate() {
        let window = match p.context { Some(n) if n >= 1_000_000 => format!("{}M", n / 1_000_000), Some(n) => format!("{}k", n / 1_000), None => "-".into() };
        lines.push(format!(
            "{:>2}  {:<width$}  {:>5}  {:>5}  {:>6}  {}",
            i + 1,
            p.label,
            p.score(),
            secs(p.first),
            window,
            p.verdict()
        ));
    }
    lines.join("\n")
}

pub fn chain_from(probes: &[Probe], main: &str, limit: usize) -> Vec<String> {
    probes
        .iter()
        .filter(|p| p.answered && p.label != main)
        .take(limit)
        .map(|p| p.label.clone())
        .collect()
}

pub fn window_of(provider: &str, model: &str) -> Option<u32> {
    crate::quota::cached_models(&crate::quota::cache_dir(), provider)
        .into_iter()
        .find(|m| m.id == model)
        .and_then(|m| m.context_window)
}

fn usable(cfg: &crate::Config, store: &crate::auth::KeyStore, provider: &str) -> bool {
    cfg.providers
        .get(provider)
        .is_some_and(|spec| !crate::catalog::needs_key(spec) || cfg.resolve_key(store, provider).is_some())
}

fn models_of(cfg: &crate::Config, provider: &str) -> Vec<String> {
    let Some(spec) = cfg.providers.get(provider) else {
        return Vec::new();
    };
    let mut models: Vec<String> = crate::catalog::suggested_for(spec).iter().map(|m| m.to_string()).collect();
    for model in crate::quota::cached_models(&crate::quota::cache_dir(), provider) {
        if crate::ops::is_chat_model(&model.id) && !models.contains(&model.id) {
            models.push(model.id);
        }
    }
    models
}

pub fn candidates(
    cfg: &crate::Config,
    store: &crate::auth::KeyStore,
    chain: &[String],
    args: &[String],
) -> Result<Vec<(String, String, String)>, String> {
    let primary = chain
        .first()
        .and_then(|l| l.split_once(':'))
        .map(|(p, _)| p.to_string())
        .unwrap_or_default();
    let mut out: Vec<(String, String, String)> = Vec::new();
    let mut push = |provider: String, model: String| {
        let label = format!("{provider}:{model}");
        if !out.iter().any(|(l, _, _)| *l == label) && out.len() < MAX_CANDIDATES {
            out.push((label, provider, model));
        }
    };
    match args {
        [] => {
            for label in chain {
                if let Some((p, m)) = crate::split_chain_entry(label, &primary) {
                    push(p, m);
                }
            }
            let mut names: Vec<&String> = cfg.providers.keys().collect();
            names.sort();
            for name in names {
                if usable(cfg, store, name) {
                    for model in models_of(cfg, name).into_iter().take(3) {
                        push(name.clone(), model);
                    }
                }
            }
        }
        [one] if cfg.providers.contains_key(one.as_str()) => {
            if !usable(cfg, store, one) {
                return Err(format!("{one} has no API key; /login {one} first"));
            }
            let models = models_of(cfg, one);
            if models.is_empty() {
                return Err(format!("no models are known for {one} yet; run /models refresh"));
            }
            for model in models {
                push(one.clone(), model);
            }
        }
        entries => {
            for entry in entries {
                let Some((p, m)) = crate::split_chain_entry(entry, &primary) else {
                    continue;
                };
                if !cfg.providers.contains_key(&p) {
                    return Err(format!("no provider named '{p}' (from '{entry}'); write entries as provider:model or give one provider name"));
                }
                if !usable(cfg, store, &p) {
                    return Err(format!("{p} has no API key; /login {p} first"));
                }
                push(p, m);
            }
        }
    }
    if out.is_empty() {
        return Err("nothing to rank: no provider with a key has known models; /login and /models refresh first".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(label: &str, answered: bool, tools: bool, ms: u64, context: Option<u32>) -> Probe {
        Probe {
            label: label.into(),
            answered,
            tools,
            first: Some(Duration::from_millis(ms)),
            total: Duration::from_millis(ms + 100),
            error: (!answered).then(|| "rate limited".into()),
            context,
        }
    }

    #[test]
    fn working_tool_callers_rank_first_then_speed_and_room() {
        let order = ranked(vec![
            probe("slow:tools", true, true, 9000, Some(200_000)),
            probe("broken:x", false, false, 100, Some(200_000)),
            probe("fast:notools", true, false, 300, Some(200_000)),
            probe("fast:tools", true, true, 400, Some(1_000_000)),
            probe("fast:tools-small", true, true, 400, Some(16_000)),
        ]);
        let labels: Vec<&str> = order.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, ["fast:tools", "fast:tools-small", "fast:notools", "slow:tools", "broken:x"]);
        assert_eq!(chain_from(&order, "fast:tools", 2), ["fast:tools-small", "fast:notools"]);
        let shown = table(&order);
        assert!(shown.contains(" 1  fast:tools ") && shown.contains("works, calls tools"), "{shown}");
        assert!(shown.contains("rate limited"));
    }

    #[test]
    fn a_tool_call_counts_only_with_the_right_arguments() {
        assert!(called_add("add", r#"{"a":2,"b":3}"#));
        assert!(called_add("add", r#"{"a":"2","b":3.0}"#));
        assert!(!called_add("add", r#"{"a":2,"b":2}"#));
        assert!(!called_add("sum", r#"{"a":2,"b":3}"#));
        assert!(!called_add("add", "not json"));
    }
}
