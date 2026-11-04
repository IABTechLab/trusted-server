#!/usr/bin/env bash
# Build a fresh test-utils artifact and exercise terminal I/O on a local runtime.
set -euo pipefail
REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
case "${1:-}" in
  axum|fastly|cloudflare|spin) export HTML_EOF_ADAPTER="$1"; shift ;;
  *) echo "Usage: $0 {axum|fastly|cloudflare|spin} [--release] [--auction]" >&2; exit 2 ;;
esac
export HTML_EOF_RELEASE=false HTML_EOF_AUCTION=false
for argument in "$@"; do
  case "$argument" in
    --release) export HTML_EOF_RELEASE=true ;;
    --auction) export HTML_EOF_AUCTION=true ;;
    *) echo "Unknown argument: $argument" >&2; exit 2 ;;
  esac
done
cd "$REPO_ROOT"
EOF_HOST_TARGET=$(rustc -vV | sed -n 's/host: //p')
exec cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml \
  --target "$EOF_HOST_TARGET" --features html-eof-runtime --test html_eof_runtime \
  actual_runtime -- --ignored --exact --nocapture --test-threads=1
