#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
test -d vendor/proton-sdk || { echo 'Run scripts/bootstrap-sdk.sh first.' >&2; exit 1; }
test "$(bun --version)" = 1.4.2 || { echo 'This build pins Bun 1.4.2.' >&2; exit 1; }
test "$(git -C vendor/proton-sdk rev-parse HEAD)" = 8ffae007b0fbb8bcef53514719d6f093bb32dcf9
bun install --frozen-lockfile
bun run typecheck
bun test adapter/tests
cargo test --locked --all-features
cargo clippy --locked --all-features --all-targets -- -D warnings
cargo build --locked --features gui
dbus-run-session --config-file=tests/dbus-session.conf -- python3 tests/service-integration.py
bun run build:adapter
cargo build --locked --release --features gui
