# Debug launcher with a scripted controller. Builds the diagnostics build once
# (target\diag), then runs the title with DEKUBI_KEYS so keys and pad-1 buttons
# are pressed on a timer, with or without window focus. A real pad or the
# keyboard still works at the same time. Screenshots go to <log>\frames.
#
# Key names: Enter (A), Back (B), Esc (Menu), Tab (View), Up/Down/Left/Right
# (D-pad), X, Y, Q (LB), E (RB), F (LS), H (RS), letters, digits.
#
# Run:
#   powershell -ExecutionPolicy Bypass -File scripts\autopad.ps1
#   ...\autopad.ps1 -Keys "15:Enter,20:Enter,25:Down,26:Enter"   # own script
#   ...\autopad.ps1 -Every 3 -From 10 -To 240 -Key Enter          # press Enter every 3 s
#   ...\autopad.ps1 -Build                                        # force rebuild
param(
    [string]$Game = "C:\Users\User\Dekubi Box Project\Dekubi Ally X\game_scarlett_master.exe",
    [string]$Keys = "",
    [string]$Key = "Enter",
    [double]$Every = 4,
    [double]$From = 15,
    [double]$To = 300,
    [int]$Minutes = 10,
    [int]$ShotEvery = 20,
    [switch]$Build
)
$ErrorActionPreference = "Stop"
$repo = Split-Path $PSScriptRoot -Parent
$exe = Join-Path $repo "target\diag\dekubi-x.exe"
if ($Build -or -not (Test-Path $exe)) {
    Push-Location $repo
    try { cargo build --profile diag -p dekubi-ally --features diagnostics } finally { Pop-Location }
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
if (-not $Keys) {
    $list = @(); for ($t = $From; $t -le $To; $t += $Every) { $list += "{0}:{1}" -f $t, $Key }
    $Keys = $list -join ","
}
$out = Join-Path (Split-Path $repo -Parent) "_auto-logs\autopad-$(Get-Date -Format yyyyMMdd-HHmmss)"
New-Item -ItemType Directory -Force $out | Out-Null
Set-Content (Join-Path $out "keys.txt") $Keys
$env:DEKUBI_KEYS = $Keys
# Screenshots (half size PNG) into the log folder: frames 1, 2, 4, 8, ... plus
# one every $ShotEvery seconds (30 fps), so the menus reached can be seen.
$shots = Join-Path $out "frames"
New-Item -ItemType Directory -Force $shots | Out-Null
$env:DEKUBI_CAPTURE = $shots
$env:DEKUBI_CAPTURE_FRAMES = ((1..([math]::Floor($Minutes * 60 / $ShotEvery))) | ForEach-Object { $_ * $ShotEvery * 30 }) -join ","
$env:DEKUBI_CACHE_DIR = "$env:LOCALAPPDATA\DekubiX\cache"
$env:DEKUBI_DATA_DIR = "$env:LOCALAPPDATA\DekubiX\data"
$status = Join-Path (Split-Path $repo -Parent) ".claude-status.txt"
Set-Content $status "Autopad run: $Minutes min, keys in $out\keys.txt" -Encoding utf8
$p = Start-Process -FilePath $exe -ArgumentList @("run", "`"$Game`"", "--execute") -PassThru `
     -RedirectStandardOutput (Join-Path $out "launch.out.log") -RedirectStandardError (Join-Path $out "launch.err.log")
if (-not $p.WaitForExit($Minutes * 60000)) { Stop-Process -Id $p.Id -Force; $r = "stopped after $Minutes min" }
else { $r = "exit=$($p.ExitCode)" }
Get-ChildItem "$env:LOCALAPPDATA\DekubiX" -Recurse -Filter "crashes.log" -ErrorAction SilentlyContinue | Copy-Item -Destination $out
Set-Content (Join-Path $out "summary.txt") "autopad $r"
Set-Content $status "Autopad done ($r): logs in $out" -Encoding utf8
Write-Host "Autopad $r. Logs: $out"
