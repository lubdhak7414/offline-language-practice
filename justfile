# Local equivalents of what CI runs. `just` is optional — every recipe is a
# one-liner you can paste — but keeping them here means CI and a laptop run
# the same commands instead of drifting apart.
#
# Install: https://github.com/casey/just

manifest := "src-tauri/Cargo.toml"

# List the recipes.
default:
    @just --list

# Format Rust in place.
fmt:
    cargo fmt --manifest-path {{manifest}}

# Everything CI checks, in CI's order. Run this before pushing.
check: fmt-check lint test

fmt-check:
    cargo fmt --manifest-path {{manifest}} --check

lint:
    cargo clippy --manifest-path {{manifest}} --locked --all-targets -- -D warnings

# Rust and frontend suites. Network-tagged tests stay out; see `test-net`.
test:
    cargo test --manifest-path {{manifest}} --locked --all-targets
    npm test

# The handful of tests that actually talk to Hugging Face. Run after
# re-pinning a model revision — they are the only check that the pinned
# URLs still resolve and still hash to what the catalog claims.
test-net:
    cargo test --manifest-path {{manifest}} --lib network_ -- --ignored --nocapture

# Dependency audit: advisories, licenses, duplicate crates, source origins.
deny:
    cargo deny --manifest-path {{manifest}} check

# Fetch the models to ./models (the app can also do this on first run).
models:
    ./scripts/download-models.sh

# Run the app. `beforeDevCommand` starts Vite, so this is the only terminal
# you need.
dev:
    npm run tauri dev

# Release build for the current platform.
build:
    npm run tauri build
