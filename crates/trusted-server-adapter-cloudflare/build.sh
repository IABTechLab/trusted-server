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
[ -f "$SCRIPT_DIR/.env.cloudflare.dev" ] && set -a && . "$SCRIPT_DIR/.env.cloudflare.dev" && set +a

# worker-build must run from the crate root (where Cargo.toml lives) regardless
# of which directory wrangler was invoked from.
cd "$SCRIPT_DIR"

# worker-build must match the worker crate's 0.7 API surface pinned in Cargo.toml.
# A globally installed worker-build 0.8.x rejects worker 0.7 with
# "Unsupported version worker@0.7.x, expected at least worker@0.8.4", so check the
# installed major.minor and (re)install ^0.7 when it is absent or mismatched
# rather than only when it is missing.
worker_build_version=""
if command -v worker-build >/dev/null 2>&1; then
    worker_build_version="$(worker-build --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)"
fi
case "$worker_build_version" in
    0.7.*) ;;
    *) cargo install -q --version '^0.7' --force worker-build ;;
esac
worker-build --release
