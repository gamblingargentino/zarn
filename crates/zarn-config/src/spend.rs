use crate::Config;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Limits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day_usd: Option<f64>,
}

impl Limits {
    pub fn is_empty(&self) -> bool {
        self.session_usd.is_none() && self.day_usd.is_none()
    }

    pub fn fill_from(&mut self, other: Limits) {
        self.session_usd = self.session_usd.or(other.session_usd);
        self.day_usd = self.day_usd.or(other.day_usd);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Check {
    Ok,
    Warn(Vec<String>),
    Stop(String),
}

pub const WARN_AT: f64 = 0.8;

fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

pub fn today() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0) as i64;
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02}")
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Day {
    usd: f64,
}

#[derive(Clone, Debug)]
pub struct Ledger {
    dir: PathBuf,
}

impl Ledger {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Ledger { dir: dir.into() }
    }

    pub fn system() -> Self {
        Self::new(crate::paths::data_dir().join("spend"))
    }

    pub fn path_for(&self, day: &str) -> PathBuf {
        self.dir.join(format!("{day}.json"))
    }

    fn read(path: &Path) -> f64 {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<Day>(&t).ok())
            .map(|d| d.usd)
            .unwrap_or(0.0)
    }

    pub fn today(&self) -> f64 {
        Self::read(&self.path_for(&today()))
    }

    pub fn add(&self, usd: f64) -> std::io::Result<f64> {
        if usd <= 0.0 || !usd.is_finite() {
            return Ok(self.today());
        }
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path_for(&today());
        let total = Self::read(&path) + usd;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string(&Day { usd: total })?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(total)
    }
}

fn level(name: &str, key: &str, cap: f64, spent: f64) -> Option<Result<String, String>> {
    if cap <= 0.0 {
        return Some(Err(format!(
            "the {name} spend cap is ${cap:.2}. Change {key} under [limits] in config.toml, or /limits {}",
            if key == "session_usd" { "session <usd|off>" } else { "day <usd|off>" }
        )));
    }
    let ratio = spent / cap;
    if ratio >= 1.0 {
        return Some(Err(format!(
            "the {name} spend cap of ${cap:.2} is reached (${spent:.4} spent). Raise or remove {key} under [limits] in config.toml, or run /limits {} <usd|off>",
            if key == "session_usd" { "session" } else { "day" }
        )));
    }
    (ratio >= WARN_AT).then(|| {
        Ok(format!(
            "{:.0}% of the {name} spend cap used (${spent:.4} of ${cap:.2})",
            ratio * 100.0
        ))
    })
}

pub fn check(limits: &Limits, session: f64, day: f64) -> Check {
    let mut warnings = Vec::new();
    for (name, key, cap, spent) in [
        ("session", "session_usd", limits.session_usd, session),
        ("daily", "day_usd", limits.day_usd, day),
    ] {
        let Some(cap) = cap else { continue };
        match level(name, key, cap, spent) {
            Some(Err(stop)) => return Check::Stop(stop),
            Some(Ok(warn)) => warnings.push(warn),
            None => {}
        }
    }
    if warnings.is_empty() {
        Check::Ok
    } else {
        Check::Warn(warnings)
    }
}

pub fn unenforceable(limits: &Limits, model: &str, priced: bool) -> Option<String> {
    (!limits.is_empty() && !priced).then(|| {
        format!(
            "spend caps cannot be enforced for {model}: no rate is configured, so its cost is unknown, not zero. Add it under [pricing] to enforce them."
        )
    })
}

pub fn validate(cfg: &Config) -> anyhow::Result<()> {
    for (key, value) in [
        ("session_usd", cfg.limits.session_usd),
        ("day_usd", cfg.limits.day_usd),
    ] {
        if let Some(v) = value
            && (!v.is_finite() || v < 0.0)
        {
            anyhow::bail!("[limits] {key} must be a positive number of dollars");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_computed_without_a_calendar_crate() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_742), (2026, 10, 16));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }

    #[test]
    fn caps_warn_at_eighty_percent_and_stop_at_the_cap_naming_it() {
        let limits = Limits {
            session_usd: Some(1.0),
            day_usd: Some(10.0),
        };
        assert_eq!(check(&limits, 0.5, 1.0), Check::Ok);
        let Check::Warn(w) = check(&limits, 0.85, 1.0) else { panic!() };
        assert!(w[0].contains("85% of the session spend cap"));
        let Check::Stop(s) = check(&limits, 1.0, 1.0) else { panic!() };
        assert!(s.contains("session spend cap of $1.00") && s.contains("session_usd"));
        let Check::Stop(s) = check(&limits, 0.1, 10.5) else { panic!() };
        assert!(s.contains("daily spend cap") && s.contains("day_usd"));
    }

    #[test]
    fn the_ledger_adds_up_today_and_holds_nothing_but_a_number() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(dir.path());
        ledger.add(0.25).unwrap();
        assert!((ledger.add(0.5).unwrap() - 0.75).abs() < 1e-9);
        let body = std::fs::read_to_string(ledger.path_for(&today())).unwrap();
        assert_eq!(body, "{\"usd\":0.75}");
    }

    #[test]
    fn an_unpriced_model_is_never_treated_as_free() {
        let limits = Limits {
            session_usd: Some(1.0),
            day_usd: None,
        };
        assert!(unenforceable(&limits, "local", false).unwrap().contains("cannot be enforced"));
        assert!(unenforceable(&limits, "priced", true).is_none());
        assert!(unenforceable(&Limits::default(), "local", false).is_none());
    }
}
