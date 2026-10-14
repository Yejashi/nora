#!/usr/bin/env bash
# N30: verify nora builds reproducibly from a clean checkout.
#
# Acceptance: `cargo clean` followed by `cargo build --release` succeeds
# with no prior build state, and the resulting binary runs. Set
# CARGO_TARGET_DIR to point the build at a separate directory (e.g. in
# CI or tests) so the shared target directory is not wiped.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> cargo clean"
cargo clean

echo "==> cargo build --release"
cargo build --release

BIN="${CARGO_TARGET_DIR:-target}/release/nora"
echo "==> verifying the binary runs"
"$BIN" --version
"$BIN" --help
# Headless config check: no display or GPU needed.
"$BIN" --check-config

echo "clean build OK: $("$BIN" --version)"
