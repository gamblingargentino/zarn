# zarn

An agent CLI for the terminal. Bring your own model provider and API key.

## Install

**Windows:** download `zarn-setup-<version>.exe` from the
[latest release](https://github.com/gamblingargentino/zarn/releases/latest)
and run it. It installs zarn with its icon, adds it to your PATH (optional),
creates Start Menu and desktop shortcuts, and registers an uninstaller. No admin
rights are needed. Then open a new terminal and run `zarn`.

**macOS and Linux:** download `zarn-<target>` from the same release, make it
executable and put it on your PATH:

```sh
chmod +x zarn-x86_64-unknown-linux-gnu
mv zarn-x86_64-unknown-linux-gnu ~/.local/bin/zarn
```

**From source:**

```sh
cargo install --path crates/zarn
```

### Updates

`zarn update` installs the newest release from GitHub over the running
binary (`zarn update --check` only looks), and `/update` does the same inside
zarn. Every download is checked against the release's SHA-256 checksums
before anything is replaced. zarn checks for a new release at most once a day
when it starts and tells you in the chat; turn that off with
`general.update_check = false` or `ZARN_NO_UPDATE_CHECK=1`. A private fork can
set `ZARN_UPDATE_REPO=owner/repo` and `GITHUB_TOKEN`.

### Publishing a release

Bump `version` in `[workspace.package]` in `Cargo.toml`, commit, then tag and
push the tag (run the two commands separately in Windows PowerShell):

```sh
git tag -a v0.2.0 -m "zarn 0.2.0"
git push origin v0.2.0
```

The `release` workflow builds the Windows installer, the Windows, Linux and
macOS binaries, and a `SHA256SUMS` file, and attaches them to the release.
`zarn update` installs from that release.

### Building the installer yourself

Install [Inno Setup 6](https://jrsoftware.org/isdl.php) once
(`winget install JRSoftware.InnoSetup`), then run from PowerShell:

```powershell
scripts\installer.ps1
```

It builds the release binary with the icon embedded in `zarn.exe`, then writes
`zarn-setup-<version>.exe` to `target\dist\installer\`. The icon lives in
`assets/icon/`; `python3 assets/icon/make_icon.py` redraws it. The embedding
needs the MSVC Rust toolchain (the default on Windows).

To build binaries without the installer, `scripts/release.sh` (macOS, Linux,
Git Bash) or `scripts/release.ps1` (PowerShell) writes them to
`target/dist/release/` with the build machine's paths remapped out.

## First run

zarn ships with no provider and no key. The first time you run `zarn` it opens
a setup flow. The first choice, "Set up in your browser", opens a page on
`http://localhost:9276` (the next free port up to 9299 if that one is busy):
paste the key and press Save. zarn recognises the provider from the key,
fills in its address, checks the key by listing the models it can use, stores
the key in your OS credential store, picks a model and opens the prompt. Every
listed model is then offered on the page, in `/model` and in the `/model `
completion list. You can also stay in the terminal: pick a provider, paste
your key (it is never shown on screen) and pick from the models the key can
use. Either way nothing is saved until the key works.

Built in, with their addresses filled in: Anthropic, OpenAI, Google Gemini,
OpenRouter, Groq, DeepSeek, Mistral, xAI, Together AI, Fireworks AI,
Cerebras, Moonshot (Kimi), Alibaba Qwen, Z.ai (GLM), Perplexity, NVIDIA NIM,
Hugging Face, Ollama and LM Studio. Any other OpenAI-compatible server only
needs a name and a base URL.

zarn works out the provider from keys with a unique format (`sk-ant-`,
`sk-or-`, `sk-proj-`, `gsk_`, `AIza`, `xai-`, `pplx-`, `nvapi-`, `hf_`,
`csk-`, `fw_`, `tgp_` and Z.ai keys). A plain `sk-` key could belong to
OpenAI, DeepSeek, Moonshot or Qwen, so zarn asks which one instead of
sending your key to all of them to find out.

The setup page only listens on 127.0.0.1, only works from the link zarn prints
(it carries a one-time token), refuses other hosts and other sites' forms, and
never shows a saved key again. Open it any time with `/login web` or
`zarn setup`; it closes when you click Done or after 30 idle minutes.

Everything after that is done from inside zarn, with no hand-edited files:

| Command | What it does |
| --- | --- |
| `/config` | settings panel: providers, models, skills, MCP, agents, modes, quota, permissions, hooks, plugins, general |
| `/login [provider\|auto\|web]`, `/logout [provider]` | add, replace or remove a key and load its models; `auto` works out the provider from the key, `web` opens the browser setup page |
| `/provider list\|add\|switch` | manage providers |
| `/model [name]`, `/models [refresh]` | pick a model; list every provider's models, limits and credits |
| `/profile` (ctrl+p) | pick a profile for this session |
| `/mode [name]` (shift+tab) | switch mode: default, plan, accept-edits, bypass, full or your own |
| `/skills list\|show\|new\|create\|install\|remove\|enable\|disable` | manage skills; `create` has the model write one with you, `install` takes a git URL, GitHub folder link, folder or `SKILL.md` link (`--project` for this project) |
| `/commands [list\|new <name>]` | list or scaffold your own slash commands |
| `/diff [path]` | show the uncommitted changes, untracked files included, colored and folded; nothing is sent to the model (`/attach-output` adds it to your next message) |
| `/system [show\|edit\|reset] [--project]` | show the system prompt, or replace it with your own |
| `/chain [show\|add\|remove\|set\|clear]` | the models to fall back to when one fails |
| `/rank [provider \| provider:model … \| save [n]]` | check models for speed and tool use, rank them, save the best as the chain |
| `/style [name]` | how replies are written: `default`, `explanatory`, `learning`, `concise` or your own |
| `/review`, `/security-review`, `/test`, `/explain <code>` | review the uncommitted changes, look for security problems, run and fix the tests, explain part of the code |
| `/memory [add <note>]`, `/add-dir <path>`, `/todos` | ZARN.md memory files, another directory for this session, the model's todo list |
| `/mcp`, `/agents`, `/permissions`, `/hooks`, `/plugins` | open the matching panel tab or summary |
| `/doctor` | check config, keys, providers, MCP, modes, routing, caches, hooks, plugins, spend caps, sandbox, keybindings and the terminal |
| `/context`, `/cost`, `/limits` | what fills the context window, what the session cost, spend caps |
| `/compact`, `/clear`, `/rewind` (esc esc) | shrink, drop or rewind the conversation |
| `/resume`, `/fork`, `/rename`, `/export [path]`, `/copy` | sessions |
| `/review`, `/commit`, `/pr` | git helpers |
| `/tasks`, `/loop` | background agents and repeating prompts |
| `/status`, `/theme`, `/init` | session info, theme or accent color, write a `ZARN.md` for the project |

Type `/` to see every command, including your own from `.zarn/commands/*.md`,
MCP prompts and your skills. After a command and a space, the list shows its
arguments (subcommands, modes, models, profiles, providers, MCP servers,
agents, themes) and the input shows the syntax still to fill in, for example
`/limits [session|day <usd|off>]`. ↑↓ or the wheel move through a list, tab
completes, enter runs, and a click picks an entry.

The same operations exist on the command line:

```sh
zarn setup                     # the browser page on localhost:9276
zarn login                     # paste any key; zarn works out the provider
zarn login groq
zarn providers add --kind openai-compat --base-url http://localhost:8000/v1
zarn providers list | test <name> | remove <name>
zarn models [--provider NAME] [--refresh]
zarn limits [--session 5] [--day 20]
zarn modes list | show <name>
zarn skills list | show <name> | new <name> [description] [--project]
zarn skills install <git URL, GitHub folder link, folder or SKILL.md link> [--project]
zarn skills remove <name> | enable <name> | disable <name>
zarn mcp add files -- npx -y @modelcontextprotocol/server-filesystem .
zarn hooks list
zarn plugin add <dir or git URL> | list | approve <name> | revoke <name> | remove <name>
zarn schedule add "<prompt>" | list | remove <id>
zarn doctor
```

## Where your API key is stored

Your key never goes into a config file, a log, a session transcript, an export
or tool output. zarn refuses to load a config file that contains a literal
`api_key`, or an MCP `env` or `headers` value that looks like a key.

Keys are kept in your operating system's credential store under the service
name `zarn`, one entry per provider:

| System | Store |
| --- | --- |
| Windows | Credential Manager (Control Panel → Credential Manager → Windows Credentials, entries named `zarn`) |
| macOS | Keychain (Keychain Access, search for `zarn`) |
| Linux | Secret Service (GNOME Keyring, KWallet) |

The same store holds:

- OAuth tokens for remote MCP servers, as `zarn-mcp:<server>` (written by
  `/mcp login <server>`).
- Any other secret you save with `zarn login <name>` and reference as
  `key:<name>` from an MCP server's `env` or `headers`, or from `[websearch]`.

If no credential store is available, for example on a headless Linux server,
zarn keeps keys in `~/.config/zarn/auth.json` with owner-only permissions and
tells you once that it is doing so. Set `ZARN_CREDENTIAL_STORE=file` to use
that file even when a credential store exists.

Two other ways to supply a provider key, set per provider in `/config`:

- `api_key_env`: the name of an environment variable. When that variable is
  set it takes precedence; otherwise zarn falls back to the credential store,
  then the file.
- `api_key_helper`: a command that prints the key, for example a password
  manager CLI. It runs without a shell, with a timeout, and its output is never
  logged.

### Removing a key

```sh
zarn logout <provider>
```

or `/logout <provider>` inside zarn. This deletes the key from the credential
store and from the fallback file. `zarn providers remove <provider>` deletes
the provider and its key together. `/mcp logout <server>` removes an MCP
token. If the provider also reads an environment variable, unset that variable
in your shell too.

## Configuration files

Settings live in three files, highest precedence first:

| File | Scope | Committed? |
| --- | --- | --- |
| `./.zarn/config.local.toml` | just for you, in this project | no (`.zarn/` is ignored) |
| `./.zarn/config.toml` | the project | if you choose to |
| `~/.config/zarn/config.toml` | you, everywhere | no |

Every change made in zarn is written back to the file that defines that
setting. When a permission prompt or the MCP form asks where to save, `local`
(or `[l] just for me`) writes to `config.local.toml`. `ZARN_CONFIG_DIR` and
`ZARN_DATA_DIR` move the user config and the data directory.

### Every config key

```toml
[general]
default_profile = "default"
accent = "violet"
theme = "auto"
banner = true
show_cost = true
status_line = "{model} · {mode} · {branch} · {context} · {cost}"
notifications = true
mouse = true
vim = false
mode = "default"
auto_compact = true
context_budget = 8000
discovery_cache_minutes = 60
read_only_commands = ["ls", "git status", "git diff", "git log"]
auto_allow_read_only = true
sandbox = false
max_parallel_agents = 4
lazy_tools_threshold = 40

[providers.anthropic]
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[providers.local]
kind = "openai-compat"
base_url = "http://localhost:11434/v1"
context_window = 32768
max_output = 4096
vision = false
tools = "prompted"
api_key_helper = "pass show zarn/local"
credits_url = "https://example.test/balance"
credits_format = "openrouter"

[profiles.default]
provider = "anthropic"
model = "claude-sonnet-5-5"
fallback = ["local"]
chain = ["anthropic:claude-haiku-5-5", "local:qwen3-coder"]

[routing]
enabled = true

[routing.roles.title]
provider = "local"
model = "small-model"

[pricing."claude-sonnet-5-5"]
input = 3.0
output = 15.0
cached = 0.3

[limits]
session_usd = 5.0
day_usd = 20.0

[permissions]
bash = "ask"
read = "allow"
allow = ["bash(cargo test:*)", "edit(src/**)"]
ask = ["bash(git push:*)"]
deny = ["bash(rm -rf:*)"]

[hooks]
timeout_secs = 30

[[hooks.pre_tool]]
match = "bash"
command = "scripts/check-command.sh"

[[hooks.post_tool]]
match = "edit"
command = "cargo fmt"

[[hooks.prompt_submit]]
command = "scripts/screen-prompt.sh"

[[hooks.turn_end]]
command = "notify-send zarn done"

[skills]
pdf = false

[mcp.files]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]

[mcp.files.env]
LOG_LEVEL = "info"
GITHUB_TOKEN = "key:github-token"

[mcp.remote]
url = "https://mcp.example.test/mcp"
client_id = "zarn"
scope = "read"

[mcp.remote.headers]
X-Api-Key = "env:REMOTE_API_KEY"

[websearch]
url = "https://search.example.test/search?q={query}&format=json"
header = "X-Subscription-Token"
key = "key:websearch"

[git]
commit_trailer = "Reviewed-by: Your Name <you@example.test>"
```

What the less obvious keys do:

| Key | Meaning |
| --- | --- |
| `general.theme` | `dark`, `light`, `colorblind` or `auto` (reads `COLORFGBG`, falls back to dark) |
| `general.status_line` | template with `{model}`, `{mode}`, `{profile}`, `{context}`, `{cost}`, `{cwd}`, `{branch}`; `{{` and `}}` are literal braces |
| `general.notifications` | bell and window title after turns over 10 seconds and on permission prompts while the terminal may be unfocused; never written when stdout is not a terminal |
| `general.output_style` | the output style added to the system prompt (`explanatory`, `learning`, `concise` or a file in `styles/`); `/style` sets it |
| `general.update_check` | on by default: look for a new zarn release at most once a day at startup and say so in the chat; `zarn update` or `/update` installs it |
| `general.mouse` | on by default: the wheel scrolls the conversation and menus take clicks; native text selection needs shift (option in macOS Terminal). Turn it off to select without shift; the wheel then sends arrow keys in most terminals (see `/doctor`) |
| `general.context_budget` | token budget for all `ZARN.md` text, including nested files and imports |
| `general.read_only_commands`, `auto_allow_read_only` | shell commands that run without asking |
| `general.sandbox` | run bash with secret-looking environment variables removed; not OS isolation |
| `general.max_parallel_agents` | how many `task` calls in one reply run at once |
| `general.lazy_tools_threshold` | above this many tools, MCP tools load on demand through `tool_search` |
| `providers.*.tools` | `native` or `prompted` for models without native tool calling |
| `pricing.<prefix>` | dollars per million tokens; a model with no rate shows "no rate configured", never `$0` |
| `limits` | spend caps; they only apply to priced models |
| `permissions.*` | `allow`, `ask` or `deny` per tool, and pattern rules; deny beats ask beats allow |
| `hooks.*` | commands that get the event as JSON on stdin; a non-zero exit from `pre_tool` or `prompt_submit` blocks it |
| `mcp.*.env`, `mcp.*.headers` | literals, or `env:NAME` / `key:NAME` references resolved when the server starts (also inside a value, like `Bearer env:TOKEN`) |
| `websearch` | your own search backend; there is no default and no shipped key |
| `git.commit_trailer` | a line `/commit` appends; without it no attribution line is ever added |

## Modes

A mode bundles a permission overlay, a tool list and extra instructions:
`default`, `plan` (read-only until you approve the plan), `accept-edits` (file
edits without asking), `bypass` (everything but deny rules; asks you to
confirm) and `full` (full access: no prompts, any path, installs the tools and
packages it needs for your user account without sudo, tests its own work and
keeps going for up to 200 steps per message; asks you to confirm, like
bypass). Your own modes are Markdown files with frontmatter in
`.zarn/modes/` or `~/.config/zarn/modes/`:

```markdown
---
name: review
description: read and comment, never change files
tools: read, grep, glob
permissions: bash=deny
model: small-model
---
Review the code you are pointed at and report problems with file and line.
```

`max_steps: 100` raises how many model calls one message may take (32 by
default). The active mode is `general.mode`; shift+tab cycles modes and `/mode`
picks one.

## Models, profiles and routing

A profile names a provider, a model and fallback providers that take over on
rate limits or outages (zarn tells you when it switches). With `[routing]`
enabled, roles such as `title`, `compact`, `plan`, `subagent` or `review` can
use a cheaper or stronger model. `/models` and `zarn models` list each
provider's models, rate limits and credit balance where the provider exposes
them; results are cached for `discovery_cache_minutes`.

When a provider says to retry within about a minute (a free tier's
requests-per-minute limit) or answers that it is busy (HTTP 502, 503 or 529),
zarn waits, says so, and tries again up to twice. A daily quota that is used up,
or a model the key has no quota for, is reported with the time it resets and a
pointer to `/model`. When you connect a key, zarn sends one short test request
to the model it picked; if that model cannot be used with the key (retired for
new users, no quota), it tries the next suggested models and keeps the first
that answers.

### Model chains and switching

A chain lists the models to fall back to, in order, as `provider:model`
(`chain` under a profile, or `/chain`). When the model in use fails with a
rate limit, an outage, a dropped connection, no credit, a conversation too
long for it, or a model the provider no longer offers, the same turn goes to
the next model in the chain and zarn says which one answered. A model that
fails twice in a row sits out for 5 minutes (30 for a rejected key, no credit
or a missing model), so later turns go straight to the next one; after its
rest it is tried first again and zarn says when it is back. A model whose
context window is too small for the conversation is skipped while a bigger one
is in the chain. Older `fallback = ["provider"]` entries still work and now
use that provider's own model.

| Command | What it does |
|---|---|
| `/chain` | the chain for this profile, and which models are resting |
| `/chain add <provider:model> …` | add models (completion lists every model your keys can use) |
| `/chain remove <n or provider:model>` | drop one |
| `/chain set <provider:model> …` | replace the chain |
| `/chain clear` | use only the main model |

`/rank` finds good models for the chain. It sends each candidate one short
request (256 output tokens at most) that asks it to call an `add` tool, then
ranks them: models that answer and call the tool with the right arguments
first, then by time to the first token and by context window. With no
arguments it checks the chain and a few suggested models from every provider
with a key; `/rank openrouter` checks one provider's models, and `/rank
a:b c:d` checks exactly those. `/rank save [n]` puts the best `n` working
models (3 by default, never the main model) in the chain. From the shell:
`zarn rank [provider | provider:model …] [--save [n]]`.

Before a message goes out, zarn compacts the conversation when it no longer
fits the current model (for example after `/model` picks one with a smaller
window), and when there is no earlier turn to summarize it shortens old tool
outputs instead. Replies may use up to 16384 output tokens, or the provider's
`max_output`; a model that allows fewer gets the request again with its own
limit. A tool call cut off by that limit is never run: the model is told to
write large files in smaller steps.

## Permissions

Every tool call goes through one policy: deny rules, then ask rules, then
allow rules, then the mode, then the tool's risk (reads are allowed, writes
and commands ask). The prompt shows a diff for edits and a numbered list: yes
once, yes for this pattern for the session, or yes and save the rule to the
project, just for you, or your user config, or no. Move with ↑↓ and press
enter, press the number, click the line, or use the letters `y a p l u n`.
The plan review and other confirmations work the same way. File tools ask before touching paths
outside the working directory; `--add-dir` adds more directories. Read-only
shell commands run without asking.

Every edit is checkpointed: `/rewind` (or esc twice) restores files and the
conversation to before any earlier turn.

## Input

- `@path` attaches a file (fuzzy completion, respects `.gitignore`); images go
  to models that accept them. `@server:uri` attaches an MCP resource.
- Large pastes collapse to a token and expand when sent.
- `!command` runs a shell command without the model (through the bash
  permission policy); `/attach-output` sends its output with your next message.
- `# note` appends a line to `ZARN.md`.
- ctrl+g edits the input in `$VISUAL` or `$EDITOR`; `general.vim` turns on
  normal and insert modes.
- Keys are rebindable in `~/.config/zarn/keybindings.toml`, for example
  `transcript = "ctrl+o"`; `/doctor` reports conflicts.

## Display

- The mouse wheel scrolls the conversation (`general.mouse`, on by default);
  ↑↓ in the input walk your earlier prompts, moving between lines first in a
  multi-line prompt. Menus, pickers, the command list and the settings tabs
  and rows take clicks. With the mouse on, hold shift to select text.
- Replies render Markdown: headings, bullet and numbered lists, quotes,
  tables, links and code blocks.
- A todo list the model keeps (`todo` tool) stays above the input.
- Tool cards are specific per tool: line ranges, match counts, diff summaries,
  live bash output, and a row per running agent.
- ctrl+o opens a transcript with every tool output in full; ctrl+s searches
  the scrollback, `n` and `N` move between hits.
- The spinner shows elapsed time and an estimate of tokens streamed.

## Output styles

A style changes how zarn writes, not what it may do; it works with any mode.
`/style` opens a picker, `/style <name>` switches, and the choice is saved as
`general.output_style`.

| Style | What it does |
|---|---|
| `default` | zarn's normal replies |
| `explanatory` | adds short "Insight" notes on why it chose an approach and the patterns in your code |
| `learning` | does the routine work but leaves small, meaningful pieces for you to write, marked `TODO(human)` |
| `concise` | short answers with no preamble or recap |

Your own styles are Markdown files in `.zarn/styles/<name>.md` (this project)
or `~/.config/zarn/styles/<name>.md` (everywhere). The file name is the style's
name and the body is added to the system prompt; an optional frontmatter
`description:` shows in the picker:

```markdown
---
description: answers for a reviewer in a hurry
---
Lead with the verdict in one sentence, then the evidence as a short list.
```

## Agents

Agents are Markdown files in `.zarn/agents/` or `~/.config/zarn/agents/`:

```markdown
---
name: reviewer
description: reviews a diff for bugs
tools: read, grep, glob
model: small-model
isolation: worktree
---
You review diffs and report problems with file and line.
```

The model delegates with the `task` tool. Several `task` calls in one reply run
at the same time, each with a live row. `background = true` returns at once;
`/tasks` lists background agents and `/tasks cancel <id>` stops one; finished
results go to the model with your next message. `isolation: worktree` runs the
agent in its own git worktree, which is kept only if it has changes. Agents use
the same permission prompts as the main conversation.

## Custom commands

`.zarn/commands/<name>.md` (or `~/.config/zarn/commands/`) becomes `/<name>`;
subdirectories become `/<dir>:<name>`:

```markdown
---
description: fix a GitHub issue
argument-hint: <issue> [branch]
allowed-tools: bash(gh issue view:*), read
model: claude-sonnet-5-5
---
Fix issue $1 on branch $2. Context: !`gh issue view $1`
Follow the notes in @docs/contributing.md
```

`$ARGUMENTS` is everything after the command, `$1`..`$9` the words. A
`` !`command` `` runs before sending and is replaced by its output; it asks for
permission exactly like the bash tool unless `allowed-tools` covers it.
`@path` attaches files. `model` applies to that one turn.

## System prompt

zarn's built-in prompt tells the model how to work: read before changing,
check its work, write files with the file tools instead of shell heredocs,
work anywhere you point it rather than in a made-up scratch folder, and ask
before anything hard to undo. `/system` shows the prompt in use and where it
comes from.

Replace it with your own:

| File | Applies to |
|---|---|
| `.zarn/SYSTEM.md` | this project (wins over yours) |
| `~/.config/zarn/SYSTEM.md` | you, everywhere |

`/system edit` opens yours in `$VISUAL`/`$EDITOR` (Notepad on Windows when
neither is set), starting from the built-in text; add `--project` for the
project file. `/system reset` deletes it and goes back to the built-in prompt.
`{cwd}` and `{environment}` in the file are replaced with the working
directory and the OS and shell. Edits made in any editor apply from the next
message. `ZARN.md` memory, the mode and the output style are still added after
it.

## Memory

`ZARN.md` in the project and in `~/.config/zarn` go into the system prompt. A
`ZARN.md` in a subdirectory joins the conversation the first time the model
reads or edits a file under it. A line that is just `@path/to/file.md` imports
that file (up to 5 levels deep; cycles are skipped). All of it shares
`general.context_budget`.

## MCP

Servers are stdio commands or HTTP URLs (`/mcp add`, the MCP tab, or
`zarn mcp add`). Their tools appear as `<server>__<tool>`, their resources as
`@server:uri` mentions and their prompts as `/server:prompt` commands. When a
server reports that its tools changed, zarn refreshes them without a restart.
Remote servers that need OAuth sign in with `/mcp login <server>` (device
flow); the token goes to the credential store. With more than
`lazy_tools_threshold` tools, MCP tools are found and loaded through
`tool_search` instead of being sent with every request.

## Plugins

A plugin is a directory with a `plugin.toml`:

```toml
name = "lint-kit"
version = "1.0.0"
description = "linting commands and a pre-tool check"
commands = "commands"
agents = "agents"
skills = "skills"
modes = "modes"

[[hooks.pre_tool]]
match = "bash"
command = "lint-kit-check"

[mcp.lint]
command = "lint-kit-mcp"
```

`zarn plugin add <dir or git URL>` copies it to `~/.config/zarn/plugins/`.
Installing never runs plugin code. Its commands, agents, skills and modes are
available at once, namespaced as `lint-kit:<name>`; its hooks and MCP servers
run code, so they stay off until you approve them with
`zarn plugin approve lint-kit` or in the Plugins tab. Changing a plugin's code
drops the approval.

## Tools

Built in: `read`, `write`, `edit`, `multiedit` (several edits, all or nothing),
`notebook_edit` (`.ipynb` cells), `glob`, `grep`, `bash` (with
`background = true`, plus `bash_output` and `bash_kill`), `webfetch` (HTTP GET
with size and time limits, asks first), `websearch` (only with your own
`[websearch]` backend), `todo`, `task` and `skill`. Web content is wrapped as
untrusted data so it cannot pose as instructions.

## Scripting and CI

```sh
zarn --print "summarize the open TODOs" --output-format json
echo "explain this error" | zarn --print
zarn --print "fix the lint" --allowed-tools "edit,bash(cargo clippy:*)" --max-turns 10
```

| Flag | Meaning |
| --- | --- |
| `--output-format text\|json\|stream-json` | one object at the end, or one object per event per line; keys are redacted |
| `--allowed-tools`, `--disallowed-tools` | rules to allow or deny; a bare name in `--disallowed-tools` also hides the tool |
| `--mode`, `--permission-mode` | mode for the run; bypass and full need `--permission-mode bypass` or `full` |
| `--max-turns N` | stop after N model calls |
| `--add-dir DIR` | another directory the file tools may touch (repeatable) |
| `--system-prompt-file FILE`, `--append-system-prompt TEXT` | replace zarn's base instructions, or add to the end |
| `--no-session-persistence` | do not save the run as a session |
| `--continue`, `--resume ID`, `--fork-session` | continue a session, or copy it into a new one |
| `--worktree NAME` | run in `.zarn/worktrees/NAME` on a new branch |

`--print` never asks: anything that would need permission is refused.
`docs/github-action.md` shows a workflow you can copy.

`/loop 10m <prompt>` repeats a prompt inside a session. For runs while zarn is
closed, `zarn schedule add "<prompt>"` stores it and prints a crontab line that
calls `zarn run-scheduled <id>`; zarn does not install schedulers itself.

## Git

`/review [focus]` reviews the uncommitted changes. `/commit` drafts a message
from the staged diff, shows it, and commits only when you press `y`. `/pr`
drafts a title and body from the branch and saves it to `.zarn/pr.md`; it
calls no hosting API.

## Windows notes

zarn runs shell features through `cmd /C` on Windows (bash tool, hooks, `!`
commands) and stops background commands with `taskkill`. Paths are handled
with the platform's separators and home directory. The clipboard uses
`clip.exe`. Scheduled prompts run from Task Scheduler with
`zarn run-scheduled <id>`.

## Contributing

Enable the pre-commit hook, which blocks a commit that stages anything shaped
like an API key:

```sh
git config core.hooksPath .githooks
```

The same scan runs as a test (`cargo test -p zarn --test no_secrets`), over
every file in the repository and over a release build of the binary. To scan
the whole history:

```sh
scripts/secret-scan.sh --history
```

Test fixtures and mock servers must use obviously fake keys such as
`test-key-not-real`.
