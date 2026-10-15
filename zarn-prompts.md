# zarn — Prompt Pack

Every prompt you need, in order. Run the Opus-tier ones in your main Claude Code session. The ones marked **[delegate]** go to a subagent.

Rule of thumb: you run anything that touches `zarn-core`. Everything else gets farmed out.

---

## Phase 0 — Setup

### 0.1 Workspace

```
Set up a Rust workspace called zarn — an agent CLI.

Crates: zarn-core, zarn-providers, zarn-tools, zarn-tui,
zarn-config, zarn (binary). Edition 2024.

Dependencies at workspace level: tokio (full), reqwest with rustls
and stream features, serde, serde_json, async-trait, futures,
ratatui, crossterm, clap (derive), toml, tiktoken-rs, ignore,
tracing, tracing-subscriber, anyhow, thiserror.

zarn-core must not depend on zarn-tui. Nothing depends on the
binary crate.

Add a CLAUDE.md stating:
- zarn-core/src/provider.rs and zarn-core/src/tools.rs define
  frozen interfaces. Subagents must never modify them.
- No code comments anywhere in this project.
- Every change must pass `cargo clippy -- -D warnings` and
  `cargo test` before being reported as done.

Don't write any implementation yet.
```

### 0.2 Subagents

```
Create these subagents in .claude/agents/:

adapter-writer (model: haiku, tools: Read, Write, Edit, Bash)
  Implements the Provider trait for exactly one backend per
  invocation. Writes one file. Never modifies zarn-core. Runs
  cargo test before reporting done.

tool-writer (model: haiku, tools: Read, Write, Edit, Bash)
  Implements the Tool trait for exactly one built-in tool per
  invocation. Same constraints as adapter-writer.

config-writer (model: haiku, tools: Read, Write, Edit, Bash)
  Writes config parsing and CLI argument handling.

reviewer (model: sonnet, tools: Read, Grep, Glob)
  Reviews a diff. Flags any change to a trait definition, any new
  pub item in zarn-core, any concrete type swapped for Box<dyn>,
  and any code comment. Reports findings only, makes no edits.

If .claude/agents/ didn't exist before, restart Claude Code after.
```

---

## Phase 1 — Core

### 1.1 Types

```
In zarn-core/src/types.rs, define the message types.

enum Message with variants System(String), User(Vec<Content>),
Assistant { content: Vec<Content>, tool_calls: Vec<ToolCall> },
ToolResult { id: String, content: String, is_error: bool }.

enum Content with Text(String) and Image { mime: String, data: Vec<u8> }.

struct ToolCall { id, name, args: serde_json::Value }.

struct ToolSpec { name, description, schema: serde_json::Value }.

struct Request { messages, tools, model, max_tokens, temperature }.

enum StopReason { EndTurn, ToolUse, MaxTokens, Cancelled, Refusal }.

All Serialize + Deserialize + Clone + Debug. These are the only
message shapes the rest of the codebase knows about — no provider
types leak past the adapter boundary.
```

### 1.2 Provider trait

```
In zarn-core/src/provider.rs, define the Provider trait.

enum Event { TextDelta(String), ToolCallStart { id, name },
ToolCallDelta(String), ToolCallEnd, Usage { input, output, cached },
Done(StopReason), Error(ProviderError) }

enum ToolSupport { Native, Prompted, None }

struct Caps { tools: ToolSupport, vision: bool, streaming: bool,
caching: bool, context_window: u32, max_output: u32 }

#[async_trait]
trait Provider: Send + Sync {
    async fn stream(&self, req: Request)
        -> Result<BoxStream<'static, Event>, ProviderError>;
    fn caps(&self) -> Caps;
    fn count_tokens(&self, msgs: &[Message]) -> Result<u32, ProviderError>;
}

ProviderError should distinguish RateLimit, Auth, Network, BadRequest,
and Other — the fallback chain branches on this, so the distinction
has to be real.
```

### 1.3 Tool trait

```
In zarn-core/src/tools.rs, define the Tool trait.

enum Risk { Safe, Write, Execute }

struct ToolOutput { content: String, is_error: bool }

struct Ctx holding the working directory and a set of files that
have been read this session.

#[async_trait]
trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn risk(&self) -> Risk;
    async fn run(&self, args: Value, ctx: &Ctx) -> Result<ToolOutput>;
}

Also a ToolRegistry that holds Box<dyn Tool>, looks up by name, and
produces the Vec<ToolSpec> for a Request.
```

### 1.4 First adapter

```
Implement OpenAiCompatProvider in zarn-providers/src/openai.rs.

Takes a base_url and api_key so it works against OpenAI, Groq,
Together, OpenRouter, vLLM, llama.cpp server, and LM Studio
unchanged.

SSE streaming. Map deltas to our Event enum. Accumulate tool call
argument fragments across chunks — they arrive split.

This is the reference implementation every other adapter gets
compared against, so make the structure clean and obvious.
```

### 1.5 Loop

```
Write the agent loop in zarn-core/src/agent.rs.

loop {
  stream the request
  forward events to a caller-supplied sink
  collect tool calls
  if none, break
  run each tool, push results
}

Requirements:
- Takes Arc<dyn Provider> and a ToolRegistry.
- Events go out through a channel so the TUI and headless mode can
  both consume them.
- A cancellation token aborts mid-stream and preserves whatever
  text arrived as a partial assistant message.
- A max_iterations guard.
- Does not know what a terminal is.
```

### 1.6 Headless binary

```
Wire up a minimal binary: zarn --print "prompt" runs one loop
against the openai-compat provider using env vars for base_url and
key, prints text deltas to stdout, prints tool calls as one line
each.

This is the test harness for everything that follows. Keep it after
the TUI exists.
```

---

## Phase 2 — Tools

### 2.1 **[delegate]** tool-writer, one invocation each

```
Use the tool-writer subagent to implement the read tool in
zarn-tools/src/read.rs. Takes a path, optional offset and limit.
Returns contents with line numbers. Records the file in Ctx as read.
Truncates to a token budget with a marker showing what was cut.
Risk::Safe.
```

```
Use the tool-writer subagent to implement the edit tool in
zarn-tools/src/edit.rs. Takes path, old_string, new_string, and an
optional replace_all flag. Exact string replacement — never line
numbers. Errors if the file hasn't been read this session. Errors if
old_string appears zero times or more than once without replace_all.
Risk::Write.
```

```
Use the tool-writer subagent to implement the write tool in
zarn-tools/src/write.rs. Takes path and content. Errors if the file
exists and has not been read this session. Creates parent
directories. Risk::Write.
```

```
Use the tool-writer subagent to implement the bash tool in
zarn-tools/src/bash.rs. Takes a command and an optional timeout
defaulting to 120s. Streams stdout and stderr, merges them,
truncates to a token budget. Kills the process group on timeout.
Risk::Execute.
```

```
Use the tool-writer subagent to implement glob and grep in
zarn-tools/src/search.rs. glob takes a pattern and optional path,
returns paths sorted by modification time. grep wraps ripgrep if
present and falls back to the regex crate. Both respect .gitignore
via the ignore crate. Risk::Safe.
```

### 2.2 Permissions

```
Add a permission layer in zarn-tools/src/permissions.rs.

Per-tool policy: Allow, Ask, Deny. Resolved from config with a
per-session override map for "always allow this one".

The agent loop consults it before running any tool and calls a
caller-supplied async callback when the policy is Ask, so the TUI
can prompt and headless mode can auto-deny.
```

---

## Phase 3 — Adapters

### 3.1 **[delegate]** adapter-writer, one invocation each

```
Use the adapter-writer subagent to implement AnthropicProvider in
zarn-providers/src/anthropic.rs. Match the structure of openai.rs.
Content-block streaming format. Set caps().caching = true and add
cache_control breakpoints on the system prompt and tool definitions.
```

```
Use the adapter-writer subagent to implement GoogleProvider in
zarn-providers/src/google.rs, matching openai.rs. Gemini
generateContent streaming format.
```

```
Use the adapter-writer subagent to implement OllamaProvider in
zarn-providers/src/ollama.rs, matching openai.rs. Native Ollama API.
Model list from /api/tags. caps().context_window read from the model
info endpoint.
```

```
Use the adapter-writer subagent to implement ExecProvider in
zarn-providers/src/exec.rs. Spawns a configured command, writes the
Request as JSON to stdin, reads newline-delimited Event JSON from
stdout. This is the escape hatch for any backend without an adapter.
Document the wire format in the crate README.
```

```
Use the adapter-writer subagent to implement LlamaCppProvider in
zarn-providers/src/llamacpp.rs using llama-cpp-2. In-process, no
server. Put it behind a "local" cargo feature so the default build
doesn't need the native dependency.
```

### 3.2 Registry and fallback

```
In zarn-providers/src/lib.rs, add a factory that builds a
Box<dyn Provider> from a config entry by its `kind` field.

Add FallbackProvider: wraps an ordered list and advances to the next
on RateLimit, Auth, or Network errors only. Never on BadRequest or a
model refusal. Logs each switch through tracing.
```

### 3.3 Prompted tool calls

```
Implement the prompted tool-call fallback in
zarn-core/src/prompted.rs.

When caps().tools == Prompted, inject tool definitions into the
system prompt as XML and parse <tool_use> blocks out of the response
text, emitting the same ToolCallStart/Delta/End events as a native
provider.

The agent loop must not branch on this — it's handled at the adapter
boundary so everything upstream stays identical.

Write tests with recorded model output, including the malformed
cases: unclosed tags, two calls in one block, invalid JSON args.
```

---

## Phase 4 — TUI

### 4.1 Shell

```
Build the TUI shell in zarn-tui. ratatui + crossterm.

Layout: scrollback region, a status line showing model, token count,
and cost, and a multiline input box at the bottom.

Alternate screen, raw mode, restores the terminal on panic via a
hook. Async event loop consuming both crossterm events and the agent
event channel.
```

### 4.2 Rendering

```
Render agent events into the scrollback.

Text deltas stream in word by word. Tool calls render as a bordered
card: name, key arguments, then a status that goes pending → running
→ done or failed. Tool output is collapsed to three lines with an
indicator of how many were hidden.

Markdown in assistant text: bold, italic, inline code, and fenced
code blocks with syntax highlighting.
```

### 4.3 Interaction

```
Add input handling.

Esc cancels the current stream. Ctrl+C twice exits. Up and down
cycle through input history. Shift+Enter inserts a newline, Enter
submits. Page up and down scroll scrollback. Ctrl+R expands the
last collapsed tool output.

Permission prompts render inline with y / n / a options and block
the loop until answered.
```

---

## Phase 5 — Config and CLI

### 5.1 **[delegate]** config-writer

```
Use the config-writer subagent to implement zarn-config.

TOML. Sections: [providers.<name>] with a kind field and
kind-specific keys, [profiles.<name>] with provider, model,
fallback, [permissions] with per-tool policy.

Resolution order: CLI flags, then ./.zarn/config.toml, then
~/.config/zarn/config.toml. Later sources fill gaps, they don't
replace whole sections.

API keys only ever come from api_key_env, never inline. Reject a
config with a literal key and say why.
```

### 5.2 **[delegate]** config-writer

```
Use the config-writer subagent to implement the CLI in zarn/src/cli.rs
with clap derive.

zarn                      interactive TUI
zarn --print "..."        one-shot, stdout
zarn --continue           resume last session
zarn --resume <id>        resume specific
zarn --model <m>          override
zarn --profile <p>        override
zarn config init          write a starter config
zarn config check         validate and show resolved values
zarn providers            list configured providers and reachability
```

---

## Phase 6 — Sessions and context

### 6.1 Persistence

```
Add session persistence in zarn-core/src/session.rs.

JSONL under ~/.local/share/zarn/sessions/<project-hash>/<id>.jsonl.
One record per message. Append as you go so a crash loses at most
the current turn.

Each session stores cwd, profile, model, created and updated times,
and cumulative token usage.

Implement --continue as "newest session for this cwd".
```

### 6.2 Compaction

```
Implement compaction in zarn-core/src/compact.rs.

Triggers at 80% of caps().context_window. Summarizes everything
except the last N turns into a single system message. The summary
must preserve: files read and their paths, decisions made,
unresolved problems, and the user's stated goal.

Never compacts the first user message or anything from the last
three turns.

Expose it as /compact as well as automatic.
```

### 6.3 Cost

```
Add cost tracking in zarn-core/src/cost.rs.

Per-model input, output, and cached rates in a table that's
overridable from config. Accumulate from Usage events. Show
session total in the status line, with a per-turn breakdown
available under /cost.
```

---

## Phase 7 — Extensions

### 7.1 MCP client

```
Add an MCP client in zarn-tools/src/mcp.rs.

stdio and SSE transports. Servers configured in
[mcp_servers.<name>]. Tools discovered at startup, namespaced as
<server>__<tool>, registered into the same ToolRegistry as the
built-ins.

A failing server logs a warning and is skipped — it never blocks
startup.
```

### 7.2 Subagents

```
Add subagent support in zarn-core/src/subagent.rs.

A `task` tool that spawns a nested agent loop with its own message
history, its own provider (so a cheap model can be used), and a
restricted tool set. Returns only its final message to the parent.

Definitions from .zarn/agents/*.md with YAML frontmatter: name,
description, tools, model.
```

### 7.3 Commands

```
Add slash commands in zarn-core/src/commands.rs.

Built-ins: /help /clear /compact /cost /model /resume /tools.

Custom commands from .zarn/commands/*.md — the file body becomes
the user message, with $ARGUMENTS substituted.
```

---

## Recurring

Keep these around; you'll use them constantly.

### Review before merging delegated work

```
Use the reviewer subagent on the current diff.
```

### When a subagent drifts

```
That changed the Provider trait. Revert zarn-core to HEAD and
reimplement the adapter against the existing trait. If the trait
genuinely can't express this backend, stop and tell me why instead
of changing it.
```

### Debugging an adapter

```
The <name> adapter drops tool calls that arrive split across chunks.
Add a test in zarn-providers/tests/ with recorded SSE chunks that
reproduces it, then fix it. Don't touch the other adapters.
```

### Adding a backend later

```
Use the adapter-writer subagent to implement <Name>Provider in
zarn-providers/src/<name>.rs, matching openai.rs. API docs: <url>.
Set caps() honestly — if it can't stream or doesn't support tools,
say so and let the fallback handle it.
```

### Before any release

```
Run cargo clippy -- -D warnings, cargo test, and cargo build
--release. Then check: does zarn-core still compile without
zarn-tui? Are there any code comments? Any pub items in zarn-core
that aren't part of the intended API?
```