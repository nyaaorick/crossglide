#!/bin/zsh
# Builds and runs the tray app from source (a dev build) in a Terminal window, so the log shows up
# there. For testing on the Mac: double-click this file in Finder, or drag it into Terminal and
# press Enter. Quit from the menu bar icon, or press Ctrl-C.
#
# Each launch first checks that GitHub is reachable and, if it is, fast-forwards to the latest
# commit. Offline, or with local commits the remote doesn't have, it just runs what's checked out.

# zsh reads a script as it runs it, and the pull below can rewrite this very file, so everything
# lives in a function: zsh parses it whole before running any of it. Inside a function $0 is the
# function's name, so the script's own directory is passed in from the top level, below.
main() {
    cd "$1" || exit 1

    # Finder starts this with a minimal PATH, which doesn't include cargo.
    [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

    update

    # Only one copy runs at a time, so stop any copy that's already running (from this script's
    # last run, or started some other way).
    pkill -x crossglide 2>/dev/null && sleep 0.3

    cargo build -p crossglide-ui || exit 1
    # Signed, so the Accessibility permission "Start at login" needs survives the next rebuild.
    scripts/sign-mac.sh target/debug/crossglide
    exec target/debug/crossglide
}

# Pulls the latest commit if GitHub answers. Never fails the launch. ICMP ping is often blocked, so
# this probes GitHub over HTTPS instead.
update() {
    git rev-parse --abbrev-ref '@{u}' >/dev/null 2>&1 || return 0
    if ! curl -fsS -o /dev/null --max-time 3 https://github.com 2>/dev/null; then
        echo "update: GitHub unreachable, running what's checked out"
        return 0
    fi
    echo "update: GitHub reachable, pulling"
    # --ff-only never merges or overwrites: diverged or conflicting local work just stays as is.
    # The low-speed settings abort a stalled transfer after 15s.
    git -c http.lowSpeedLimit=1000 -c http.lowSpeedTime=15 pull --ff-only \
        || echo "update: pull failed, running what's checked out"
}

main "${0:A:h}/.."
exit
