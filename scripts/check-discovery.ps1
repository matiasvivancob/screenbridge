# Starts the built tray app briefly and prints the Macs it discovers (reads %APPDATA%\ScreenBridge\discovered.json).
param([string]$Exe = "$PSScriptRoot\..\windows\target\release\screenbridge.exe", [int]$Seconds = 8)
$tray = Start-Process -FilePath $Exe -PassThru
try {
    Start-Sleep $Seconds
    $file = "$env:APPDATA\ScreenBridge\discovered.json"
    if (Test-Path $file) {
        $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
        (Get-Content $file -Raw -Encoding UTF8 | ConvertFrom-Json) | ForEach-Object {
            "{0}  {1}  last seen {2}s ago" -f $_.name, $_.address, ($now - $_.last_seen)
        }
    } else { "no Macs heard in $Seconds seconds" }
} finally {
    Stop-Process -Id $tray.Id -Force -ErrorAction SilentlyContinue
}
