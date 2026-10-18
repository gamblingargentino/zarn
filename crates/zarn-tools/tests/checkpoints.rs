use serde_json::json;
use std::sync::Arc;
use zarn_core::agent::ToolGate;
use zarn_core::tools::{Ctx, Risk, Tool};
use zarn_tools::checkpoint::{Action, CheckpointGate, Checkpoints};
use zarn_tools::permissions::{Overlay, Permissions};

struct Fixture {
    _dir: tempfile::TempDir,
    cwd: std::path::PathBuf,
    checkpoints: Arc<Checkpoints>,
    gate: CheckpointGate,
    ctx: Ctx,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let checkpoints = Arc::new(Checkpoints::new(dir.path().join("snapshots"), &cwd));
    let allow = Arc::new(Permissions::headless().with_overlay(Overlay {
        name: "bypass".into(),
        allow_all: true,
        ..Overlay::default()
    }));
    let gate = CheckpointGate::new(allow, checkpoints.clone());
    let ctx = Ctx::new(&cwd);
    Fixture {
        _dir: dir,
        cwd,
        checkpoints,
        gate,
        ctx,
    }
}

async fn write(fx: &Fixture, path: &str, content: &str) {
    let args = json!({ "path": path, "content": content });
    fx.gate.check("write", Risk::Write, &args).await;
    zarn_tools::read::Read
        .run(json!({ "path": path }), &fx.ctx)
        .await
        .ok();
    let out = zarn_tools::write::Write.run(args, &fx.ctx).await.unwrap();
    assert!(!out.is_error, "{}", out.content);
}

async fn bash(fx: &Fixture, command: &str) {
    let args = json!({ "command": command });
    fx.gate.check("bash", Risk::Execute, &args).await;
    zarn_tools::bash::Bash::new().run(args, &fx.ctx).await.unwrap();
}

#[tokio::test]
async fn rewinding_restores_modified_removes_created_and_brings_back_deleted_files() {
    let fx = fixture();
    std::fs::write(fx.cwd.join("kept.txt"), "original").unwrap();
    std::fs::write(fx.cwd.join("doomed.txt"), "precious").unwrap();

    fx.checkpoints.begin("first", 1);
    write(&fx, "kept.txt", "changed").await;
    write(&fx, "fresh.txt", "new").await;
    bash(&fx, "rm doomed.txt").await;
    fx.checkpoints.end();

    fx.checkpoints.begin("second", 3);
    write(&fx, "kept.txt", "changed twice").await;
    fx.checkpoints.end();

    let plan = fx.checkpoints.plan(0).unwrap();
    assert_eq!(plan.message_index, 1);
    let text = plan.describe(&fx.cwd);
    assert!(text.contains("restore kept.txt"), "{text}");
    assert!(text.contains("remove fresh.txt"), "{text}");
    assert!(text.contains("restore doomed.txt"), "{text}");
    assert!(fx.checkpoints.apply(&plan).is_empty());

    assert_eq!(std::fs::read_to_string(fx.cwd.join("kept.txt")).unwrap(), "original");
    assert!(!fx.cwd.join("fresh.txt").exists());
    assert_eq!(std::fs::read_to_string(fx.cwd.join("doomed.txt")).unwrap(), "precious");
    assert!(fx.checkpoints.turns().is_empty());
}

#[tokio::test]
async fn rewinding_to_a_later_turn_keeps_earlier_work() {
    let fx = fixture();
    fx.checkpoints.begin("one", 1);
    write(&fx, "a.txt", "v1").await;
    fx.checkpoints.end();
    fx.checkpoints.begin("two", 3);
    write(&fx, "a.txt", "v2").await;
    fx.checkpoints.end();
    let plan = fx.checkpoints.plan(1).unwrap();
    assert_eq!(plan.changes.len(), 1);
    assert!(matches!(plan.changes[0].1, Action::Restore(_)));
    fx.checkpoints.apply(&plan);
    assert_eq!(std::fs::read_to_string(fx.cwd.join("a.txt")).unwrap(), "v1");
    assert_eq!(fx.checkpoints.turns().len(), 1);
}

#[tokio::test]
async fn a_file_changed_outside_zarn_since_the_checkpoint_is_reported_and_left_alone() {
    let fx = fixture();
    std::fs::write(fx.cwd.join("shared.txt"), "base").unwrap();
    fx.checkpoints.begin("turn", 1);
    write(&fx, "shared.txt", "zarn's version").await;
    fx.checkpoints.end();
    std::fs::write(fx.cwd.join("shared.txt"), "my manual fix").unwrap();
    let plan = fx.checkpoints.plan(0).unwrap();
    assert!(plan.changes.is_empty());
    assert!(plan.describe(&fx.cwd).contains("skip shared.txt (changed outside zarn"));
    fx.checkpoints.apply(&plan);
    assert_eq!(std::fs::read_to_string(fx.cwd.join("shared.txt")).unwrap(), "my manual fix");
}

#[tokio::test]
async fn gitignored_paths_and_huge_files_are_not_snapshotted_and_say_so() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_path_buf();
    std::fs::write(cwd.join(".gitignore"), "target/\n").unwrap();
    std::fs::create_dir_all(cwd.join("target")).unwrap();
    std::fs::write(cwd.join("target/out.bin"), "build").unwrap();
    std::fs::write(cwd.join("big.txt"), "x".repeat(2048)).unwrap();
    let checkpoints = Checkpoints::new(dir.path().join(".snap"), &cwd).with_max_bytes(1024);
    checkpoints.begin("t", 1);
    assert!(checkpoints.record("target/out.bin").is_none());
    let note = checkpoints.record("big.txt").unwrap();
    assert!(note.contains("not snapshotted"));
    checkpoints.end();
    let plan = checkpoints.plan(0).unwrap();
    let text = plan.describe(&cwd);
    assert!(text.contains("target/out.bin (ignored by .gitignore"), "{text}");
    assert!(text.contains("big.txt (too large to snapshot)"), "{text}");
}

#[tokio::test]
async fn a_denied_call_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let checkpoints = Arc::new(Checkpoints::new(dir.path().join("s"), dir.path()));
    let gate = CheckpointGate::new(Arc::new(Permissions::headless()), checkpoints.clone());
    checkpoints.begin("t", 1);
    gate.check("write", Risk::Write, &json!({ "path": "x.txt", "content": "y" })).await;
    checkpoints.end();
    assert!(checkpoints.turns()[0].files.is_empty());
}
