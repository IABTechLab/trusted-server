#!/usr/bin/env bash
# Fail if a workspace crate built into the Cloudflare Worker calls a std clock
# method that panics with `RuntimeError: unreachable` on wasm32-unknown-unknown
# (std::time::Instant::{now,elapsed}, std::time::SystemTime::{now,elapsed});
# use `web_time` instead.
#
# This cannot live in the workspace clippy.toml: on native and wasm32-wasip1,
# `web_time::Instant` and `web_time::SystemTime` are re-exports of the std
# types, so clippy would flag the compliant `web_time` calls too. On
# wasm32-unknown-unknown the types are distinct, so only that target can tell
# them apart. The dedicated config dir holds just the disallowed-methods rule.
#
# Linting the Cloudflare adapter covers trusted-server-core and the other
# workspace crates it builds, with the production feature set, and reuses the
# dependency artifacts from `cargo clippy-cloudflare-wasm`.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

CLIPPY_CONF_DIR="$root/scripts/clippy-wasm-clock" \
    cargo clippy -p trusted-server-adapter-cloudflare --target wasm32-unknown-unknown \
    --features cloudflare --lib -- -A clippy::all -D clippy::disallowed_methods
