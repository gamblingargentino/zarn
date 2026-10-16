mod support;

use support::*;

#[test]
fn a_fallback_switch_is_reported_once_on_stderr_in_print_mode() {
    let limited = Mock::status(429);
    let backup = Mock::openai("from backup");
    let home = Home::new();
    home.config(&format!(
        "[providers.anthropic]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[providers.openai]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[profiles.default]\nprovider = \"anthropic\"\nmodel = \"m\"\nfallback = [\"openai\"]\n",
        limited.base(),
        backup.base()
    ));
    let out = home.run(&["--print", "hi"], "");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("from backup"));
    let err = stderr(&out);
    assert_eq!(
        err.matches("anthropic rate-limited, answered by openai").count(),
        1,
        "{err}"
    );
}

fn one_provider(home: &Home, mock: &Mock) {
    home.config(&format!(
        "[providers.mock]\nkind = \"openai-compat\"\nbase_url = \"{}\"\n\n[profiles.default]\nprovider = \"mock\"\nmodel = \"m\"\n",
        mock.base()
    ));
}

fn review_mode(home: &Home) {
    let dir = home.project().join(".zarn/modes");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("review.md"),
        "---\nname: review\ndescription: read-only code review\ntools: read, glob, grep\npermissions: edit=deny, write=deny, bash=ask\n---\nReview the code.\n",
    )
    .unwrap();
}

#[test]
fn modes_list_and_show_describe_builtin_and_custom_modes() {
    let home = Home::new();
    review_mode(&home);
    let listed = home.run(&["modes", "list"], "");
    assert!(listed.status.success(), "{}", stderr(&listed));
    let text = stdout(&listed);
    for name in ["default", "plan", "accept-edits", "bypass", "review"] {
        assert!(text.contains(name), "{text}");
    }
    assert!(text.contains("project"));
    let shown = home.run(&["modes", "show", "review"], "");
    let text = stdout(&shown);
    assert!(text.contains("read, glob, grep"));
    assert!(text.contains("edit=deny"));
    assert!(text.contains("Review the code."));
    let missing = home.run(&["modes", "show", "nope"], "");
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("no mode named 'nope'"));
}

#[test]
fn print_refuses_bypass_unless_the_permission_mode_says_so() {
    let mock = Mock::openai("ok");
    let home = Home::new();
    one_provider(&home, &mock);
    let refused = home.run(&["--print", "hi", "--mode", "bypass"], "");
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("--permission-mode bypass"));
    assert!(mock.requests().is_empty());
    let allowed = home.run(&["--print", "hi", "--permission-mode", "bypass"], "");
    assert!(allowed.status.success(), "{}", stderr(&allowed));
}

#[test]
fn print_honours_a_custom_mode_for_tools_and_prompt() {
    let mock = Mock::openai("ok");
    let home = Home::new();
    one_provider(&home, &mock);
    review_mode(&home);
    let out = home.run(&["--print", "look", "--mode", "review"], "");
    assert!(out.status.success(), "{}", stderr(&out));
    let body: serde_json::Value = serde_json::from_str(&mock.requests()[0].body).unwrap();
    let mut names: Vec<String> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(names, vec!["glob", "grep", "read"]);
    assert!(body["messages"][0]["content"].as_str().unwrap().contains("Review the code."));
}

#[test]
fn print_in_accept_edits_writes_without_a_prompt_and_default_does_not() {
    for (mode, expect) in [("accept-edits", true), ("default", false)] {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let mock = Mock::start(std::sync::Arc::new(move |_| {
            let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = if n == 0 {
                tool_call_sse("w", "write", &serde_json::json!({ "path": "made.txt", "content": "x" }))
            } else {
                sse("done")
            };
            (200, "text/event-stream".into(), body)
        }));
        let home = Home::new();
        one_provider(&home, &mock);
        let out = home.run(&["--print", "make it", "--mode", mode], "");
        assert!(out.status.success(), "{}", stderr(&out));
        assert_eq!(home.project().join("made.txt").exists(), expect, "{mode}");
    }
}
