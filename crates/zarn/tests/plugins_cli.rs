mod support;

use support::*;

#[test]
fn zarn_plugin_adds_lists_approves_and_removes_without_running_plugin_code() {
    let home = Home::new();
    let source = home.root().join("kit");
    let marker = home.root().join("ran");
    std::fs::create_dir_all(source.join("commands")).unwrap();
    std::fs::write(
        source.join("plugin.toml"),
        format!("name = \"kit\"\nversion = \"0.2.0\"\n\n[[hooks.turn_end]]\ncommand = \"touch {}\"\n", marker.display()),
    )
    .unwrap();
    std::fs::write(source.join("commands/fix.md"), "Fix it").unwrap();

    let add = home.run(&["plugin", "add", source.to_str().unwrap()], "");
    assert!(add.status.success(), "{}", stderr(&add));
    let text = stdout(&add);
    assert!(text.contains("installed kit"), "{text}");
    assert!(text.contains("runs hook turn_end: touch"), "{text}");
    assert!(text.contains("stay off until you approve them: zarn plugin approve kit"), "{text}");

    let list = stdout(&home.run(&["plugin", "list"], ""));
    assert!(list.contains("kit 0.2.0") && list.contains("hooks and MCP wait for approval"), "{list}");
    assert!(!stdout(&home.run(&["hooks", "list"], "")).contains("touch"), "unapproved hooks are off");

    let refused = home.run(&["plugin", "approve", "kit"], "");
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("rerun with --yes"));

    let approved = home.run(&["plugin", "approve", "kit", "--yes"], "");
    assert!(stdout(&approved).contains("approved kit"), "{}", stderr(&approved));
    assert!(stdout(&home.run(&["hooks", "list"], "")).contains("touch"));

    let removed = home.run(&["plugin", "remove", "kit"], "");
    assert!(stdout(&removed).contains("removed kit"));
    assert!(stdout(&home.run(&["plugin", "list"], "")).contains("no plugins"));
    assert!(!marker.exists(), "nothing the plugin ships ever ran");
}
