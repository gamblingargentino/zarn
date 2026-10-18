use serde_json::json;
use std::path::Path;
use zarn_core::tools::{Ctx, Tool};
use zarn_tools::skill::{Source, SkillTool, discover, index};

fn skill(root: &Path, folder: &str, name: &str, description: &str, body: &str) {
    let dir = root.join(folder);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n"),
    )
    .unwrap();
}

#[test]
fn a_project_skill_wins_a_name_clash_with_a_user_skill() {
    let user = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    skill(user.path(), "pdf", "pdf", "user pdf", "user body");
    skill(user.path(), "docx", "docx", "user docx", "docx body");
    skill(project.path(), "pdf-local", "pdf", "project pdf", "project body");

    let skills = discover(Some(user.path()), project.path());
    assert_eq!(skills.len(), 2);
    let pdf = skills.iter().find(|s| s.name == "pdf").unwrap();
    assert_eq!(pdf.description, "project pdf");
    assert_eq!(pdf.source, Source::Project);
    let docx = skills.iter().find(|s| s.name == "docx").unwrap();
    assert_eq!(docx.source, Source::User);
}

#[test]
fn a_folder_without_skill_md_is_ignored_and_a_nameless_skill_uses_its_folder() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("empty")).unwrap();
    let dir = root.path().join("folder-name");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), "---\ndescription: d\n---\nbody").unwrap();

    let skills = discover(None, root.path());
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "folder-name");
}

#[test]
fn the_index_carries_only_name_and_description() {
    let root = tempfile::tempdir().unwrap();
    skill(root.path(), "a", "alpha", "does alpha things", "THE FULL BODY");
    let text = index(&discover(None, root.path()));
    assert!(text.contains("alpha: does alpha things"));
    assert!(!text.contains("THE FULL BODY"));
}

#[tokio::test]
async fn the_skill_tool_loads_the_full_body_and_lists_supporting_files() {
    let root = tempfile::tempdir().unwrap();
    skill(root.path(), "forms", "forms", "fill forms", "Step one.\nStep two.");
    std::fs::create_dir_all(root.path().join("forms/scripts")).unwrap();
    std::fs::write(root.path().join("forms/scripts/fill.py"), "print(1)").unwrap();

    let tool = SkillTool::new(discover(None, root.path()));
    assert_eq!(tool.spec().name, "skill");
    let ctx = Ctx::new(root.path());
    let out = tool.run(json!({ "name": "forms" }), &ctx).await.unwrap();
    assert!(!out.is_error);
    assert!(out.content.contains("Step one.\nStep two."));
    assert!(!out.content.contains("description: fill forms"));
    assert!(out.content.contains("scripts/fill.py"));
    assert!(ctx.was_read(root.path().join("forms/SKILL.md")));
}

#[tokio::test]
async fn an_unknown_skill_is_an_error_naming_the_known_ones() {
    let root = tempfile::tempdir().unwrap();
    skill(root.path(), "a", "alpha", "d", "b");
    let tool = SkillTool::new(discover(None, root.path()));
    let out = tool
        .run(json!({ "name": "beta" }), &Ctx::new(root.path()))
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("alpha"));
}
