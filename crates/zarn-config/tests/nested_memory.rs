use serde_json::json;
use std::sync::Arc;
use zarn_config::context::{self, NestedMemory, WithMemory};
use zarn_core::tools::{Ctx, Tool};

fn write(root: &std::path::Path, rel: &str, body: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn read_tool(memory: Arc<NestedMemory>) -> Box<dyn Tool> {
    WithMemory::wrap(Box::new(zarn_tools::read::Read::new()), memory)
}

#[tokio::test]
async fn a_subdirectory_zarn_md_joins_the_context_the_first_time_a_file_under_it_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "ZARN.md", "top level rules");
    write(root, "crates/core/ZARN.md", "core rules: no unsafe");
    write(root, "crates/core/src/lib.rs", "pub fn x() {}");
    write(root, "crates/core/src/other.rs", "pub fn y() {}");
    write(root, "docs/guide.md", "guide");
    let memory = NestedMemory::new(root);
    let tool = read_tool(memory.clone());
    let ctx = Ctx::new(root);

    let first = tool.run(json!({ "path": "crates/core/src/lib.rs" }), &ctx).await.unwrap();
    assert!(first.content.contains("pub fn x"));
    assert!(first.content.contains("<memory source=\"nested\" path=\"crates/core/ZARN.md\">\ncore rules: no unsafe\n</memory>"), "{}", first.content);
    assert!(first.content.contains("apply to files under crates/core/"));
    assert!(!first.content.contains("top level rules"), "the root file is already in the system prompt");

    let second = tool.run(json!({ "path": "crates/core/src/other.rs" }), &ctx).await.unwrap();
    assert!(!second.content.contains("<memory"), "only the first time");
    let elsewhere = tool.run(json!({ "path": "docs/guide.md" }), &ctx).await.unwrap();
    assert!(!elsewhere.content.contains("<memory"));
    assert_eq!(memory.loaded().len(), 1);

    let grep = WithMemory::wrap(Box::new(zarn_tools::search::Grep::new()), memory.clone());
    assert_eq!(grep.spec().name, "grep");
}

#[tokio::test]
async fn edits_and_writes_also_load_it_and_the_budget_still_applies() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "a/ZARN.md", &"a ".repeat(400));
    write(root, "b/ZARN.md", "b rules");
    let memory = NestedMemory::new(root);
    memory.set_budget(50);
    let ctx = Ctx::new(root);
    let writer = WithMemory::wrap(Box::new(zarn_tools::write::Write::new()), memory.clone());
    let out = writer.run(json!({ "path": "a/new.txt", "content": "x" }), &ctx).await.unwrap();
    assert!(out.content.contains("path=\"a/ZARN.md\""), "{}", out.content);
    assert!(out.content.contains("[a/ZARN.md truncated to fit the ZARN.md token budget]"));
    let out = writer.run(json!({ "path": "b/new.txt", "content": "x" }), &ctx).await.unwrap();
    assert!(out.content.contains("[b/ZARN.md omitted: the ZARN.md token budget is used up]"), "{}", out.content);
}

#[test]
fn imports_expand_nest_and_stop_at_cycles_and_depth() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, "ZARN.md", "intro\n@docs/style.md\n@missing.md\nnot an @import line\n");
    write(root, "docs/style.md", "style guide\n@parts/naming.md");
    write(root, "docs/parts/naming.md", "use snake_case\n@../style.md");
    let files = context::memory_files(root, None);
    let text = &files[0].text;
    assert!(text.contains("intro\nstyle guide\nuse snake_case"), "{text}");
    assert!(text.contains("[import @../style.md skipped: it would import itself in a cycle]"), "{text}");
    assert!(text.contains("[import @missing.md not found]"));
    assert!(text.contains("not an @import line"));

    for i in 0..8 {
        write(root, &format!("deep/{i}.md"), &format!("level {i}\n@{}.md", i + 1));
    }
    write(root, "deep/ZARN.md", "@0.md");
    let deep = context::expand_imports("@0.md", &root.join("deep/ZARN.md"));
    assert!(deep.contains("level 4"), "{deep}");
    assert!(!deep.contains("level 6"), "{deep}");
    assert!(deep.contains("skipped: imports nest deeper than 5"), "{deep}");

    let section = context::memory_section(&files, 3);
    assert!(section.contains("truncated to fit a 3 token budget"));
}
