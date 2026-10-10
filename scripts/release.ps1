# Builds everything into dist\: the Windows installer and the Mac .dmg, plus SHA256SUMS.txt.
#   scripts\release.ps1                          Windows installer only if no Mac is configured
#   $env:SB_MAC_HOST = 'user@192.168.1.104'      also builds the .dmg on that Mac over SSH (key auth)
#   scripts\release.ps1 -Publish                 then creates a GitHub release v<version> with the files
# The .dmg can always be built by hand on any Mac: sh mac/build.sh
param([switch]$Publish)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$version = (Get-Content "$root\VERSION" -Raw).Trim()
$dist = "$root\dist"

& "$PSScriptRoot\build-windows.ps1"

$macHost = $env:SB_MAC_HOST
if ($macHost) {
    Write-Host "Building the Mac app on $macHost..."
    $remote = '~/screenbridge-build'
    ssh $macHost "rm -rf $remote && mkdir -p $remote/assets"
    scp -q -r "$root\mac" "${macHost}:$remote/"
    scp -q "$root\VERSION" "${macHost}:$remote/"
    scp -q "$root\assets\icon.png" "${macHost}:$remote/assets/"
    ssh $macHost "cd $remote && rm -rf mac/build && sh mac/build.sh"
    if ($LASTEXITCODE) { throw "Mac build failed" }
    scp -q "${macHost}:$remote/mac/build/ScreenBridge-$version.dmg" "$dist\"
} else {
    Write-Host "SB_MAC_HOST not set: skipping the Mac build (run 'sh mac/build.sh' on a Mac)."
}

$files = Get-ChildItem $dist -File | Where-Object { $_.Name -match "ScreenBridge-.*$([regex]::Escape($version))\.(exe|dmg)$" }
$files | ForEach-Object { "{0}  {1}" -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $_.Name } | Set-Content "$dist\SHA256SUMS.txt"
Get-Content "$dist\SHA256SUMS.txt"

if ($Publish) {
    if (-not $macHost) { throw "Refusing to publish a release without the Mac .dmg" }
    $notes = "ScreenBridge $version. See docs/INSTALL.md."
    gh release create "v$version" @($files.FullName) "$dist\SHA256SUMS.txt" --title "ScreenBridge $version" --notes $notes
}
