# Builds and runs the tray app from source (a debug build, which keeps its console window, so the
# log shows up in the terminal). For testing on Windows: drag this file into a PowerShell window
# and press Enter. Quit from the tray menu, or press Ctrl-C.
#
# Each launch first checks that GitHub is reachable and, if it is, fast-forwards to the latest
# commit. Offline, or with local commits the remote doesn't have, it just runs what's checked out.
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot)

# Pulls the latest commit if GitHub answers. Never fails the launch. ICMP ping is often blocked
# (it is on the PC), so this probes GitHub over HTTPS instead.
function Update-Source {
    # Native commands write progress to stderr; don't let that count as an error here.
    $ErrorActionPreference = 'Continue'
    git rev-parse --abbrev-ref '@{u}' *> $null
    if ($LASTEXITCODE -ne 0) { return }
    try {
        Invoke-WebRequest -Uri https://github.com -Method Head -UseBasicParsing -TimeoutSec 3 | Out-Null
    } catch {
        Write-Host "update: GitHub unreachable, running what's checked out"
        return
    }
    Write-Host 'update: GitHub reachable, pulling'
    # --ff-only never merges or overwrites: diverged or conflicting local work just stays as is.
    # The low-speed settings abort a stalled transfer after 15s.
    git -c http.lowSpeedLimit=1000 -c http.lowSpeedTime=15 pull --ff-only
    if ($LASTEXITCODE -ne 0) { Write-Host "update: pull failed, running what's checked out" }
}
Update-Source

# Only one copy runs at a time, and Windows won't let cargo replace a running exe, so stop any
# copy that's already running (from this script's last run, or started some other way).
Get-Process crossglide -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 300

cargo run -p crossglide-ui
