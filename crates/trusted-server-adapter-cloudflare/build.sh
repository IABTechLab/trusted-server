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

# worker-build 0.8+ requires worker >= 0.8.4, but this crate and the pinned
# edgezero adapter use worker 0.7. Install a matching 0.7-series worker-build
# into a crate-local root so a newer globally-installed worker-build (used by
# other projects) is neither required nor disturbed. The version guard also
# re-pins if the local copy is somehow on a non-0.7 series.
WORKER_BUILD_ROOT="$SCRIPT_DIR/.worker-build"
WORKER_BUILD_BIN="$WORKER_BUILD_ROOT/bin/worker-build"
if ! "$WORKER_BUILD_BIN" --version 2>/dev/null | grep -qE '0\.7\.'; then
    # --force so a stale non-0.7 binary already in the root is overwritten;
    # without it `cargo install` refuses to replace the existing binary and the
    # guard can never self-heal an incompatible local worker-build.
    cargo install -q --force --version '^0.7' --root "$WORKER_BUILD_ROOT" worker-build
fi
"$WORKER_BUILD_BIN" --release
