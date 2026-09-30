set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# List recipes
default:
    @just --list

# Run the agent
dev *args:
    cargo run -p crossglide-agent -- {{args}}

# Build and run the tray app from source, with its log in the terminal
tray:
    cargo run -p crossglide-ui

# Run all Rust tests
test:
    cargo test --workspace

# Check formatting and clippy warnings
lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings

# Run the M1 audio spike, e.g. `just spike recv` (see ROADMAP.md)
spike *args:
    cargo run -p crossglide-spike -- {{args}}

# Check dependencies for advisories, bans and unknown sources
deny:
    cargo deny check advisories bans sources

# Build, sign and install the virtual precision touchpad driver (Windows, administrator)
[windows]
touchpad-driver *args:
    powershell -ExecutionPolicy Bypass -File drivers\touchpad\build.ps1 {{args}}
