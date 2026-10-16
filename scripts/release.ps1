$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$userHome = $env:USERPROFILE
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $userHome '.cargo' }
$sep = [char]0x1f
$env:CARGO_ENCODED_RUSTFLAGS = @(
    "--remap-path-prefix=$userHome=~",
    "--remap-path-prefix=$cargoHome=/cargo",
    "--remap-path-prefix=$root=/zarn"
) -join $sep
$env:CARGO_TARGET_DIR = Join-Path $root 'target\dist'
Push-Location $root
try {
    cargo build --release -p zarn --bin zarn @args
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
    Pop-Location
}
Write-Output "built $(Join-Path $env:CARGO_TARGET_DIR 'release\zarn.exe')"
