# Example: running zarn in a GitHub Action

This is an example to copy into your own repository, not a workflow that
runs here. Nothing in this repository runs it automatically: to use it,
save the YAML below as `.github/workflows/zarn.yml` in your project.

The job installs zarn, writes a minimal config that reads the API key from
an environment variable, and runs one headless turn with `zarn --print`.
The key comes from a repository secret, so it never appears in the config,
the logs or the repository.

## 1. Add the secret

In your repository on GitHub, open Settings → Secrets and variables →
Actions and add a secret named `ANTHROPIC_API_KEY` (or the variable your
provider uses).

## 2. Add the workflow

Replace `OWNER` with the account that hosts the zarn repository you install
from.

```yaml
name: zarn review

on:
  pull_request:

permissions:
  contents: read

jobs:
  review:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - uses: dtolnay/rust-toolchain@stable

      - name: Install zarn
        run: cargo install --locked --git https://github.com/OWNER/zarn zarn

      - name: Configure zarn
        run: |
          mkdir -p "$RUNNER_TEMP/zarn"
          cat > "$RUNNER_TEMP/zarn/config.toml" <<'TOML'
          [providers.anthropic]
          kind = "anthropic"
          api_key_env = "ANTHROPIC_API_KEY"

          [profiles.default]
          provider = "anthropic"
          model = "claude-sonnet-5-5"
          TOML

      - name: Review the change
        env:
          ZARN_CONFIG_DIR: ${{ runner.temp }}/zarn
          ZARN_DATA_DIR: ${{ runner.temp }}/zarn-data
          ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
        run: |
          git diff "origin/${{ github.base_ref }}...HEAD" > "$RUNNER_TEMP/change.diff"
          zarn --print "Review the diff in $RUNNER_TEMP/change.diff. List bugs and risky changes with file and line." \
            --add-dir "$RUNNER_TEMP" \
            --allowed-tools read,grep,glob \
            --disallowed-tools bash,write,edit,multiedit,notebook_edit,webfetch \
            --max-turns 20 \
            --no-session-persistence \
            --output-format json > review.json

      - uses: actions/upload-artifact@v4
        with:
          name: zarn-review
          path: review.json
```

## Notes

- `--print` runs headless: any tool call that would ask for permission is
  refused, so the run can only use what `--allowed-tools` and the mode allow.
  It refuses `bypass` mode unless you pass `--permission-mode bypass`; do not
  do that on untrusted pull requests.
- `--add-dir` lets the file tools read the diff outside the checkout; without
  it, paths outside the working directory are refused.
- `--output-format json` prints one object with the final text, the tool
  calls, the token usage and the cost; keys are redacted from it. Use
  `stream-json` to get one object per event instead.
- Pull requests from forks do not receive repository secrets, so the step
  fails there by design.
- To post the review as a comment, add a step that reads `review.json` with
  your own token and permissions; zarn never calls the GitHub API.
