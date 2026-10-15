use crate::types::ToolSpec;
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Risk {
    Safe,
    Write,
    Execute,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        ToolOutput {
            content: content.into(),
            is_error: false,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        ToolOutput {
            content: content.into(),
            is_error: true,
        }
    }
}

#[derive(Debug)]
pub struct Ctx {
    cwd: PathBuf,
    read_files: Mutex<HashSet<PathBuf>>,
}

impl Ctx {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Ctx {
            cwd: cwd.into(),
            read_files: Mutex::new(HashSet::new()),
        }
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn resolve(&self, path: impl AsRef<Path>) -> PathBuf {
        let p = path.as_ref();
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.cwd.join(p)
        }
    }

    fn key(&self, path: impl AsRef<Path>) -> PathBuf {
        let p = self.resolve(path);
        p.canonicalize().unwrap_or(p)
    }

    pub fn mark_read(&self, path: impl AsRef<Path>) {
        let k = self.key(path);
        if let Ok(mut set) = self.read_files.lock() {
            set.insert(k);
        }
    }

    pub fn was_read(&self, path: impl AsRef<Path>) -> bool {
        let k = self.key(path);
        self.read_files
            .lock()
            .map(|set| set.contains(&k))
            .unwrap_or(false)
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn risk(&self) -> Risk;
    async fn run(&self, args: Value, ctx: &Ctx) -> Result<ToolOutput>;
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.spec().name, tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self.tools.values().map(|t| t.spec()).collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tools.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }
}
