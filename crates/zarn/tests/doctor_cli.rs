mod support;

use support::*;

#[test]
fn zarn_doctor_covers_each_subsystem_and_names_the_fix() {
    let home = Home::new();
    home.config(
        "[general]\nmode = \"nope\"\n\n[providers.local]\nkind = \"openai-compat\"\nbase_url = \"http://127.0.0.1:9\"\n\n[profiles.default]\nprovider = \"local\"\nmodel = \"m\"\n\n[providers.cloud]\nkind = \"anthropic\"\n\n[routing]\nenabled = true\n\n[routing.roles.title]\nprovider = \"cloud\"\n\n[limits]\nday_usd = 5.0\n\n[[hooks.turn_end]]\ncommand = \"definitely-not-a-real-program-zarn\"\n",
    );
    std::fs::write(home.root().join("cfg/keybindings.toml"), "transcript = \"ctrl+r\"\n").unwrap();
    let discovery = home.root().join("discovery");
    std::fs::create_dir_all(&discovery).unwrap();
    std::fs::write(discovery.join("local.json"), "{ not json").unwrap();
    let plugin = home.root().join("kit");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(plugin.join("plugin.toml"), "name = \"kit\"\n\n[[hooks.turn_end]]\ncommand = \"echo hi\"\n").unwrap();
    assert!(home.run(&["plugin", "add", plugin.to_str().unwrap()], "").status.success());

    let out = home.run(&["doctor", "--offline"], "");
    let text = stdout(&out);
    for needle in [
        "general.mode is 'nope', which is not a mode; run /mode or zarn modes list",
        "role title uses cloud, which has no key; run zarn login cloud",
        "is unreadable; run zarn models --refresh to rebuild it",
        "hooks and MCP servers of kit are off until approved: zarn plugin approve kit",
        "spend caps cannot be enforced for m",
        "ctrl+r is bound to both",
        "sandbox",
        "hooks",
        "mouse",
    ] {
        assert!(text.contains(needle), "missing '{needle}' in:\n{text}");
    }
    assert!(!out.status.success(), "a failing check makes zarn doctor exit non-zero");
}

#[test]
fn an_invalid_config_names_the_fix_too() {
    let home = Home::new();
    home.config("[providers.local]\nkind = \"openai-compat\"\nbase_url = \"http://127.0.0.1:9\"\n\n[profiles.default]\nprovider = \"local\"\nmodel = \"m\"\n\n[routing]\nenabled = true\n\n[routing.roles.title]\nprovider = \"ghost\"\n");
    let text = stdout(&home.run(&["doctor", "--offline"], ""));
    assert!(text.contains("names provider 'ghost', which is not configured; run zarn providers add ghost"), "{text}");
}
