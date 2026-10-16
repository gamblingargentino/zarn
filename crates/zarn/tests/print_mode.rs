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
