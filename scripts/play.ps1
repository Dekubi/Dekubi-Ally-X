# Runs a title with Dekubi X for normal play. Cache, data and log stay outside the repo.
# Run: powershell -ExecutionPolicy Bypass -File scripts\play.ps1 -Game "<game folder>\<game exe>"
# Launcher idea from ztrx (github.com/Zettrox/ztrx, GPL-3.0-or-later, Zettrox).
param(
    [Parameter(Mandatory = $true)][string]$Game,
    [string]$Cache = "$env:LOCALAPPDATA\DekubiX\cache",
    [string]$Data = "$env:LOCALAPPDATA\DekubiX\data",
    [string]$Exe = "$PSScriptRoot\..\target\release\Dekubi X.exe"
)
foreach ($v in "DEKUBI_KEYS", "DEKUBI_CAPTURE", "DEKUBI_CAPTURE_FRAMES", "DEKUBI_DUMP_TARGETS") {
    Remove-Item "Env:$v" -ErrorAction SilentlyContinue
}
New-Item -ItemType Directory -Force $Cache, $Data | Out-Null
$env:DEKUBI_CACHE_DIR = $Cache
$env:DEKUBI_DATA_DIR = $Data
$log = "$env:LOCALAPPDATA\DekubiX\last-run.log"
& $Exe run $Game --execute 2>&1 | Tee-Object -FilePath $log
Write-Host "Exited with $LASTEXITCODE; log: $log"
