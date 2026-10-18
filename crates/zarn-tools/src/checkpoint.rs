use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use zarn_core::agent::{GateDecision, ToolGate};
use zarn_core::tools::Risk;

pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
pub const WRITE_TOOLS: &[&str] = &["edit", "write", "multiedit", "notebook_edit"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Before {
    Absent,
    Stored { copy: PathBuf, hash: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub before: Before,
    pub after: Option<Option<u64>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    pub label: String,
    pub message_index: usize,
    pub files: Vec<Entry>,
    pub skipped: Vec<(PathBuf, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Restore(PathBuf),
    Remove,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub turn: usize,
    pub message_index: usize,
    pub changes: Vec<(PathBuf, Action)>,
    pub skipped: Vec<(PathBuf, String)>,
}

impl Plan {
    pub fn describe(&self, cwd: &Path) -> String {
        let show = |p: &Path| {
            p.strip_prefix(cwd)
                .unwrap_or(p)
                .display()
                .to_string()
        };
        let mut lines = Vec::new();
        for (path, action) in &self.changes {
            lines.push(match action {
                Action::Restore(_) => format!("restore {}", show(path)),
                Action::Remove => format!("remove {} (created in that turn)", show(path)),
            });
        }
        for (path, why) in &self.skipped {
            lines.push(format!("skip {} ({why})", show(path)));
        }
        if lines.is_empty() {
            lines.push("no file changes to undo".into());
        }
        lines.join("\n")
    }
}

pub fn hash_file(path: &Path) -> Option<u64> {
    std::fs::read(path).ok().map(|b| crate::freshness::hash(&b))
}

struct State {
    turns: Vec<Turn>,
    open: bool,
    copies: usize,
}

pub struct Checkpoints {
    root: PathBuf,
    cwd: PathBuf,
    max_bytes: u64,
    state: Mutex<State>,
}

fn ignored(cwd: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(cwd) else {
        return false;
    };
    let mut builder = ignore::gitignore::GitignoreBuilder::new(cwd);
    let mut dir = cwd.to_path_buf();
    builder.add(dir.join(".gitignore"));
    for part in rel.parent().into_iter().flat_map(|p| p.components()) {
        dir.push(part);
        builder.add(dir.join(".gitignore"));
    }
    builder
        .build()
        .map(|g| g.matched_path_or_any_parents(path, false).is_ignore())
        .unwrap_or(false)
}

impl Checkpoints {
    pub fn new(root: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Checkpoints {
            root: root.into(),
            cwd: cwd.into(),
            max_bytes: MAX_BYTES,
            state: Mutex::new(State {
                turns: Vec::new(),
                open: false,
                copies: 0,
            }),
        }
    }

    pub fn with_max_bytes(mut self, max: u64) -> Self {
        self.max_bytes = max;
        self
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn begin(&self, label: impl Into<String>, message_index: usize) {
        if let Ok(mut state) = self.state.lock() {
            state.turns.push(Turn {
                label: label.into(),
                message_index,
                files: Vec::new(),
                skipped: Vec::new(),
            });
            state.open = true;
        }
    }

    fn resolve(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        let full = if p.is_absolute() { p.to_path_buf() } else { self.cwd.join(p) };
        let mut parts: Vec<std::path::Component> = Vec::new();
        for part in full.components() {
            match part {
                std::path::Component::ParentDir => {
                    parts.pop();
                }
                std::path::Component::CurDir => {}
                other => parts.push(other),
            }
        }
        parts.iter().collect()
    }

    pub fn record(&self, path: &str) -> Option<String> {
        let full = self.resolve(path);
        let Ok(mut state) = self.state.lock() else {
            return None;
        };
        if !state.open {
            return None;
        }
        let turn_index = state.turns.len() - 1;
        let known = state.turns[turn_index].files.iter().any(|e| e.path == full)
            || state.turns[turn_index].skipped.iter().any(|(p, _)| *p == full);
        if known {
            return None;
        }
        if ignored(&self.cwd, &full) {
            state.turns[turn_index]
                .skipped
                .push((full, "ignored by .gitignore, not snapshotted".into()));
            return None;
        }
        let before = match std::fs::metadata(&full) {
            Err(_) => Before::Absent,
            Ok(meta) if !meta.is_file() => return None,
            Ok(meta) if meta.len() > self.max_bytes => {
                let note = format!(
                    "{} is larger than {} MB and was not snapshotted; rewind cannot restore it",
                    full.display(),
                    self.max_bytes / (1024 * 1024)
                );
                state.turns[turn_index]
                    .skipped
                    .push((full, "too large to snapshot".into()));
                return Some(note);
            }
            Ok(_) => {
                state.copies += 1;
                let copy = self
                    .root
                    .join(format!("turn-{turn_index}"))
                    .join(format!("{}.snap", state.copies));
                let stored = copy
                    .parent()
                    .map(std::fs::create_dir_all)
                    .transpose()
                    .ok()
                    .and_then(|_| std::fs::copy(&full, &copy).ok());
                match (stored, hash_file(&full)) {
                    (Some(_), Some(hash)) => Before::Stored { copy, hash },
                    _ => return Some(format!("could not snapshot {}", full.display())),
                }
            }
        };
        state.turns[turn_index].files.push(Entry {
            path: full,
            before,
            after: None,
        });
        None
    }

    pub fn record_command(&self, command: &str) {
        for word in command.split_whitespace() {
            let word = word.trim_matches(['"', '\'']);
            if word.starts_with('-') || word.is_empty() {
                continue;
            }
            let full = self.resolve(word);
            if full.starts_with(&self.cwd) && full.is_file() {
                self.record(word);
            }
        }
    }

    pub fn end(&self) {
        if let Ok(mut state) = self.state.lock() {
            if !state.open {
                return;
            }
            state.open = false;
            if let Some(turn) = state.turns.last_mut() {
                for entry in &mut turn.files {
                    entry.after = Some(hash_file(&entry.path));
                }
            }
        }
    }

    pub fn turns(&self) -> Vec<Turn> {
        self.state.lock().map(|s| s.turns.clone()).unwrap_or_default()
    }

    pub fn plan(&self, turn: usize) -> Option<Plan> {
        let state = self.state.lock().ok()?;
        let target = state.turns.get(turn)?;
        let later = &state.turns[turn..];
        let mut paths: Vec<PathBuf> = Vec::new();
        for t in later {
            for e in &t.files {
                if !paths.contains(&e.path) {
                    paths.push(e.path.clone());
                }
            }
        }
        let mut changes = Vec::new();
        let mut skipped: Vec<(PathBuf, String)> = later.iter().flat_map(|t| t.skipped.clone()).collect();
        for path in paths {
            let entries: Vec<&Entry> = later
                .iter()
                .flat_map(|t| t.files.iter())
                .filter(|e| e.path == path)
                .collect();
            let first = entries[0];
            let last = entries[entries.len() - 1];
            let expected = last.after.unwrap_or_else(|| hash_file(&path));
            if hash_file(&path) != expected {
                skipped.push((path, "changed outside zarn since the checkpoint".into()));
                continue;
            }
            let action = match &first.before {
                Before::Absent if expected.is_none() => continue,
                Before::Absent => Action::Remove,
                Before::Stored { copy, hash } => {
                    if expected == Some(*hash) {
                        continue;
                    }
                    Action::Restore(copy.clone())
                }
            };
            changes.push((path, action));
        }
        Some(Plan {
            turn,
            message_index: target.message_index,
            changes,
            skipped,
        })
    }

    pub fn apply(&self, plan: &Plan) -> Vec<String> {
        let mut errors = Vec::new();
        for (path, action) in &plan.changes {
            let result = match action {
                Action::Remove => std::fs::remove_file(path),
                Action::Restore(copy) => path
                    .parent()
                    .map(std::fs::create_dir_all)
                    .unwrap_or(Ok(()))
                    .and_then(|_| std::fs::copy(copy, path).map(|_| ())),
            };
            if let Err(err) = result {
                errors.push(format!("{}: {err}", path.display()));
            }
        }
        if let Ok(mut state) = self.state.lock() {
            state.turns.truncate(plan.turn);
            state.open = false;
        }
        errors
    }
}

pub struct CheckpointGate {
    inner: Arc<dyn ToolGate>,
    checkpoints: Arc<Checkpoints>,
    notes: Arc<Mutex<Vec<String>>>,
}

impl CheckpointGate {
    pub fn new(inner: Arc<dyn ToolGate>, checkpoints: Arc<Checkpoints>) -> Self {
        CheckpointGate {
            inner,
            checkpoints,
            notes: Arc::default(),
        }
    }

    pub fn with_notes(mut self, notes: Arc<Mutex<Vec<String>>>) -> Self {
        self.notes = notes;
        self
    }
}

#[async_trait]
impl ToolGate for CheckpointGate {
    async fn check(&self, name: &str, risk: Risk, args: &Value) -> GateDecision {
        let decision = self.inner.check(name, risk, args).await;
        if decision == GateDecision::Allow {
            if WRITE_TOOLS.contains(&name) {
                let path = ["path", "notebook_path"]
                    .iter()
                    .find_map(|k| args.get(*k).and_then(Value::as_str));
                if let Some(path) = path
                    && let Some(note) = self.checkpoints.record(path)
                    && let Ok(mut notes) = self.notes.lock()
                {
                    notes.push(note);
                }
            } else if name == "bash"
                && let Some(command) = args.get("command").and_then(Value::as_str)
            {
                self.checkpoints.record_command(command);
            }
        }
        decision
    }
}
