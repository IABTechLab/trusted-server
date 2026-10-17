#!/usr/bin/env bash
# Builds the permissions inspector's WebAssembly engine from the real
# trusted-server-core code and places it beside the inspector page.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build \
  --manifest-path tools/permissions-inspector/wasm/Cargo.toml \
  --release --target wasm32-unknown-unknown
cp tools/permissions-inspector/wasm/target/wasm32-unknown-unknown/release/permissions_inspector_wasm.wasm \
   tools/permissions-inspector/permissions_inspector_wasm.wasm
echo "Built tools/permissions-inspector/permissions_inspector_wasm.wasm"

# Lists the repository's sample permissions files for the page's dropdown,
# labeled by each file's `name:` line, or the file name when it has none.
manifest="tools/permissions-inspector/policies.json"
{
  printf '['
  first=1
  for f in config/permissions/*.yaml; do
    base=$(basename "$f")
    name=$(sed -n 's/^name:[[:space:]]*//p' "$f" | head -1)
    [ -z "$name" ] && name="$base"
    [ $first -eq 1 ] || printf ','
    first=0
    printf '{"file":"%s","name":"%s"}' "$base" "$name"
  done
  printf ']\n'
} > "$manifest"
echo "Wrote $manifest"
