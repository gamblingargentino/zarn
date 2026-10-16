$raw = [Console]::In.ReadToEnd()
try { $payload = $raw | ConvertFrom-Json } catch { exit 0 }

$sep = [char]92
$path = $payload.tool_input.file_path
if (-not $path) { exit 0 }

$norm = ([string]$path).Replace($sep, [char]47)
if (-not ($norm -eq 'Cargo.toml' -or $norm.EndsWith('/Cargo.toml'))) { exit 0 }

$root = $env:CLAUDE_PROJECT_DIR
if (-not $root) { $root = 'C:' + $sep + 'zarn' }
$root = ([string]$root).Replace($sep, [char]47).TrimEnd([char]47)
$cwd = ([string]$payload.cwd).Replace($sep, [char]47).TrimEnd([char]47)

if ($cwd -ieq $root) { exit 0 }

[Console]::Error.WriteLine("Blocked: subagents must never edit any Cargo.toml. If you need a dependency, report which crate and version you need in your final message and stop.")
exit 2
