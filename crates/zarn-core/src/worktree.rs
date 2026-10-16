use anyhow::{Result, anyhow, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| anyhow!("cannot run git: {e}"))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains("..")
        && !name.starts_with(['-', '/', '.'])
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

pub fn repo_root(dir: &Path) -> Result<PathBuf> {
    git(dir, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .map_err(|_| anyhow!("{} is not inside a git repository", dir.display()))
}

pub fn head(dir: &Path) -> Result<String> {
    git(dir, &["rev-parse", "HEAD"])
}

pub fn path_for(root: &Path, name: &str) -> PathBuf {
    root.join(".zarn").join("worktrees").join(name)
}

pub fn create(root: &Path, name: &str) -> Result<PathBuf> {
    if !valid_name(name) {
        bail!("'{name}' is not a usable worktree name; use letters, digits, - _ . or /");
    }
    let path = path_for(root, name);
    if path.join(".git").exists() {
        return Ok(path);
    }
    let parent = root.join(".zarn").join("worktrees");
    std::fs::create_dir_all(&parent)?;
    let ignore = parent.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(&ignore, "*\n")?;
    }
    let target = path.to_string_lossy().to_string();
    let branch_exists = git(root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{name}")]).is_ok();
    if branch_exists {
        git(root, &["worktree", "add", &target, name])?;
    } else {
        git(root, &["worktree", "add", "-b", name, &target])?;
    }
    Ok(path)
}

pub fn has_changes(path: &Path, base: &str) -> bool {
    let dirty = git(path, &["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(true);
    let ahead = git(path, &["rev-list", "--count", &format!("{base}..HEAD")])
        .map(|n| n != "0")
        .unwrap_or(true);
    dirty || ahead
}

pub fn remove_if_clean(root: &Path, path: &Path, branch: &str, base: &str) -> Result<bool> {
    if has_changes(path, base) {
        return Ok(false);
    }
    git(root, &["worktree", "remove", &path.to_string_lossy()])?;
    let _ = git(root, &["branch", "-D", branch]);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.test")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.test")
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "first"]);
        dir
    }

    #[test]
    fn worktrees_are_created_reused_and_only_removed_when_clean() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = repo();
        let root = repo_root(dir.path()).unwrap();
        let base = head(&root).unwrap();
        let clean = create(&root, "clean-one").unwrap();
        assert!(clean.ends_with(".zarn/worktrees/clean-one"));
        assert_eq!(create(&root, "clean-one").unwrap(), clean, "reused");
        assert!(remove_if_clean(&root, &clean, "clean-one", &base).unwrap());
        assert!(!clean.exists());

        let dirty = create(&root, "dirty-one").unwrap();
        std::fs::write(dirty.join("b.txt"), "b").unwrap();
        assert!(!remove_if_clean(&root, &dirty, "dirty-one", &base).unwrap());
        assert!(dirty.join("b.txt").exists(), "a worktree with changes is never deleted");
        assert!(create(&root, "../escape").is_err());
        assert!(repo_root(Path::new("/")).is_err());
    }
}
