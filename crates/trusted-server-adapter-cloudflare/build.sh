#!/usr/bin/env bash
# Source nvm so cargo's build.rs subprocess inherits the native arm64 Node.
# Without this, bash -lc tasks find the Rosetta x64 system node, which causes
# rollup to look for @rollup/rollup-darwin-x64 instead of the arm64 binary.
export NVM_DIR="$HOME/.nvm"
[ -s "$NVM_DIR/nvm.sh" ] && \. "$NVM_DIR/nvm.sh"

# Source the root .env (same values used by the Fastly and Axum dev tasks).
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_ENV="$SCRIPT_DIR/../../.env"
[ -f "$ROOT_ENV" ] && set -a && source "$ROOT_ENV" && set +a

# Allow cloudflare-specific overrides on top (not committed).
# Copy .env.cloudflare.dev.example → .env.cloudflare.dev to customise.
[ -f "$SCRIPT_DIR/.env.cloudflare.dev" ] && . "$SCRIPT_DIR/.env.cloudflare.dev"

# worker-build must run from the crate root (where Cargo.toml lives) regardless
# of which directory wrangler was invoked from.
cd "$SCRIPT_DIR"

# Pin worker-build to the exact `worker` crate version resolved in Cargo.lock.
# worker-build is released in lockstep with worker and downloads the wasm-bindgen
# CLI matching the locked `wasm-bindgen`. A floating `^0.8` install pulled
# worker-build 0.8.5, which passes `--force-enable-abort-handler` to wasm-bindgen
# — a flag the CLI matching the pinned wasm-bindgen (0.2.123, via worker 0.8.4)
# does not understand. Tracking the locked worker version keeps the toolchain in
# lockstep, and the pin advances automatically when the lockfile bumps.
WORKER_VERSION="$(
  grep -A1 '^name = "worker"$' "$SCRIPT_DIR/../../Cargo.lock" |
    sed -nE 's/^version = "(.*)"/\1/p' | head -1
)"
if [ -z "$WORKER_VERSION" ]; then
  echo "error: could not determine the worker crate version from Cargo.lock" >&2
  exit 1
fi
cargo install -q --version "=$WORKER_VERSION" worker-build && worker-build --release
