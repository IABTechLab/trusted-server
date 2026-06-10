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
command -v worker-build >/dev/null 2>&1 || cargo install -q --version '^0.7' worker-build
worker-build --release
