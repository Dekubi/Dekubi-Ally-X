# Builds Dekubi X and places it as "Dekubi X.exe" next to dekubi-x.exe.
# Cargo does not allow a space in a binary name, so the copy is made here.
# Run: powershell -ExecutionPolicy Bypass -File scripts\build.ps1 [-Profile release|debug]
param([ValidateSet("release", "debug")][string]$Profile = "release")
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    if ($Profile -eq "release") { cargo build --release -p dekubi-ally } else { cargo build -p dekubi-ally }
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $dir = Join-Path $root "target\$Profile"
    Copy-Item -Force (Join-Path $dir "dekubi-x.exe") (Join-Path $dir "Dekubi X.exe")
    # Also place it in the project folder (the repo's parent), where a double-click
    # finds the game folder next to it and writes logs\ there.
    $proj = Split-Path $root -Parent
    Copy-Item -Force (Join-Path $dir "dekubi-x.exe") (Join-Path $proj "Dekubi X.exe")
    Write-Host "Built $dir\Dekubi X.exe and $proj\Dekubi X.exe"
} finally { Pop-Location }
