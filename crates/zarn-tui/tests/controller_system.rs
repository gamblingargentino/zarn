mod support;

use support::*;

#[tokio::test]
async fn the_system_prompt_can_be_shown_overridden_edited_and_reset() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let mut c = env.controller();

    turn(&mut c, "first").await;
    let first = system_text(&chat_bodies(&mock).await[0]);
    assert!(first.starts_with("You are zarn, an agent"), "{first}");
    assert!(first.contains("Do not invent a scratch or temp folder"), "{first}");
    assert!(first.contains(&env.project().display().to_string()), "the working directory is filled in");
    assert!(!first.contains("{cwd}") && !first.contains("{environment}"));

    c.type_line("/system");
    let shown = notices(&c).last().unwrap().clone();
    assert!(shown.contains("system prompt: the built-in default") && shown.contains("You are zarn, an agent"), "{shown}");

    std::fs::create_dir_all(env.user_dir()).unwrap();
    std::fs::write(env.user_dir().join("SYSTEM.md"), "You are the user's own agent in {cwd}.").unwrap();
    std::fs::write(env.project().join("ZARN.md"), "Project rule: tabs.").unwrap();
    turn(&mut c, "second").await;
    let second = system_text(&chat_bodies(&mock).await[1]);
    assert!(second.starts_with(&format!("You are the user's own agent in {}.", env.project().display())), "{second}");
    assert!(second.contains("Project rule: tabs."), "ZARN.md still comes after a custom prompt: {second}");
    assert!(!second.contains("You are zarn, an agent"));

    let project_file = env.project().join(".zarn").join("SYSTEM.md");
    std::fs::create_dir_all(project_file.parent().unwrap()).unwrap();
    std::fs::write(&project_file, "Project prompt wins.").unwrap();
    turn(&mut c, "third").await;
    assert!(system_text(&chat_bodies(&mock).await[2]).starts_with("Project prompt wins."));
    c.type_line("/system");
    assert!(notices(&c).last().unwrap().contains("(project)"));

    c.type_line("/system reset --project");
    assert!(!project_file.exists());
    assert!(notices(&c).last().unwrap().contains("SYSTEM.md (user)"), "{:?}", notices(&c).last());
    c.type_line("/system reset");
    assert!(notices(&c).last().unwrap().contains("the built-in default"));
    c.type_line("/system reset");
    assert!(notices(&c).last().unwrap().contains("nothing to reset"));
    turn(&mut c, "fourth").await;
    assert!(system_text(&chat_bodies(&mock).await[3]).starts_with("You are zarn, an agent"));
}

#[cfg(unix)]
#[tokio::test]
async fn system_edit_opens_the_editor_seeded_with_the_default_and_saves() {
    let mock = echo_server("ok").await;
    let env = Env::new(&provider_config(&mock.uri(), "")).await;
    let script = env.root().join("ed.sh");
    let seen = env.root().join("seen.txt");
    std::fs::write(
        &script,
        format!("cp \"$1\" {}\nprintf 'Edited prompt for {{cwd}}.' > \"$1\"\n", seen.display()),
    )
    .unwrap();
    unsafe {
        std::env::set_var("VISUAL", format!("sh {}", script.display()));
    }
    let mut c = env.controller();
    c.type_line("/system edit --project");
    unsafe {
        std::env::remove_var("VISUAL");
    }
    let seeded = std::fs::read_to_string(&seen).unwrap();
    assert!(seeded.starts_with("You are zarn, an agent"), "the editor starts from the default: {seeded}");
    let saved = env.project().join(".zarn").join("SYSTEM.md");
    assert_eq!(std::fs::read_to_string(&saved).unwrap(), "Edited prompt for {cwd}.\n");
    assert!(notices(&c).last().unwrap().contains("saved"));
    turn(&mut c, "hi").await;
    assert!(system_text(&chat_bodies(&mock).await[0]).starts_with(&format!("Edited prompt for {}.", env.project().display())));
}
