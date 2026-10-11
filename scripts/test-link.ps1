# Runs the PC app in a throwaway portable folder against tools\fake_mac.py (no real Mac involved).
# Checks connect, per-display toggles, reconnect with restored displays, and "Mac said bye".
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "$root\windows\target" }
$exe = "$target\release\screenbridge.exe"
if (-not (Test-Path $exe)) { throw "Build first: cd windows; cargo build --release" }

$dir = Join-Path $env:TEMP 'sb-link-test'
if ($dir -notlike '*\sb-link-test') { throw "unexpected dir $dir" }
if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
New-Item -ItemType Directory "$dir\data" | Out-Null
Copy-Item $exe "$dir\screenbridge.exe"
Set-Content "$dir\portable.txt" "test"

$cfg = @{
    pc_id = ('{0:x32}' -f (Get-Random -Maximum ([int]::MaxValue)))
    mac_name = ''; mac_address = '127.0.0.1'; auto_connect = $true
    displays = @(
        @{ enabled = $true; width = 1280; height = 720; monitor = ''; fullscreen = $false; auto_on = $false },
        @{ enabled = $true; width = 1280; height = 720; monitor = ''; fullscreen = $false; auto_on = $false }
    )
    codec = 'h264'; mbps = 30; fps = 60; speed = $false; low_latency_rc = $true
}
# No BOM, and discovery disabled below: this test must never be able to reach a real Mac.
[System.IO.File]::WriteAllText("$dir\data\config.json", ($cfg | ConvertTo-Json -Depth 5), (New-Object System.Text.UTF8Encoding($false)))
$env:SB_DISABLE_DISCOVERY = '1'

$tray = $null
try {
    $fake = Start-Process python -ArgumentList "`"$root\tools\fake_mac.py`"" -PassThru -NoNewWindow -RedirectStandardOutput "$dir\fake.out" -RedirectStandardError "$dir\fake.err"
    Start-Sleep 1
    $tray = Start-Process "$dir\screenbridge.exe" -PassThru
    $fake | Wait-Process -Timeout 90 -ErrorAction SilentlyContinue
    if (-not $fake.HasExited) { Stop-Process -Id $fake.Id -Force; Write-Host "fake Mac timed out" }
    Get-Content "$dir\fake.out"
    if (Test-Path "$dir\fake.err") { Get-Content "$dir\fake.err" | Select-Object -First 15 }
    $exit = $fake.ExitCode
} finally {
    if ($tray -and -not $tray.HasExited) { Stop-Process -Id $tray.Id -Force; $tray.WaitForExit(5000) | Out-Null }
    Start-Sleep 1
    Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
}
exit $exit
