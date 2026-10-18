# zarn roadmap: agent brief

You are implementing the next set of features for zarn, a Rust agent CLI. Read this whole file, then `CLAUDE.md`, then work the phases in order. Commit after each numbered item. Do not stop to ask unless a rule below says to stop.

## Hard rules

These come from `CLAUDE.md` and the previous work. They override anything in a phase.

1. **No code comments anywhere.** No `//`, `///`, `//!`, `/* */` in Rust. No `#` comments in shell scripts beyond the shebang. Check your diff before committing.
2. **Frozen interfaces.** Never edit, extend, or reformat `crates/zarn-core/src/provider.rs` or `crates/zarn-core/src/tools.rs`. Extend through the existing `Provider` and `Tool` traits and `ToolRegistry`. If an item seems to need a trait change, skip that item, note it, and report it. Do not work around it by editing the file.
3. **Dependencies.** Subagents never edit any `Cargo.toml`. If you are a subagent and need a crate, report the crate and version in your final message and stop that item. If you are the main session, you may add a dependency, but prefer std or a small local implementation first. Every new dependency must be named in your final report.
4. **Never run a bare `zarn`.** It starts the interactive TUI and blocks forever. Use `zarn --print "<prompt>"`, or drive the TUI through a pseudo-terminal script as the earlier end-to-end check did (a Python `pty` plus `pyte` harness against a local mock server).
5. **Secrets.** No real key, default credential, or personal path anywhere in source, tests, fixtures, docs or the binary. Fixtures use `test-key-not-real` (and suffixed variants). The pre-commit hook and `crates/zarn/tests/no_secrets.rs` enforce this. Do not weaken `scripts/secret-patterns.txt`.
6. **Keys never leak.** Anything that touches provider responses, error strings, logs, session files, tool output or panel text must pass through `zarn_config::auth::redact` or the existing guard wrappers in `crates/zarn-config/src/guard.rs`.
7. **Before every commit** run `cargo clippy --all-targets -- -D warnings` and `cargo test`. Both must pass. Finish with `cargo build --release`.
8. **One feature, one commit.** Commit messages say what and why, not just a title. End each with the attribution lines the session gives you.
9. **Tests are part of the item.** A feature with no test for each of its assertions is not done. TUI changes need `TestBackend` snapshots under `crates/zarn-tui/tests/snapshots/` (regenerate with `ZARN_UPDATE_SNAPSHOTS=1`, then read the output before trusting it). Network behaviour is tested against a local mock server, never a real API.
10. **Do not claim what you did not verify.** If something could not be run here (Windows Credential Manager, real provider APIs, a real MCP server), say so in the report.

## Codebase map

| Area | Where |
| --- | --- |
| Frozen traits | `crates/zarn-core/src/provider.rs`, `tools.rs` |
| Agent loop, events, `ToolGate` | `crates/zarn-core/src/agent.rs` |
| Subagents (`TaskTool`, `AgentDef`) | `crates/zarn-core/src/subagent.rs` |
| Compaction | `crates/zarn-core/src/compact.rs` (`should_compact`, `usage_ratio`) |
| Cost | `crates/zarn-core/src/cost.rs` (`CostTable`, `CostTracker`) |
| Sessions | `crates/zarn-core/src/session.rs` |
| Provider factory, `ProviderSpec` | `crates/zarn-providers/src/lib.rs` |
| Fallback chain | `crates/zarn-providers/src/fallback.rs` |
| Config model, merge, validate | `crates/zarn-config/src/lib.rs` |
| Project/user layers and writes | `crates/zarn-config/src/layers.rs` |
| Shared edit operations (panel, CLI, commands all call these) | `crates/zarn-config/src/ops.rs` |
| Key storage and redaction | `crates/zarn-config/src/auth.rs`, `guard.rs` |
| Provider kinds catalog | `crates/zarn-config/src/catalog.rs` |
| Runtime assembly (`plan`, `assemble`) | `crates/zarn-config/src/runtime.rs` |
| System prompt, `ZARN.md`, skills discovery | `crates/zarn-config/src/context.rs` |
| Doctor checks | `crates/zarn-config/src/doctor.rs` |
| Built-in tools | `crates/zarn-tools/src/` (`builtin_list()` in `lib.rs`) |
| Permissions (`Policy`, `Permissions`) | `crates/zarn-tools/src/permissions.rs` |
| MCP client and `McpServer` | `crates/zarn-tools/src/mcp.rs` |
| Skill tool | `crates/zarn-tools/src/skill.rs` |
| TUI controller, slash commands, event loop | `crates/zarn-tui/src/run.rs` (`Controller`) |
| TUI app state, key handling, popup logic | `crates/zarn-tui/src/state.rs` |
| Settings panel | `crates/zarn-tui/src/settings.rs`, drawn by `views.rs` |
| First-run setup | `crates/zarn-tui/src/onboarding.rs` |
| `/` popup entries (`BUILTINS`) | `crates/zarn-tui/src/popup.rs` |
| Scrollback rendering | `crates/zarn-tui/src/render.rs` |
| CLI definitions and subcommands | `crates/zarn/src/cli.rs`, `main.rs`, `commands.rs` |
| Secret scanning | `scripts/secret-scan.sh`, `scripts/secret-patterns.txt`, `.githooks/pre-commit` |

Design principles already in place, which you must keep:

- Every setting edit goes through `ops.rs` and `Layers::save`, which writes to the file that owns the setting (project or user) and refuses to write a config that fails `validate()`.
- The panel, the slash commands and the CLI share `ops.rs`. A new setting gets all three, not just one.
- Every edit calls `Controller::rebuild()` so it takes effect without a restart.
- Anything new that appears in the `/` popup is added to `popup::BUILTINS` and handled in `Controller::command`.
- Anything new that needs the panel gets a tab or rows in `settings.rs`, with a snapshot test.

## How to report

Every phase below has a numbered list of required assertions. When you finish a phase, your report must address each assertion **by number**, either as implemented (with the test that proves it) or as skipped (with the reason). A reviewer will check the report against this list and flag any assertion that was neither implemented nor declared skipped. Silence on a number counts as a failure.

Finish the whole task with: the output tail of clippy, test and release build, the list of things you could not verify, and the list of dependencies added or requested.

---

## Phase 0: foundation fixes

Small items that fix things that currently look broken or silent, and that later phases rely on.

**Context.** `runtime.rs` builds the subagent `ProviderFactory` as `Arc::new(move |_model| shared.clone())`, so a subagent's `model:` is ignored. `CostTracker` is created with `CostTable::empty()` in `run.rs`, so cost always shows $0.00. `compact::should_compact` exists but only the manual `/compact` calls compaction. `FallbackProvider` switches provider with only a `tracing::warn`. The `edit` tool checks `Ctx::was_read` but not whether the file changed afterwards. `Controller::submit` rejects input while streaming. Edits to `ZARN.md` are not noticed until the next `rebuild()`.

**Required assertions**

0.1 A subagent whose definition names a model uses that model. The factory builds a provider for the requested model, falling back to the main provider only when no model is named. Test: two mock servers, an agent with `model: x`, assert the request body carries `x`.

0.2 When the fallback chain moves to another provider, the user sees one notice naming both ("anthropic rate-limited, answered by openai"). The notice appears in the TUI scrollback and on stderr in `--print`. It appears once per switch, not once per event. `FallbackProvider` may expose the switch through a channel or callback in `zarn-providers`; do not change the `Provider` trait.

0.3 A `[pricing]` config section maps a model-name prefix to input, output and cached prices per million tokens. Defaults for common models ship in code. User config overrides defaults. `CostTracker` is built from this table. `/cost` and the status line show real numbers for a priced model and say "no rate configured" for an unpriced one. The "cost display" toggle still hides it.

0.4 Auto-compaction. When `usage_ratio` reaches the trigger after a turn, the controller compacts before the next turn and says so in the scrollback. It can be turned off in the General tab. A conversation too short to split is left alone.

0.5 A context meter in the status line shows tokens used over the context window as a percentage, and `/context` prints the breakdown (system prompt, memory, skills index, tools, messages).

0.6 The `edit` and `write` tools refuse to modify a file whose modification time or content hash changed after the agent last read it, with an error telling the model to read it again. Test: read, modify on disk, edit, assert refusal; read again, edit, assert success. Do this inside `zarn-tools` using `Ctx` as it is; do not edit `tools.rs`. If `Ctx` cannot carry the extra state without editing `tools.rs`, keep the state in a side table inside `zarn-tools` and say so.

0.7 Messages submitted while a turn is streaming are queued, shown as queued in the scrollback, and sent in order when the turn ends. `esc` cancels the turn and keeps the queue; a second action clears it. Test through `App` key handling.

0.8 A change to `ZARN.md` (project or user) or a skill file is picked up on the next turn without a restart. Test with a temp directory.

0.9 `ctrl+r` stays "expand last tool output" for now, but the key lives in one table (see Phase 4, item 4.4) so it can be rebound later. If you are not doing 4.4 yet, only note this.

report each by number, or say which you skipped.

---

## Phase 1: modes and per-task model routing

### Design decisions (already made, change only if they do not work)

**Modes.** A mode is a named bundle of: an extra system-prompt section, a permission policy overlay, an optional tool allowlist, and an optional profile or model. Modes are stored as Markdown files with frontmatter, the same shape as agents and skills:

```
---
name: review
description: read-only code review
tools: read, glob, grep
permissions: edit=deny, write=deny, bash=ask
profile: default
model: 
---
Review the code. Do not modify files. Report findings by severity.
```

Locations: `~/.config/zarn/modes/*.md` (user) and `./.zarn/modes/*.md` (project, wins on name clash). The config chooses the startup mode with `general.mode`. Built-in modes exist in code and cannot be deleted: `default`, `plan`, `accept-edits`, `bypass`. A custom file with a built-in name is rejected with a clear error.

**Routing.** A `[routing]` section:

```toml
[routing]
enabled = true

[routing.roles.explore]
provider = "groq"
model = "llama-3.3-70b"

[routing.roles.compact]
provider = "anthropic"
model = "claude-haiku-5-5"
```

Roles: `main`, `subagent`, `explore`, `plan`, `compact`, `title`, `init`, `review`. A role with no entry, or any role when `enabled = false`, uses the main model. A role whose provider has no key or fails its test falls back to main, with a visible notice (reuse 0.2).

**Required assertions**

1.1 The four built-in modes and custom modes from both directories load. Project overrides user on a name clash. A custom file using a built-in name is rejected with an error that names the file. A malformed file is skipped with a warning and does not stop startup.

1.2 `shift+tab` cycles modes in the chat view. `/mode` with no argument opens a picker, `/mode <name>` sets it. The active mode is shown in the status line. The choice persists to `general.mode` through `ops.rs`. Add `mode` to `popup::BUILTINS`.

1.3 `default`: current behaviour. `accept-edits`: `edit` and `write` are allowed without prompting, others unchanged. `bypass`: every tool allowed, and entering it requires an explicit confirmation naming the risk; `--print` refuses bypass unless `--permission-mode bypass` is given. Tests prove each policy through `Permissions`.

1.4 `plan` mode: the model may only use tools with `Risk::Safe`, plus a new `exit_plan` tool. The `exit_plan` tool takes a plan string, shows it to the user for approval, and on approval switches the mode to the one the user picks (default or accept-edits) and continues. On rejection it stays in plan mode and passes the user's feedback back. Implement `exit_plan` as a `Tool` registered through `ToolRegistry`; do not edit `tools.rs`. Write tests with a scripted mock provider that calls `write` in plan mode and is denied, then calls `exit_plan`.

1.5 A mode's `tools` list restricts the registry for that mode. A mode's `permissions` overlay is applied on top of the configured permissions and removed when the mode changes. A mode's `profile` or `model` switches the runtime on entry and restores it on exit.

1.6 A Modes tab in `/config` lists modes (built-in and custom, with source), lets you create, edit and delete custom ones with a form, and shows the active mode. Snapshot test. Edits write the `.md` file in the chosen scope; they never touch `config.toml` except for `general.mode`.

1.7 `[routing]` parses, validates and merges across project and user layers. `validate()` rejects a role that names a provider that is not configured, naming the role and provider.

1.8 With routing enabled, each role uses its configured provider and model. Test with a mock server per role: subagents hit the `subagent` server, compaction hits the `compact` server, titles hit `title`, and the main loop hits `main`. With routing disabled, all of them hit `main`, even when roles are configured.

1.9 A toggle in the Models tab turns routing on or off, takes effect immediately, and persists. The Models tab lists each role with its provider and model, lets you pick them from the configured providers (and from discovered models once Phase 2 exists), and lets you clear a role.

1.10 A role whose provider has no key, or whose request fails with a transient error, falls back to the main model for that call, and the user sees a notice (reuse 0.2). Test the no-key case without a network.

1.11 `zarn modes list`, `zarn modes show <name>` and `--mode <name>` exist on the CLI. `--print` honours `--mode`.

1.12 Session titles: after the first assistant reply, a short title is generated with the `title` role (or main) and stored in the session metadata. `/resume` shows titles. Title generation never blocks the UI and never runs when it would fail for lack of a key.

report each by number, or say which you skipped.

---

## Phase 2: model and quota discovery, and provider operations

**Constraint.** The `Provider` trait cannot gain methods. Put discovery in a new module, `crates/zarn-providers/src/discovery.rs`, as free functions and a small struct, called by `ops.rs`. It makes its own HTTP calls with `reqwest`. It must not go through `Provider::stream`.

**Honesty about quotas.** Most providers do not expose a balance to an ordinary API key. Collect what is available and say plainly when something is not:

- **Model list:** each provider's list endpoint (Anthropic `/v1/models`, OpenAI-compatible `/models`, Google models list, Ollama `/api/tags`). Include context window, tool support and vision support where the response carries them; otherwise leave them unknown. Do not invent values.
- **Rate limits:** read the `*-ratelimit-*` response headers from a one-token probe request, where the provider returns them.
- **Credit or balance:** only for providers that offer an endpoint for it (for example OpenRouter's key-info endpoint). Otherwise report "not exposed by this provider".
- Never print a guessed quota. Every field is either a value from the response or "unknown".

**Required assertions**

2.1 `discovery::models(spec)` returns the available models for each supported kind, parsed from a mock server response in that provider's real shape. One test per kind with a recorded fixture body. A 401 yields an error that names the provider and never contains the key.

2.2 `discovery::limits(spec, model)` sends a minimal probe and returns the rate-limit values found in the response headers (requests and tokens, limit, remaining, reset), or "not exposed". Test with a mock that sets the headers and one that sets none.

2.3 `discovery::credits(spec)` returns the balance and limit for providers that offer an endpoint, and `NotExposed` otherwise. Test both.

2.4 Results are cached per provider in the user data directory with a timestamp, reused for a configurable time (default 10 minutes), and refreshed on demand. The cache holds no key. A test asserts that no key string appears in the cache file.

2.5 `/models` prints, per configured provider with a key, the models and the quota information, using the cache unless `/models refresh` is given. Add it to `popup::BUILTINS`. A provider that fails shows its error on its own row and does not stop the others. Discovery for multiple providers runs concurrently with a timeout per provider.

2.6 A Models/Quota tab in `/config` shows the same data, refreshes with a key, and shows a spinner while loading. Snapshot test for loaded, loading and failed states.

2.7 `/model` and the Models tab pickers use discovered models when available, marked with their context window, and still accept a typed name that is not in the list.

2.8 `zarn models [--provider NAME] [--refresh]` prints the same information on the CLI.

2.9 Spend caps. `[limits]` supports `session_usd` and `day_usd`. At 80 percent a notice appears; at 100 percent the turn stops with a message saying which cap was hit and how to change it. The daily total is stored locally without keys. Caps only apply to models with a known price (0.3); an unpriced model is never silently treated as free, and the notice says the cap cannot be enforced for it.

2.10 `apiKeyHelper`. A provider may set `api_key_helper = "command"`. The key resolution order becomes env var, helper, credential store, fallback file. The helper's stdout is the key, trimmed. It runs without a shell where possible, with a timeout, and its output is registered for redaction. A helper that fails produces an error that names the provider and the exit status, not its output. Config files still reject a literal `api_key`. Tests use a small script that prints a fake key.

2.11 Provider health. The Providers tab shows, per provider, the last test result with its age, the last measured latency, and a failure count since start. Data comes from real requests the app already made plus explicit tests. No background polling.

2.12 Quick profile switch: a keybinding (default `ctrl+p`) opens the profile picker in the chat view.

2.13 Compare. `/compare <provider-a> <provider-b> <prompt>` sends the same prompt, without tools, to both, and shows the answers side by side with latency and token counts. Add to `popup::BUILTINS`. Each side has its own error handling.

2.14 Local-model help. If a provider with `tools = "native"` returns tool calls that fail to parse three times in a row, suggest switching the provider to `tools = "prompted"`, offer it as a one-key action, and write the change through `ops.rs`. For Ollama, read the real context window from the model info endpoint (the existing `OllamaProvider::context_window`) and use it for the context meter.

report each by number, or say which you skipped.

---

## Phase 3: trust and control

**Required assertions**

3.1 Diff previews. When `edit` or `write` asks for permission, the prompt shows a unified diff (added lines green, removed red, with line numbers and a few lines of context) instead of raw arguments. For a new file it shows the content with a "new file" header. Large diffs are truncated with a count and expandable. The diff is computed without writing the file. Snapshot tests for edit, new file, and truncated diff.

3.2 Pattern permission rules. Permissions accept entries like `bash = "allow"` and rule lists such as `allow = ["bash(git diff:*)", "bash(cargo test:*)", "edit(src/**)"]`, `deny = ["bash(rm -rf:*)"]`, `ask = [...]`. Deny beats ask beats allow. A tool-wide setting still works. Matching is by command prefix for `bash` and by glob on the path argument for file tools. Tests cover precedence, prefix matching, and path globs.

3.3 "Always allow" in the permission prompt offers three choices: this call, this exact pattern for the session, and this pattern saved to config (project or user, chosen). Saved rules are written through `ops.rs`. The Permissions tab lists rules and lets you delete them.

3.4 Safe shell commands. Read-only commands (`ls`, `cat`, `pwd`, `git status`, `git diff`, `git log`, `grep`, `rg`, `find` without `-exec` or `-delete`, `head`, `tail`, `wc`, `which`) run without prompting under the default policy. The check parses the command first: any of `;`, `&&`, `||`, `|` to a non-safe command, `$(`, backticks, redirections to files, `>`, `>>`, or `-exec` makes it fall back to the normal policy. Table-driven tests with at least 40 cases, half of them adversarial. The safe list is configurable and can be turned off.

3.5 Hooks. `[hooks]` supports `pre_tool`, `post_tool`, `prompt_submit` and `turn_end`, each a list of `{ match = "edit|write", command = "..." }`. The hook receives a JSON description of the event on stdin. A `pre_tool` hook that exits non-zero blocks the call and its stderr is shown to the model as the reason. Hooks time out (default 30 seconds, configurable). Hook output is redacted. A panel tab lists hooks, and `zarn hooks list` exists. Implement blocking through the `ToolGate` trait in `agent.rs`, not by editing the agent loop. Tests with real small scripts.

3.6 Checkpoints. Before each user turn, record which files the turn's tools modify (copy-on-first-write into the data directory, keyed by session and turn). `esc esc` (or `/rewind`) opens a list of turns; choosing one restores the files and truncates the conversation to that point, after showing what will change and asking to confirm. Files created in the turn are removed on rewind. Files changed outside zarn since the checkpoint are reported and skipped, never overwritten. Snapshots are excluded from `.gitignore`d paths only if they are ignored; a size cap skips huge files with a notice. Tests restore modified, created and deleted files.

3.7 Bash sandbox (best effort, off by default). A `sandbox` setting runs `bash` with a restricted working directory and a scrubbed environment (keys and tokens in the environment are removed). Where an OS sandbox is not available, say so in `/doctor` instead of claiming protection. Do not claim isolation you cannot verify. The scrubbed environment is tested: set a fake key in the parent environment, run `env` through the tool, assert it is absent.

3.8 `/permissions` shows rules, the active mode's overlay, and what each tool resolves to right now. Snapshot test.

report each by number, or say which you skipped.

---

## Phase 4: input

**Required assertions**

4.1 `@` mentions. Typing `@` in the input opens the same style of popup as `/`, listing files under the working directory (respecting `.gitignore`, using the `ignore` crate already in the workspace), filtered fuzzily as you type. Choosing one inserts the path. On submit, mentioned files are attached to the message as file contents with a size cap and a note when truncated. Paths outside the working directory are refused. Tests for filtering, `.gitignore`, and the cap.

4.2 Image and PDF input. Pasting an image path or using `@image.png` attaches it as `Content::Image` for providers whose `caps().vision` is true, and shows a clear notice otherwise. PDF text is extracted if it can be done without a new dependency; if not, report the dependency needed and skip.

4.3 Inline shell and quick memory. A line starting with `!` runs the command through the `bash` tool path (so permissions apply) and shows the output without sending it to the model, with an option to add it to context. A line starting with `#` appends the rest to the project `ZARN.md` (asking which file if both exist), then confirms. Tests for both, including that `#` never writes outside the chosen file.

4.4 Keybindings. All chat-view key bindings live in one table with action names. `~/.config/zarn/keybindings.toml` overrides it. Conflicts are reported in `/doctor`. `ctrl+r` stays "expand output"; add history search on a different default key (`ctrl+f`) with an incremental search popup over input history. Tests for override and conflict detection.

4.5 External editor. `ctrl+g` writes the current input to a temp file, opens `$VISUAL` or `$EDITOR`, and reads the result back on exit. If no editor is set, say so. The temp file is removed. The terminal is restored correctly after the editor exits.

4.6 Large paste collapsing. A paste over 20 lines or 2,000 characters is shown as `[Pasted text #N +M lines]` and expanded when sent. Typing around it works, and deleting the token removes the paste. Tests through `App::paste`.

4.7 Optional vim input mode, off by default, toggled in General. Support at least normal/insert modes, `h j k l w b 0 $ x dd i a o` and `esc`. Document the supported keys in `/help`. If this is too large, declare it skipped.

report each by number, or say which you skipped.

---

## Phase 5: feedback and display

**Required assertions**

5.1 Todo tool. A `todo` tool (registered through `ToolRegistry`) lets the model write and update a checklist with `pending`, `in_progress` and `done` items. The TUI renders the current list in a fixed region above the input while it is non-empty, and updates live. The list is stored in the session. Tests for the tool and a snapshot.

5.2 Per-tool cards. `read` shows path and line range, `grep` a match count and the pattern, `glob` a file count, `bash` the command and live streamed output (see 5.3), `edit` and `write` a diff summary (`+12 -3`), `task` the agent name and its status. Unknown and MCP tools keep the generic card. Snapshot per tool kind.

5.3 Streaming bash output. The `bash` tool streams output to the card as it arrives instead of at the end. Since the `Tool` trait returns once, do this by having the TUI controller read the output through a side channel owned by `zarn-tools` (for example a registered progress sink keyed by call id). Do not edit `tools.rs`. If this cannot be done without changing the trait, skip and report.

5.4 Status spinner shows elapsed time and a live output-token count while streaming. A thinking/reasoning display is shown collapsed by default when the provider supplies it, expandable with a key. If the existing `Event` enum has no reasoning variant, skip the thinking display and say that it needs a trait change.

5.5 Transcript view. `ctrl+o` toggles a view that shows every tool output in full, scrollable, and returns to the normal view without losing position.

5.6 Notifications. When a turn that ran longer than 10 seconds finishes, or a permission prompt appears while the terminal may be unfocused, ring the terminal bell and, where the terminal supports it, set the title. A General toggle controls it. Never emit control sequences when stdout is not a terminal.

5.7 Status line configuration. `status_line` in General is a template using fields (`{model}`, `{mode}`, `{profile}`, `{context}`, `{cost}`, `{cwd}`, `{branch}`), editable in the panel with a live preview. An invalid template shows an error and keeps the previous one.

5.8 Themes. `theme` accepts `dark`, `light`, `colorblind` and `auto`. `auto` uses `COLORFGBG` or terminal background detection where available and falls back to dark. Every hard-coded `Color::DarkGray` for body text in `render.rs`, `views.rs`, `popup.rs` and `markdown.rs` is replaced by theme roles so light terminals stay readable. Snapshots for light and dark.

5.9 Mouse scrolling in the scrollback, and `/` search within the scrollback (`ctrl+s` to search, `n`/`N` to move). Mouse capture is off by default because it breaks native selection; it is a General toggle and `/doctor` explains the trade-off.

5.10 `/export [path]` writes the conversation to Markdown with keys redacted, and `/copy` copies the last assistant reply to the clipboard where the platform allows it, otherwise prints it. Sessions can be renamed with `/rename`, and `/resume` has a type-to-filter picker over titles and first messages.

report each by number, or say which you skipped.

---

## Phase 6: subagents and extensibility

**Required assertions**

6.1 Parallel subagents. The model can issue several `task` calls in one turn and they run concurrently, bounded by a configurable limit (default 4). Each running agent gets a live row in the TUI with its name, current tool, elapsed time and tokens. Results return in the order the model asked. A failing agent returns its error to the model without cancelling the others. Test with mock providers that sleep, asserting concurrency by timing.

6.2 Background agents. A `task` call may set `background = true`; it returns immediately with an id, and `/tasks` lists running and finished background agents with their results. Finished results are delivered to the model on its next turn. Cancelling from `/tasks` works.

6.3 Custom commands. `.zarn/commands/*.md` (and the user equivalent) support frontmatter: `description`, `argument-hint`, `allowed-tools`, `model`. Body supports `$ARGUMENTS`, `$1`..`$9`, `` !`command` `` substitution (run through the permission system, output inserted), and `@path` file includes. Descriptions and argument hints appear in the `/` popup. Unsafe `!` substitution is gated by permissions exactly like the `bash` tool. Tests for each substitution and for the gate.

6.4 Nested memory. A `ZARN.md` in a subdirectory is added to context the first time the agent reads or edits a file under that directory. `@path/to/file.md` lines inside a `ZARN.md` import that file, with a depth limit of 5 and cycle detection. The token budget from Phase 0 still applies. Tests for nesting, imports, cycles and the budget.

6.5 MCP resources and prompts. `@server:resource` mentions resolve to MCP resources. MCP prompts appear as `/server:prompt` commands in the popup. Tool-list changes from a server (`list_changed`) refresh its tools without a restart. Tests against the existing mock stdio server, extended to serve resources and prompts.

6.6 MCP over HTTP with OAuth. Remote servers that require authorization run a device or browser-based flow from `/mcp`, and tokens are stored in the credential store under `zarn-mcp:<server>` (never in config or logs, and registered for redaction). If you cannot test against a real OAuth server, test the token handling against a mock and say the real flow is unverified.

6.7 Lazy MCP tools. When the combined tool count exceeds a threshold (default 40), MCP tools are not all sent in every request. Instead a `tool_search` tool lets the model find and load them by keyword. Test the threshold and the search.

6.8 Plugins. A plugin is a directory with `plugin.toml` listing commands, agents, skills, modes, hooks and MCP servers it contributes. `zarn plugin add <path-or-git-url>`, `list`, `remove`, and a Plugins tab. Plugin hooks and MCP servers are shown to the user for approval before they are enabled, because they run code. Plugin content is namespaced by plugin name to avoid clashes. Installing never executes plugin code.

6.9 More tools, each registered through `ToolRegistry`: `webfetch` (HTTP GET with size and time limits, HTML converted to text, `Risk::Execute`, prompts by default), `websearch` only if a search backend can be configured by the user without shipping any default key (otherwise skip and report), `multiedit` (several edits to one file applied atomically), `notebook_edit` for `.ipynb`, and background bash (`bash` with `background = true`, plus `bash_output` and `bash_kill`). Tests for each, including that web content is clearly delimited in the tool result so it cannot masquerade as instructions.

report each by number, or say which you skipped.

---

## Phase 7: workflow and scripting

**Required assertions**

7.1 Headless output. `--output-format text|json|stream-json` for `--print`. `json` returns one object with the final text, tool calls, usage and cost. `stream-json` emits one JSON object per line per event. Neither format ever contains a key. Tests parse the output.

7.2 Scripting flags: `--allowed-tools`, `--disallowed-tools`, `--permission-mode`, `--mode`, `--max-turns`, `--add-dir` (repeatable; extra directories the file tools may touch), `--system-prompt-file`, `--append-system-prompt`, `--no-session-persistence`. Each has a test. Reading the prompt from stdin works when `--print` has no argument and stdin is not a terminal.

7.3 `--fork-session` and `/fork` start a new session from an existing one's history without modifying the original.

7.4 Git commands: `/review` (review the working-tree diff), `/commit` (draft a message from the staged diff, show it, commit only on confirmation), `/pr` (draft a title and body from the branch diff; do not call any hosting API). They run through the normal tools and permissions. `/commit` never commits without confirmation and never adds an attribution line unless the user's config asks for one.

7.5 Git worktrees. `zarn --worktree <name>` creates `.zarn/worktrees/<name>` on a new branch and runs there; subagents can be given their own worktree with `isolation = "worktree"` in their definition. Worktrees with changes are never deleted automatically. Tests use a temp git repository.

7.6 Scheduling. `/loop <interval> <prompt>` re-runs a prompt on an interval within the session, and `zarn schedule add|list|remove` stores prompts that an external scheduler (cron, Task Scheduler) can fire with `zarn run-scheduled <id>`. zarn does not install system schedulers itself. Scheduled runs are headless and use the headless permission policy.

7.7 `config.local.toml`: `./.zarn/config.local.toml` is merged above the project file, is covered by the existing `.zarn/` ignore rule, and is where `ops.rs` writes when the user picks "just for me". Add it as a scope in `layers.rs` and the panel's scope selector.

7.8 A GitHub Action example in `docs/` that runs `zarn --print` with a key from repository secrets, documented as an example. No workflow file that runs automatically is added.

report each by number, or say which you skipped.

---

## Phase 8: known gaps

**Required assertions**

8.1 Panel changes while a request is in flight: the current turn finishes on the old provider and the next turn uses the new one. The user is told. Test with a slow mock server.

8.2 MCP `env` and `headers` values accept references instead of literals: `env:NAME` reads an environment variable at start, and `key:NAME` reads from the credential store. `config.toml` never stores a literal that looks like a secret; `validate()` rejects values matching the secret patterns in `scripts/secret-patterns.txt`. The panel's MCP form can set these references. Tests prove a literal key is rejected and a reference is resolved.

8.3 Windows: no new code path may assume `/` separators, a `HOME` variable, or a POSIX shell. Where a feature needs a shell, use `cmd /c` or PowerShell on Windows the same way `mcp.rs` already does. Add a note in the report listing every Windows-specific path you could not run.

8.4 `/doctor` and `zarn doctor` cover every new subsystem: modes, routing, discovery cache, hooks, plugins, keybinding conflicts, sandbox availability, spend caps. Each check says what is wrong and the command that fixes it.

8.5 Documentation. Update `README.md` with a short section per user-facing feature, and keep the "where your key is stored" section accurate. Document every new config key with an example. No document contains a real-looking key.

8.6 Final verification: `cargo clippy -- -D warnings`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo build --release`, `scripts/release.sh`, `sh scripts/secret-scan.sh --tree`, and `sh scripts/secret-scan.sh --history`. Paste the tails. The history scan must report no hits; if it does, fix the fixture, do not edit the patterns.

report each by number, or say which you skipped.

---

## Suggested order and sizing

1. Phase 0 (all of it). Small, and later phases rely on 0.1, 0.2 and 0.3.
2. Phase 1. Build the mode and routing model once; everything in Phase 3 plugs into it.
3. Phase 2, starting with 2.1 to 2.5, then the rest.
4. Phase 3, starting with 3.1, 3.2 and 3.4, which remove the most friction, then hooks, then checkpoints.
5. Phases 4 and 5 in any order.
6. Phase 6, with 6.1 and 6.3 first. 6.6 and 6.8 are the largest.
7. Phases 7 and 8.

Large items you may split across several commits: 2.3 to 2.6, 3.6, 5.2, 6.1, 6.5 to 6.8. Items marked optional in their text (4.2 PDF, 4.7, 5.4 thinking, 6.9 websearch) may be declared skipped with a reason. Everything else must be implemented or declared skipped by number.

## Final report format

```
Phase N
  N.1 implemented: <file>, test <test name>
  N.2 skipped: <reason>
  ...
Verification: <clippy / test / release tails>
Could not verify: <list>
Dependencies added or requested: <crate version, reason>
```

Report each by number, or say which you skipped.
