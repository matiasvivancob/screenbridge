# Silent per-user install -> verify -> uninstall round trip of dist\ScreenBridge-Setup-<version>.exe.
# Uses /CURRENTUSER (no UAC, so no firewall rule); the firewall step needs a real elevated install.
$ErrorActionPreference = 'Stop'
$script:failed = 0
function Check($name, $ok) { if (-not $ok) { $script:failed++ }; Write-Host ("{0}  {1}" -f $(if ($ok) { 'PASS' } else { 'FAIL' }), $name) }
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
# The test installs and uninstalls the autostart entry; put the user's real one back afterwards.
$savedRun = (Get-ItemProperty $runKey -Name ScreenBridge -ErrorAction SilentlyContinue).ScreenBridge
$root = Split-Path $PSScriptRoot -Parent
$version = (Get-Content "$root\VERSION" -Raw).Trim()
$setup = "$root\dist\ScreenBridge-Setup-$version.exe"
if (-not (Test-Path $setup)) { throw "Build the installer first: scripts\build-windows.ps1" }

$dir = Join-Path $env:TEMP 'sb-install-test'
if ($dir -notlike '*\sb-install-test') { throw "unexpected install dir $dir" }
if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
$log = Join-Path $env:TEMP 'sb-install.log'

$p = Start-Process -FilePath $setup -Wait -PassThru -ArgumentList @('/VERYSILENT', '/CURRENTUSER', '/SUPPRESSMSGBOXES', '/NORESTART', '/TASKS=autostart', "/DIR=$dir", "/LOG=$log")
Check "installer exits 0" ($p.ExitCode -eq 0)
$exe = Join-Path $dir 'screenbridge.exe'
Check "exe installed" (Test-Path $exe)
Check "docs installed" (Test-Path (Join-Path $dir 'docs\PROTOCOL.md'))
$run = (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name ScreenBridge -ErrorAction SilentlyContinue).ScreenBridge
Check "autostart entry points at the installed exe" ($run -like "*sb-install-test*screenbridge.exe*")
$unins = Get-ChildItem $dir -Filter 'unins*.exe' | Select-Object -First 1
Check "uninstaller present" ($null -ne $unins)

# Start the tray from the installed copy, check it stays up and holds the single-instance mutex.
# Isolated: portable.txt gives it its own settings folder and instance lock (so it can never use the real PC
# identity or collide with a copy the user is running), and discovery is off so it can never reach a real Mac.
Set-Content (Join-Path $dir 'portable.txt') 'test'
$env:SB_DISABLE_DISCOVERY = '1'
$tray = Start-Process -FilePath $exe -PassThru
Start-Sleep 3
Check "tray runs" (-not $tray.HasExited)
$second = Start-Process -FilePath $exe -PassThru
Start-Sleep 2
Check "second instance exits (single instance)" $second.HasExited

$u = Start-Process -FilePath $unins.FullName -Wait -PassThru -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART')
Start-Sleep 2
Check "uninstaller exits 0" ($u.ExitCode -eq 0)
Check "exe removed" (-not (Test-Path $exe))
Check "running tray stopped by uninstall" (-not (Get-Process -Id $tray.Id -ErrorAction SilentlyContinue))
$run2 = (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name ScreenBridge -ErrorAction SilentlyContinue).ScreenBridge
Check "autostart entry removed" (-not $run2)
if ($savedRun) { Set-ItemProperty $runKey -Name ScreenBridge -Value $savedRun }
Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
Write-Host $(if ($script:failed) { "$script:failed FAILED" } else { "ALL PASSED" })
exit $script:failed
