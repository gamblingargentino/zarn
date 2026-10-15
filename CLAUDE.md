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
