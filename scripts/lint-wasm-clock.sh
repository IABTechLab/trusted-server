#!/usr/bin/env bash
# Fail if trusted-server-core calls std::time::Instant::now or
# std::time::SystemTime::now. Both panic with `RuntimeError: unreachable` on
# wasm32-unknown-unknown (Cloudflare Workers); use `web_time` instead.
#
# This cannot live in the workspace clippy.toml: on native and wasm32-wasip1,
# `web_time::Instant` and `web_time::SystemTime` are re-exports of the std
# types, so clippy would flag the compliant `web_time` calls too. On
# wasm32-unknown-unknown the types are distinct, so only that target can tell
# them apart. The dedicated config dir holds just the disallowed-methods rule.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

CLIPPY_CONF_DIR="$root/scripts/clippy-wasm-clock" \
    cargo clippy -p trusted-server-core --lib --target wasm32-unknown-unknown -- \
    -A clippy::all -D clippy::disallowed_methods
