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

# Start from an empty artifact set so a failed step can never publish a stale file from an earlier build.
New-Item -ItemType Directory $dist -Force | Out-Null
Get-ChildItem $dist -File | Where-Object { $_.Name -match '^(ScreenBridge-|SHA256SUMS)' } | Remove-Item -Force

& "$PSScriptRoot\build-windows.ps1"

$macHost = $env:SB_MAC_HOST
if ($macHost) {
    Write-Host "Building the Mac app on $macHost..."
    $remote = '~/screenbridge-build'
    function Run($what, [scriptblock]$cmd) { & $cmd; if ($LASTEXITCODE) { throw "$what failed (exit $LASTEXITCODE)" } }
    Run "prepare Mac folder" { ssh $macHost "rm -rf $remote && mkdir -p $remote/assets" }
    Run "copy sources" { scp -q -r "$root\mac" "${macHost}:$remote/" }
    Run "copy VERSION" { scp -q "$root\VERSION" "${macHost}:$remote/" }
    Run "copy icon" { scp -q "$root\assets\icon.png" "${macHost}:$remote/assets/" }
    Run "Mac build" { ssh $macHost "cd $remote && rm -rf mac/build && sh mac/build.sh" }
    Run "copy dmg and zip back" { scp -q "${macHost}:$remote/mac/build/ScreenBridge-$version.dmg" "${macHost}:$remote/mac/build/ScreenBridge-$version-mac.zip" "$dist\" }
} else {
    Write-Host "SB_MAC_HOST not set: skipping the Mac build (run 'sh mac/build.sh' on a Mac)."
}

$files = Get-ChildItem $dist -File | Where-Object { $_.Name -match "^ScreenBridge-.*$([regex]::Escape($version)).*\.(exe|dmg|zip)$" }
$files | ForEach-Object { "{0}  {1}" -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $_.Name } | Set-Content "$dist\SHA256SUMS.txt"
Get-Content "$dist\SHA256SUMS.txt"

if ($Publish) {
    if (-not $macHost) { throw "Refusing to publish a release without the Mac .dmg" }
    $notes = "ScreenBridge $version. See docs/INSTALL.md."
    gh release create "v$version" @($files.FullName) "$dist\SHA256SUMS.txt" --title "ScreenBridge $version" --notes $notes
}
