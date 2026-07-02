#!/usr/bin/env bash
set -euo pipefail

HOST_TARGET="${1:-$(rustc -vV | awk '/host:/ { print $2 }')}"
if [ -z "$HOST_TARGET" ]; then
  echo "Failed to detect host target" >&2
  exit 1
fi

if ! command -v rustup >/dev/null 2>&1; then
  echo "rustup not found; cannot ensure host target $HOST_TARGET is installed" >&2
  echo "Run: cargo test --package trusted-server-cli --target $HOST_TARGET" >&2
  exit 1
fi

if ! rustup target list --installed | awk -v target="$HOST_TARGET" '$0 == target { found = 1 } END { exit found ? 0 : 1 }'; then
  echo "Installing Rust target: $HOST_TARGET"
  rustup target add "$HOST_TARGET"
fi

cargo test --package trusted-server-cli --target "$HOST_TARGET"
