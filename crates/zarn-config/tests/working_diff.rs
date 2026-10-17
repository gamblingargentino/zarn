use std::path::Path;
use std::process::Command;
use zarn_config::git::working_diff;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(status.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&status.stderr));
}

#[tokio::test]
async fn shows_changes_untracked_files_and_path_filters() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let err = working_diff(root, "").await.unwrap_err();
    assert!(err.contains("not a git repository"), "{err}");

    git(root, &["init", "-q"]);
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    git(root, &["add", "a.txt"]);
    let unborn = working_diff(root, "").await.unwrap();
    assert!(unborn.contains("+one"), "a repository with no commits still shows staged files: {unborn}");

    git(root, &["commit", "-q", "-m", "first"]);
    assert_eq!(working_diff(root, "").await.unwrap(), "no changes since the last commit");

    std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(root.join("b.txt"), "new\n").unwrap();
    std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
    std::fs::write(root.join("ignored.txt"), "x\n").unwrap();
    let all = working_diff(root, "").await.unwrap();
    assert!(all.contains("diff --git a/a.txt b/a.txt") && all.contains("+two"), "{all}");
    assert!(all.contains("untracked (2):\n  .gitignore\n  b.txt"), "{all}");
    assert!(!all.contains("  ignored.txt"), "{all}");

    let only_b = working_diff(root, "b.txt").await.unwrap();
    assert!(!only_b.contains("a.txt") && only_b.contains("  b.txt"), "{only_b}");
}
