# Silent per-user install -> verify -> uninstall round trip of dist\ScreenBridge-Setup-<version>.exe.
# Uses /CURRENTUSER (no UAC, so no firewall rule); the firewall step needs a real elevated install.
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$version = (Get-Content "$root\VERSION" -Raw).Trim()
$setup = "$root\dist\ScreenBridge-Setup-$version.exe"
if (-not (Test-Path $setup)) { throw "Build the installer first: scripts\build-windows.ps1" }

$dir = Join-Path $env:TEMP 'sb-install-test'
if ($dir -notlike '*\sb-install-test') { throw "unexpected install dir $dir" }
if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
$log = Join-Path $env:TEMP 'sb-install.log'

$p = Start-Process -FilePath $setup -Wait -PassThru -ArgumentList @('/VERYSILENT', '/CURRENTUSER', '/SUPPRESSMSGBOXES', '/NORESTART', '/TASKS=autostart', "/DIR=$dir", "/LOG=$log")
Write-Host "install exit code: $($p.ExitCode)"
$exe = Join-Path $dir 'screenbridge.exe'
Write-Host "exe installed:     $(Test-Path $exe)  ($([math]::Round((Get-Item $exe).Length / 1MB, 1)) MB)"
Write-Host "docs installed:    $(Test-Path (Join-Path $dir 'docs\PROTOCOL.md'))"
$run = (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name ScreenBridge -ErrorAction SilentlyContinue).ScreenBridge
Write-Host "autostart entry:   $run"
$unins = Get-ChildItem $dir -Filter 'unins*.exe' | Select-Object -First 1
Write-Host "uninstaller:       $($unins.Name)"

# Start the tray from the installed copy, check it stays up and holds the single-instance mutex.
$tray = Start-Process -FilePath $exe -PassThru
Start-Sleep 3
Write-Host "tray running:      $(-not $tray.HasExited)"
$second = Start-Process -FilePath $exe -PassThru
Start-Sleep 2
Write-Host "2nd instance exits: $($second.HasExited)"

$u = Start-Process -FilePath $unins.FullName -Wait -PassThru -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART')
Start-Sleep 2
Write-Host "uninstall exit:    $($u.ExitCode)"
Write-Host "exe removed:       $(-not (Test-Path $exe))"
Write-Host "tray stopped:      $(-not (Get-Process -Id $tray.Id -ErrorAction SilentlyContinue))"
$run2 = (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name ScreenBridge -ErrorAction SilentlyContinue).ScreenBridge
Write-Host "autostart removed: $(-not $run2)"
