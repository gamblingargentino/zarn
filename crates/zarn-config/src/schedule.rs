use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Scheduled {
    pub id: String,
    pub prompt: String,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub created: u64,
}

pub fn parse_interval(text: &str) -> Result<Duration> {
    let text = text.trim();
    let split = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let n: u64 = number.parse().map_err(|_| anyhow!("'{text}' is not an interval; use forms like 30s, 5m, 2h or 1d"))?;
    let seconds = match unit {
        "s" | "sec" | "secs" => n,
        "m" | "min" | "mins" | "" => n * 60,
        "h" | "hr" | "hrs" => n * 3600,
        "d" | "day" | "days" => n * 86_400,
        other => bail!("'{other}' is not a time unit; use s, m, h or d"),
    };
    if seconds == 0 {
        bail!("the interval must be longer than zero");
    }
    Ok(Duration::from_secs(seconds))
}

pub fn label(every: Duration) -> String {
    let s = every.as_secs();
    if s.is_multiple_of(86_400) {
        format!("{}d", s / 86_400)
    } else if s.is_multiple_of(3600) {
        format!("{}h", s / 3600)
    } else if s.is_multiple_of(60) {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

pub struct Schedules {
    file: PathBuf,
}

impl Schedules {
    pub fn at(file: PathBuf) -> Self {
        Schedules { file }
    }

    pub fn system() -> Self {
        Schedules::at(crate::paths::data_dir().join("schedules.json"))
    }

    pub fn list(&self) -> Vec<Scheduled> {
        std::fs::read_to_string(&self.file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save(&self, all: &[Scheduled]) -> Result<()> {
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.file, serde_json::to_string_pretty(all)?).with_context(|| format!("writing {}", self.file.display()))
    }

    pub fn add(&self, prompt: &str, cwd: &Path, when: Option<String>, mode: Option<String>) -> Result<Scheduled> {
        if prompt.trim().is_empty() {
            bail!("a scheduled prompt cannot be empty");
        }
        if mode.as_deref() == Some("bypass") {
            bail!("scheduled runs are headless and cannot use bypass mode");
        }
        let mut all = self.list();
        let next = all
            .iter()
            .filter_map(|s| s.id.strip_prefix('s').and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0)
            + 1;
        let created = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let entry = Scheduled {
            id: format!("s{next}"),
            prompt: prompt.trim().to_string(),
            cwd: cwd.to_path_buf(),
            when,
            mode,
            created,
        };
        all.push(entry.clone());
        self.save(&all)?;
        Ok(entry)
    }

    pub fn remove(&self, id: &str) -> Result<Scheduled> {
        let mut all = self.list();
        let at = all.iter().position(|s| s.id == id).ok_or_else(|| anyhow!("no scheduled prompt {id}"))?;
        let removed = all.remove(at);
        self.save(&all)?;
        Ok(removed)
    }

    pub fn get(&self, id: &str) -> Result<Scheduled> {
        self.list().into_iter().find(|s| s.id == id).ok_or_else(|| anyhow!("no scheduled prompt {id}"))
    }
}

pub fn cron_line(entry: &Scheduled) -> String {
    let when = entry.when.clone().unwrap_or_else(|| "0 9 * * *".into());
    format!("{when} cd {} && zarn run-scheduled {}", shell_quote(&entry.cwd.display().to_string()), entry.id)
}

fn shell_quote(text: &str) -> String {
    if text.chars().all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c)) {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_parse_and_print() {
        assert_eq!(parse_interval("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_interval("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_interval("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_interval("1d").unwrap(), Duration::from_secs(86_400));
        assert_eq!(parse_interval("10").unwrap(), Duration::from_secs(600));
        assert!(parse_interval("0s").is_err());
        assert!(parse_interval("soon").is_err());
        assert!(parse_interval("5y").is_err());
        assert_eq!(label(Duration::from_secs(7200)), "2h");
        assert_eq!(label(Duration::from_secs(90)), "90s");
    }

    #[test]
    fn schedules_are_stored_listed_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let store = Schedules::at(dir.path().join("schedules.json"));
        let a = store.add("check the build", Path::new("/work/my repo"), Some("*/30 * * * *".into()), None).unwrap();
        let b = store.add("summarize", Path::new("/work"), None, Some("plan".into())).unwrap();
        assert_eq!((a.id.as_str(), b.id.as_str()), ("s1", "s2"));
        assert_eq!(cron_line(&a), "*/30 * * * * cd '/work/my repo' && zarn run-scheduled s1");
        assert!(store.add("x", Path::new("/"), None, Some("bypass".into())).is_err());
        assert_eq!(store.remove("s1").unwrap().prompt, "check the build");
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.add("again", Path::new("/"), None, None).unwrap().id, "s3");
        assert!(store.get("s1").is_err());
    }
}
