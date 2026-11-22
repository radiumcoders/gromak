# Builds gromak in release mode and packages it:
#   target\installer\gromak-<version>-setup.exe   (Inno Setup installer)
#   target\installer\gromak-<version>-x64.exe     (portable executable)
#
# Needs Rust and Inno Setup 6 (winget install JRSoftware.InnoSetup).
#   powershell -NoProfile -File installer\build.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
Write-Host "gromak $version"

cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
$exe = Join-Path $root 'target\release\gromak.exe'

$iscc = @(
    (Get-Command iscc -ErrorAction SilentlyContinue).Source,
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 not found (winget install JRSoftware.InnoSetup)" }

& $iscc /Q "/DAppVersion=$version" "/DSourceExe=$exe" (Join-Path $PSScriptRoot 'gromak.iss')
if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }

$out = Join-Path $root 'target\installer'
Copy-Item $exe (Join-Path $out "gromak-$version-x64.exe") -Force
Get-ChildItem $out -Filter "gromak-$version-*.exe" | ForEach-Object {
    $hash = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower()
    "{0}  {1}" -f $hash, $_.Name
} | Set-Content (Join-Path $out "gromak-$version-SHA256SUMS.txt") -Encoding ascii
Get-ChildItem $out -Filter "gromak-$version-*" | Format-Table Name, Length -AutoSize
