$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

& (Join-Path $PSScriptRoot 'release.ps1') @args
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value

$candidates = @(
    (Get-Command iscc -ErrorAction SilentlyContinue).Source,
    (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
    (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'),
    (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe')
) | Where-Object { $_ -and (Test-Path $_) }
if (-not $candidates) {
    Write-Error "Inno Setup 6 is not installed. Install it with: winget install JRSoftware.InnoSetup  (then run this script again)"
    exit 1
}
$iscc = @($candidates)[0]

$bin = Join-Path $root 'target\dist\release'
$out = Join-Path $root 'target\dist\installer'
& $iscc "/DAppVersion=$version" "/DBinDir=$bin" "/DOutDir=$out" (Join-Path $root 'installer\zarn.iss')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Output "built $(Join-Path $out "zarn-setup-$version.exe")"
