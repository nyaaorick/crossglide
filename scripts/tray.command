#!/bin/zsh
# Builds and runs the tray app from source (a dev build) in a Terminal window, so the log shows up
# there. For testing on the Mac: double-click this file in Finder, or drag it into Terminal and
# press Enter. Quit from the menu bar icon, or press Ctrl-C.
cd "${0:A:h}/.." || exit 1

# Finder starts this with a minimal PATH, which doesn't include cargo.
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

# Only one copy runs at a time, so stop any copy that's already running (from this script's last
# run, or started some other way).
pkill -x crossglide 2>/dev/null && sleep 0.3

cargo run -p crossglide-ui
