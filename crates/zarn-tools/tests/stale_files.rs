use serde_json::json;
use zarn_core::tools::{Ctx, Tool};
use zarn_tools::edit::Edit;
use zarn_tools::read::Read;
use zarn_tools::write::Write;

#[tokio::test]
async fn an_edit_after_an_outside_change_is_refused_until_the_file_is_read_again() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(dir.path());
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "alpha beta").unwrap();

    let read = Read.run(json!({ "path": "notes.txt" }), &ctx).await.unwrap();
    assert!(!read.is_error);

    std::fs::write(&path, "alpha gamma").unwrap();

    let edit = json!({ "path": "notes.txt", "old_string": "alpha", "new_string": "omega" });
    let refused = Edit.run(edit.clone(), &ctx).await.unwrap();
    assert!(refused.is_error);
    assert!(refused.content.contains("read it again"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha gamma");

    Read.run(json!({ "path": "notes.txt" }), &ctx).await.unwrap();
    let accepted = Edit.run(edit, &ctx).await.unwrap();
    assert!(!accepted.is_error, "{}", accepted.content);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "omega gamma");
}

#[tokio::test]
async fn consecutive_edits_by_the_agent_do_not_count_as_outside_changes() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(dir.path());
    std::fs::write(dir.path().join("a.txt"), "one two").unwrap();
    Read.run(json!({ "path": "a.txt" }), &ctx).await.unwrap();
    let first = Edit
        .run(json!({ "path": "a.txt", "old_string": "one", "new_string": "1" }), &ctx)
        .await
        .unwrap();
    let second = Edit
        .run(json!({ "path": "a.txt", "old_string": "two", "new_string": "2" }), &ctx)
        .await
        .unwrap();
    assert!(!first.is_error && !second.is_error);
    assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "1 2");
}

#[tokio::test]
async fn a_write_over_a_file_changed_since_it_was_read_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(dir.path());
    let path = dir.path().join("b.txt");
    std::fs::write(&path, "original").unwrap();
    Read.run(json!({ "path": "b.txt" }), &ctx).await.unwrap();
    std::fs::write(&path, "someone else").unwrap();

    let out = Write
        .run(json!({ "path": "b.txt", "content": "mine" }), &ctx)
        .await
        .unwrap();
    assert!(out.is_error);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "someone else");

    Read.run(json!({ "path": "b.txt" }), &ctx).await.unwrap();
    let out = Write
        .run(json!({ "path": "b.txt", "content": "mine" }), &ctx)
        .await
        .unwrap();
    assert!(!out.is_error);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine");
}

#[tokio::test]
async fn a_file_the_agent_created_can_be_written_again() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(dir.path());
    for content in ["first", "second"] {
        let out = Write
            .run(json!({ "path": "new.txt", "content": content }), &ctx)
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
    }
    assert_eq!(std::fs::read_to_string(dir.path().join("new.txt")).unwrap(), "second");
}

#[cfg(unix)]
#[tokio::test]
async fn the_sandbox_scrubs_keys_from_the_environment_of_bash() {
    use zarn_tools::bash::Bash;
    unsafe {
        std::env::set_var("ZARN_TEST_PROVIDER_API_KEY", "test-key-not-real-sandbox-env");
        std::env::set_var("ZARN_TEST_PLAIN_SETTING", "visible-value");
    }
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(dir.path());
    let open = Bash::new().run(json!({ "command": "env" }), &ctx).await.unwrap();
    assert!(open.content.contains("test-key-not-real-sandbox-env"));
    let closed = Bash::sandboxed().run(json!({ "command": "env" }), &ctx).await.unwrap();
    assert!(!closed.content.contains("test-key-not-real-sandbox-env"));
    assert!(!closed.content.contains("ZARN_TEST_PROVIDER_API_KEY"));
    assert!(closed.content.contains("visible-value"));
    assert!(closed.content.contains("ZARN_SANDBOX=1"));
    let pwd = Bash::sandboxed().run(json!({ "command": "pwd" }), &ctx).await.unwrap();
    assert_eq!(
        std::path::Path::new(pwd.content.trim()).canonicalize().unwrap(),
        dir.path().canonicalize().unwrap()
    );
}

#[test]
fn secret_looking_values_are_scrubbed_even_under_innocent_names() {
    assert!(zarn_tools::bash::looks_secret("OPENAI_API_KEY", "x"));
    assert!(zarn_tools::bash::looks_secret("GH_TOKEN", "x"));
    assert!(zarn_tools::bash::looks_secret("MY_VAR", "sk-test-key-not-real-0123456789"));
    assert!(!zarn_tools::bash::looks_secret("PATH", "/usr/bin"));
}
