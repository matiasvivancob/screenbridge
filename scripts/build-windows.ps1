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
