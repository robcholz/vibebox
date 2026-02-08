#!/usr/bin/env bash
set -euo pipefail

cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all
cargo build --all-targets
