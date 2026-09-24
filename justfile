set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# List recipes
default:
    @just --list

# Run the agent
dev *args:
    cargo run -p crossglide-agent -- {{args}}

# Run all Rust tests
test:
    cargo test --workspace

# Check formatting and clippy warnings
lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings

# Check dependencies for advisories, bans and unknown sources
deny:
    cargo deny check advisories bans sources

# Fetch and build the Deskflow core into build/upstream
[macos]
core:
    git submodule update --init upstream
    [ -f build/upstream/build.ninja ] || cmake -S upstream -B build/upstream -G Ninja -DCMAKE_BUILD_TYPE=RelWithDebInfo -DBUILD_INSTALLER=OFF -DSKIP_BUILD_TESTS=ON -DCMAKE_PREFIX_PATH="$(brew --prefix qt);$(brew --prefix openssl@3)"
    cmake --build build/upstream

[windows]
core:
    Write-Error "Building the Deskflow core on Windows isn't set up yet (planned for M5)"; exit 1
