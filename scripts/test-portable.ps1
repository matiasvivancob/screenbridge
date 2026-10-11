# Unzips dist\ScreenBridge-<version>-portable.zip to a temp folder, runs it, and checks that settings stay inside that folder.
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$version = (Get-Content "$root\VERSION" -Raw).Trim()
$zip = "$root\dist\ScreenBridge-$version-portable.zip"
if (-not (Test-Path $zip)) { throw "Build first: scripts\build-windows.ps1" }

$dir = Join-Path $env:TEMP 'sb-portable-test'
if ($dir -notlike '*\sb-portable-test') { throw "unexpected dir $dir" }
if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
Expand-Archive $zip $dir
$app = Join-Path $dir 'ScreenBridge'
Write-Host "zip contents:      $((Get-ChildItem $app | ForEach-Object Name) -join ', ')"

$appdata = "$env:APPDATA\ScreenBridge\config.json"
$before = if (Test-Path $appdata) { (Get-Item $appdata).LastWriteTimeUtc } else { $null }

# The settings window creates the config on first run; it must land in <app>\data, not %APPDATA%.
$p = Start-Process "$app\screenbridge.exe" -ArgumentList '--settings' -PassThru
$deadline = (Get-Date).AddSeconds(30)
while ((Get-Date) -lt $deadline -and -not (Test-Path "$app\data\config.json") -and -not $p.HasExited) { Start-Sleep -Milliseconds 300 }
Write-Host "settings window:   $(if ($p.HasExited) { 'exited' } else { 'running' })"
if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force; $p.WaitForExit(5000) | Out-Null }

Write-Host "data\config.json:  $(Test-Path "$app\data\config.json")"
$after = if (Test-Path $appdata) { (Get-Item $appdata).LastWriteTimeUtc } else { $null }
Write-Host "%APPDATA% config untouched: $($before -eq $after)"
$id = (Get-Content "$app\data\config.json" -Raw | ConvertFrom-Json).pc_id
Write-Host "own pc_id:         $($id.Substring(0, 8))... (separate from the installed copy's id)"

Start-Sleep 1
Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
Write-Host "cleaned up: $(-not (Test-Path $dir))"
