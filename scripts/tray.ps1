# Builds and runs the tray app from source (a debug build, which keeps its console window, so the
# log shows up in the terminal). For testing on Windows: drag this file into a PowerShell window
# and press Enter. Quit from the tray menu, or press Ctrl-C.
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot)

# Only one copy runs at a time, and Windows won't let cargo replace a running exe, so stop any
# copy that's already running (a release build, or this script's last run).
Get-Process crossglide -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 300

cargo run -p crossglide-ui
