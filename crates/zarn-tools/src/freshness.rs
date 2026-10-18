use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;
use zarn_core::tools::Ctx;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    modified: Option<SystemTime>,
    hash: u64,
}

type Table = HashMap<(usize, PathBuf), Stamp>;

fn table() -> &'static Mutex<Table> {
    static TABLE: OnceLock<Mutex<Table>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn owner(ctx: &Ctx) -> usize {
    ctx as *const Ctx as usize
}

fn key(ctx: &Ctx, path: &Path) -> (usize, PathBuf) {
    let resolved = ctx.resolve(path);
    (owner(ctx), resolved.canonicalize().unwrap_or(resolved))
}

pub fn hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

pub fn stamp_of(path: &Path) -> Option<Stamp> {
    let bytes = std::fs::read(path).ok()?;
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    Some(Stamp {
        modified,
        hash: hash(&bytes),
    })
}

pub fn record(ctx: &Ctx, path: &Path) {
    let resolved = ctx.resolve(path);
    let Some(stamp) = stamp_of(&resolved) else {
        return;
    };
    if let Ok(mut table) = table().lock() {
        table.insert(key(ctx, &resolved), stamp);
    }
}

pub fn forget(ctx: &Ctx, path: &Path) {
    if let Ok(mut table) = table().lock() {
        table.remove(&key(ctx, path));
    }
}

pub fn stale(ctx: &Ctx, path: &Path) -> Option<String> {
    let resolved = ctx.resolve(path);
    let recorded = table().lock().ok()?.get(&key(ctx, &resolved)).copied()?;
    let current = stamp_of(&resolved);
    let changed = match current {
        None => true,
        Some(now) => now.hash != recorded.hash || now.modified != recorded.modified,
    };
    changed.then(|| {
        format!(
            "{} changed on disk after you last read it; read it again before modifying it",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unchanged_file_is_fresh_and_a_rewritten_one_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Ctx::new(dir.path());
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one").unwrap();
        record(&ctx, &path);
        assert!(stale(&ctx, &path).is_none());
        std::fs::write(&path, "two").unwrap();
        assert!(stale(&ctx, &path).unwrap().contains("read it again"));
    }

    #[test]
    fn a_file_never_recorded_is_not_reported() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Ctx::new(dir.path());
        let path = dir.path().join("b.txt");
        std::fs::write(&path, "x").unwrap();
        assert!(stale(&ctx, &path).is_none());
    }

    #[test]
    fn contexts_do_not_share_stamps() {
        let dir = tempfile::tempdir().unwrap();
        let a = Ctx::new(dir.path());
        let b = Ctx::new(dir.path());
        let path = dir.path().join("c.txt");
        std::fs::write(&path, "x").unwrap();
        record(&a, &path);
        std::fs::write(&path, "y").unwrap();
        assert!(stale(&a, &path).is_some());
        assert!(stale(&b, &path).is_none());
    }
}
