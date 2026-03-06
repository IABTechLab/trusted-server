#!/usr/bin/env bash
# Regenerate src/generated.rs from proto/openrtb.proto.
#
# This only needs to be run when the proto file changes. Normal builds use the
# checked-in generated.rs directly — no protoc required.
#
# Usage:
#   ./crates/openrtb/generate.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CODEGEN_DIR="$SCRIPT_DIR/../openrtb-codegen"

# Check for protoc
if ! command -v protoc &>/dev/null; then
    echo "error: protoc is required but not installed" >&2
    echo "Install from https://github.com/protocolbuffers/protobuf/releases" >&2
    echo "  macOS:  brew install protobuf" >&2
    echo "  Ubuntu: apt install -y protobuf-compiler" >&2
    exit 1
fi

# Build as a native binary (the repo's .cargo/config.toml defaults to wasm32).
NATIVE_TARGET="$(rustc -vV | grep '^host:' | cut -d' ' -f2)"

echo "protoc version: $(protoc --version)"
echo "Generating OpenRTB types from proto/openrtb.proto..."

cargo run \
    --manifest-path "$CODEGEN_DIR/Cargo.toml" \
    --target "$NATIVE_TARGET"

echo "Formatting generated code..."
rustfmt "$SCRIPT_DIR/src/generated.rs"

echo "Done."
