# zarn

An agent CLI for the terminal. Bring your own model provider and API key.

## Install

```sh
cargo install --path crates/zarn
```

For a binary to hand to other people, build it with the release script. It
remaps the build machine's paths (home directory, cargo registry, checkout)
out of the binary:

```sh
scripts/release.sh          # macOS, Linux, Git Bash
scripts/release.ps1         # Windows PowerShell
```

The result is in `target/dist/release/`.

## First run

zarn ships with no provider and no key. The first time you run `zarn` it opens
a setup flow: pick a provider, paste your key (it is never shown on screen),
pick a model, and zarn sends one small test request. Nothing is saved until
that request succeeds, and the prompt does not open until a provider works or
you choose to skip.

Everything after that is done from inside zarn, with no hand-edited files:

| Command | What it does |
| --- | --- |
| `/config` | settings panel: providers, models, skills, MCP, agents, permissions, general |
| `/login [provider]`, `/logout [provider]` | add, replace or remove a key |
| `/provider list\|add\|switch` | manage providers |
| `/model [name]` | pick or set the model |
| `/skills`, `/mcp`, `/agents`, `/permissions` | open the matching panel tab |
| `/doctor` | check config, keys, provider reachability, MCP servers and the terminal |
| `/status`, `/theme`, `/init` | session info, accent color, write a `ZARN.md` for the project |

Type `/` to see every command, including your own from `.zarn/commands/*.md`
and your skills.

The same operations exist on the command line:

```sh
zarn login anthropic
zarn providers add --kind openai-compat --base-url https://api.groq.com/openai/v1
zarn providers list | test <name> | remove <name>
zarn skills list
zarn mcp add files -- npx -y @modelcontextprotocol/server-filesystem .
zarn doctor
```

## Where your API key is stored

Your key never goes into a config file, a log, a session transcript or tool
output. zarn refuses to load a config file that contains a literal `api_key`.

Keys are kept in your operating system's credential store under the service
name `zarn`, one entry per provider:

| System | Store |
| --- | --- |
| Windows | Credential Manager (Control Panel → Credential Manager → Windows Credentials, entries named `zarn`) |
| macOS | Keychain (Keychain Access, search for `zarn`) |
| Linux | Secret Service (GNOME Keyring, KWallet) |

If no credential store is available, for example on a headless Linux server,
zarn keeps keys in `~/.config/zarn/auth.json` with owner-only permissions and
tells you once that it is doing so. Set `ZARN_CREDENTIAL_STORE=file` to use
that file even when a credential store exists.

To use an environment variable instead, set `api_key_env` for the provider in
`/config`. When that variable is set it takes precedence; otherwise zarn falls
back to the credential store, then the file.

### Removing a key

```sh
zarn logout <provider>
```

or `/logout <provider>` inside zarn. This deletes the key from the credential
store and from the fallback file. `zarn providers remove <provider>` deletes
the provider and its key together. If the provider also reads an environment
variable, unset that variable in your shell too.

## Configuration files

Settings live in `~/.config/zarn/config.toml` (yours) and
`./.zarn/config.toml` (the project's). The project file wins where both set
something, and every change made in zarn is written back to the file that
defines that setting. Skills are folders with a `SKILL.md` in
`~/.config/zarn/skills` or `./.zarn/skills`; `ZARN.md` files in the project
and in `~/.config/zarn` are added to the system prompt.

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
