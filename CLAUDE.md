# zarn

Rust workspace, edition 2024. Agent CLI.

- `crates/zarn-core` — traits and shared types
- `crates/zarn-providers` — `Provider` implementations per backend
- `crates/zarn-tools` — `Tool` implementations
- `crates/zarn-tui` — terminal UI
- `crates/zarn-config` — config loading
- `crates/zarn` — binary

## Frozen interfaces

`crates/zarn-core/src/provider.rs` and `crates/zarn-core/src/tools.rs` define
frozen interfaces. Only the repository owner changes them. Agents and
contributors implement against these traits; they never edit, extend or
reformat those two files. If an implementation seems to require a trait
change, stop and report it instead.

## Conventions

No code comments.

## Subagents

Subagents never edit any `Cargo.toml`. If an implementation needs a new
dependency, report which crate and version in the final message and stop;
the repository owner adds it. A `PreToolUse` hook in `.claude/settings.json`
enforces this for any agent working outside the project root.

Subagents never run a bare `zarn`. It starts the interactive TUI, blocks
forever and holds a lock on the built binary. Use `zarn --print "<prompt>"`.

## Delegated prompts

Every delegated prompt carries a numbered list of required assertions and
ends with: "report each by number, or say which you skipped." The reviewer
checks the report against that list and flags any assertion that was
neither implemented nor declared skipped.
