# Builds the Windows app and installer: dist\ScreenBridge-Setup-<version>.exe
# Needs: Rust (cargo) and Inno Setup 6 (winget install JRSoftware.InnoSetup).
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$version = (Get-Content "$root\VERSION" -Raw).Trim()

Write-Host "Building ScreenBridge $version (Windows app)..."
Push-Location "$root\windows"
try { cargo build --release; if ($LASTEXITCODE) { throw "cargo build failed" } } finally { Pop-Location }

$iscc = @(
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 not found. Install it with: winget install JRSoftware.InnoSetup" }

Write-Host "Building the installer..."
& $iscc "/DAppVersion=$version" "$root\installer\ScreenBridge.iss"
if ($LASTEXITCODE) { throw "Inno Setup failed" }
Write-Host "Built dist\ScreenBridge-Setup-$version.exe"

# Portable build: the same exe plus a marker file that keeps settings in a data\ folder next to it.
$stage = "$root\dist\portable-stage\ScreenBridge"
Remove-Item "$root\dist\portable-stage" -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $stage | Out-Null
Copy-Item "$root\windows\target\release\screenbridge.exe" $stage
Copy-Item "$root\README.md" $stage
Set-Content "$stage\portable.txt" "ScreenBridge portable mode: settings are kept in the data folder next to screenbridge.exe.`r`nDelete this file to use %APPDATA%\ScreenBridge instead."
$zip = "$root\dist\ScreenBridge-$version-portable.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path $stage -DestinationPath $zip
Remove-Item "$root\dist\portable-stage" -Recurse -Force
Write-Host "Built dist\ScreenBridge-$version-portable.zip"
