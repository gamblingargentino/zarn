use zarn_config::Config;

fn block_after(text: &str, heading: &str) -> String {
    let start = text.find(heading).expect("heading present");
    let rest = &text[start..];
    let open = rest.find("```toml\n").expect("a toml block") + "```toml\n".len();
    let close = rest[open..].find("```").expect("closed block");
    rest[open..open + close].to_string()
}

#[test]
fn the_readme_config_example_parses_validates_and_covers_every_general_key() {
    let readme = include_str!("../../../README.md");
    let example = block_after(readme, "### Every config key");
    let cfg: Config = toml::from_str(&example).expect("the README example parses");
    cfg.validate().expect("the README example is valid");
    let defaults = toml::to_string(&cfg.general).unwrap();
    for key in [
        "default_profile", "accent", "theme", "banner", "show_cost", "status_line", "notifications", "mouse", "vim",
        "mode", "auto_compact", "context_budget", "discovery_cache_minutes", "read_only_commands", "auto_allow_read_only",
        "sandbox", "max_parallel_agents", "lazy_tools_threshold",
    ] {
        assert!(defaults.contains(&format!("{key} =")), "the example sets general.{key}");
    }
    assert!(cfg.websearch.is_some() && cfg.git.commit_trailer.is_some());
    assert_eq!(cfg.mcp["files"].env["GITHUB_TOKEN"], "key:github-token");
    assert!(zarn_config::secrets::matches(readme).is_none(), "no real-looking key in the README");
    let docs = include_str!("../../../docs/github-action.md");
    assert!(zarn_config::secrets::matches(docs).is_none());
}

#[test]
fn the_plugin_example_in_the_readme_is_a_valid_manifest() {
    let readme = include_str!("../../../README.md");
    let manifest: zarn_config::plugins::Manifest = toml::from_str(&block_after(readme, "## Plugins")).unwrap();
    assert_eq!(manifest.name, "lint-kit");
    assert_eq!(manifest.hooks.pre_tool.len(), 1);
    assert!(manifest.mcp.contains_key("lint"));
}
