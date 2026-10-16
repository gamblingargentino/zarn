use crate::types::Message;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Todo {
    pub content: String,
    pub status: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Meta {
    pub id: String,
    pub cwd: PathBuf,
    pub model: String,
    pub profile: Option<String>,
    pub created: u64,
    pub updated: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub todos: Vec<Todo>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "lowercase")]
pub enum Record {
    Meta(Meta),
    Message(Message),
}

pub fn data_root() -> PathBuf {
    if let Ok(explicit) = std::env::var("ZARN_DATA_DIR") {
        return PathBuf::from(explicit);
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("zarn")
        .join("sessions")
}

pub fn project_hash(cwd: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in cwd.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:032x}")
}

pub struct Session {
    meta: Meta,
    path: PathBuf,
    file: File,
}

impl Session {
    pub fn create(root: &Path, cwd: &Path, model: &str, profile: Option<String>) -> Result<Self> {
        let dir = root.join(project_hash(cwd));
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating session directory {}", dir.display()))?;

        let id = new_id();
        let path = dir.join(format!("{id}.jsonl"));
        let stamp = now();
        let meta = Meta {
            id,
            cwd: cwd.to_path_buf(),
            model: model.to_string(),
            profile,
            created: stamp,
            updated: stamp,
            ..Meta::default()
        };

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening session file {}", path.display()))?;

        let mut session = Session { meta, path, file };
        let record = Record::Meta(session.meta.clone());
        session.write(&record)?;
        Ok(session)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let (meta, _) = read(path)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening session file {}", path.display()))?;
        Ok(Session {
            meta,
            path: path.to_path_buf(),
            file,
        })
    }

    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn write(&mut self, record: &Record) -> Result<()> {
        let line = serde_json::to_string(record)?;
        writeln!(self.file, "{line}")?;
        self.file.flush()?;
        Ok(())
    }

    pub fn append(&mut self, message: &Message) -> Result<()> {
        self.write(&Record::Message(message.clone()))
    }

    pub fn append_all(&mut self, messages: &[Message]) -> Result<()> {
        for message in messages {
            self.append(message)?;
        }
        Ok(())
    }

    pub fn set_title(&mut self, title: &str) -> Result<()> {
        self.meta.title = Some(title.to_string());
        self.meta.updated = now();
        let record = Record::Meta(self.meta.clone());
        self.write(&record)
    }

    pub fn set_todos(&mut self, todos: &[Todo]) -> Result<()> {
        self.meta.todos = todos.to_vec();
        self.meta.updated = now();
        let record = Record::Meta(self.meta.clone());
        self.write(&record)
    }

    pub fn record_usage(&mut self, input: u64, output: u64, cached: u64) -> Result<()> {
        self.meta.input_tokens += input;
        self.meta.output_tokens += output;
        self.meta.cached_tokens += cached;
        self.meta.updated = now();
        let record = Record::Meta(self.meta.clone());
        self.write(&record)
    }
}

pub fn read(path: &Path) -> Result<(Meta, Vec<Message>)> {
    let file = File::open(path).with_context(|| format!("reading session {}", path.display()))?;
    let mut meta = Meta::default();
    let mut messages = Vec::new();

    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Record>(&line) {
            Ok(Record::Meta(m)) => meta = m,
            Ok(Record::Message(m)) => messages.push(m),
            Err(err) => tracing::warn!(%err, "skipping unreadable session record"),
        }
    }

    Ok((meta, messages))
}

pub fn list(root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let dir = root.join(project_hash(cwd));
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut found: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((modified, e.path()))
        })
        .collect();

    found.sort_by_key(|a| std::cmp::Reverse(a.0));
    found.into_iter().map(|(_, p)| p).collect()
}

pub fn newest(root: &Path, cwd: &Path) -> Option<PathBuf> {
    list(root, cwd).into_iter().next()
}

pub fn find(root: &Path, cwd: &Path, id: &str) -> Option<PathBuf> {
    let direct = root.join(project_hash(cwd)).join(format!("{id}.jsonl"));
    if direct.exists() {
        return Some(direct);
    }
    list(root, cwd)
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().starts_with(id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zarn-session-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn messages_round_trip_through_the_log() {
        let root = temp();
        let cwd = Path::new("/project/alpha");
        let mut session = Session::create(&root, cwd, "m", None).unwrap();
        session.append(&Message::user("hello")).unwrap();
        session.append(&Message::assistant("hi")).unwrap();

        let (meta, messages) = read(session.path()).unwrap();
        assert_eq!(meta.model, "m");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].text(), "hello");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn usage_accumulates_into_the_latest_meta_record() {
        let root = temp();
        let cwd = Path::new("/project/beta");
        let mut session = Session::create(&root, cwd, "m", None).unwrap();
        session.record_usage(10, 5, 2).unwrap();
        session.record_usage(1, 1, 0).unwrap();

        let (meta, _) = read(session.path()).unwrap();
        assert_eq!(meta.input_tokens, 11);
        assert_eq!(meta.output_tokens, 6);
        assert_eq!(meta.cached_tokens, 2);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_title_is_stored_in_the_latest_meta_record() {
        let root = temp();
        let cwd = Path::new("/project/titled");
        let mut session = Session::create(&root, cwd, "m", None).unwrap();
        session.append(&Message::user("fix the parser")).unwrap();
        session.set_title("Fix the parser").unwrap();
        session.record_usage(1, 1, 0).unwrap();
        let (meta, messages) = read(session.path()).unwrap();
        assert_eq!(meta.title.as_deref(), Some("Fix the parser"));
        assert_eq!(messages.len(), 1);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn newest_picks_the_most_recent_session_for_this_cwd() {
        let root = temp();
        let cwd = Path::new("/project/gamma");
        let first = Session::create(&root, cwd, "m", None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let second = Session::create(&root, cwd, "m", None).unwrap();

        let newest = newest(&root, cwd).unwrap();
        assert_eq!(newest, second.path());
        assert_ne!(newest, first.path());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn separate_directories_do_not_share_sessions() {
        let root = temp();
        Session::create(&root, Path::new("/project/one"), "m", None).unwrap();
        assert!(newest(&root, Path::new("/project/two")).is_none());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_corrupt_line_does_not_lose_the_rest_of_the_session() {
        let root = temp();
        let cwd = Path::new("/project/delta");
        let mut session = Session::create(&root, cwd, "m", None).unwrap();
        session.append(&Message::user("one")).unwrap();
        let path = session.path().to_path_buf();
        drop(session);

        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{{ not valid json").unwrap();
        drop(file);

        let mut session = Session::open(&path).unwrap();
        session.append(&Message::user("two")).unwrap();

        let (_, messages) = read(&path).unwrap();
        assert_eq!(messages.len(), 2);
        std::fs::remove_dir_all(root).ok();
    }
}
