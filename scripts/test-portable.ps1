# Unzips dist\ScreenBridge-<version>-portable.zip to a temp folder, runs it, and checks that settings stay inside that folder.
$ErrorActionPreference = 'Stop'
$script:failed = 0
function Check($name, $ok) { if (-not $ok) { $script:failed++ }; Write-Host ("{0}  {1}" -f $(if ($ok) { 'PASS' } else { 'FAIL' }), $name) }
$root = Split-Path $PSScriptRoot -Parent
$version = (Get-Content "$root\VERSION" -Raw).Trim()
$zip = "$root\dist\ScreenBridge-$version-portable.zip"
if (-not (Test-Path $zip)) { throw "Build first: scripts\build-windows.ps1" }

$dir = Join-Path $env:TEMP 'sb-portable-test'
if ($dir -notlike '*\sb-portable-test') { throw "unexpected dir $dir" }
if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
Expand-Archive $zip $dir
$app = Join-Path $dir 'ScreenBridge'
Check "zip has exe, portable.txt and README" ((Test-Path "$app\screenbridge.exe") -and (Test-Path "$app\portable.txt") -and (Test-Path "$app\README.md"))

$appdata = "$env:APPDATA\ScreenBridge\config.json"
$before = if (Test-Path $appdata) { (Get-Item $appdata).LastWriteTimeUtc } else { $null }

# The settings window creates the config on first run; it must land in <app>\data, not %APPDATA%.
$p = Start-Process "$app\screenbridge.exe" -ArgumentList '--settings' -PassThru
$deadline = (Get-Date).AddSeconds(30)
while ((Get-Date) -lt $deadline -and -not (Test-Path "$app\data\config.json") -and -not $p.HasExited) { Start-Sleep -Milliseconds 300 }
Check "settings window starts" (-not $p.HasExited)
if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force; $p.WaitForExit(5000) | Out-Null }

Check "settings written to data\config.json" (Test-Path "$app\data\config.json")
$after = if (Test-Path $appdata) { (Get-Item $appdata).LastWriteTimeUtc } else { $null }
Check "%APPDATA% config untouched" ($before -eq $after)
$id = (Get-Content "$app\data\config.json" -Raw | ConvertFrom-Json).pc_id
Check "portable copy has its own pc_id" ($id.Length -ge 16)

Start-Sleep 1
Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
Check "cleaned up" (-not (Test-Path $dir))
Write-Host $(if ($script:failed) { "$script:failed FAILED" } else { "ALL PASSED" })
exit $script:failed
