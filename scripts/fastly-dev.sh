#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
config_path="${TRUSTED_SERVER_CONFIG_FILE:-$repo_root/trusted-server.toml}"
output_path="$repo_root/fastly.local.toml"
wasm_release_path="$repo_root/target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm"
wasm_debug_path="$repo_root/target/wasm32-wasip1/debug/trusted-server-adapter-fastly.wasm"

if [[ $# -gt 0 && "$1" != -* ]]; then
  config_path="$1"
  shift
fi

if [[ ! -f "$config_path" ]]; then
  echo "error: config file not found: $config_path" >&2
  echo "hint: cp trusted-server.example.toml trusted-server.toml" >&2
  exit 1
fi

config_path="$(cd "$(dirname "$config_path")" && pwd)/$(basename "$config_path")"

python3 "$repo_root/scripts/render-fastly-local-config.py" \
  --app-config "$config_path" \
  --template "$repo_root/fastly.toml" \
  --output "$output_path"

fastly_args=(compute serve --dir "$repo_root" --env=local)
fastly_args+=("$@")

has_skip_build=false
has_file=false
for arg in "$@"; do
  if [[ "$arg" == "--skip-build" ]]; then
    has_skip_build=true
  fi
  if [[ "$arg" == --file=* || "$arg" == "--file" ]]; then
    has_file=true
  fi
done

if [[ "$has_skip_build" == true && "$has_file" == false ]]; then
  if [[ -f "$wasm_release_path" ]]; then
    fastly_args+=(--file "$wasm_release_path")
  elif [[ -f "$wasm_debug_path" ]]; then
    fastly_args+=(--file "$wasm_debug_path")
  else
    echo "error: --skip-build was passed but no built Wasm binary was found" >&2
    echo "hint: run cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1" >&2
    exit 1
  fi
fi

exec fastly "${fastly_args[@]}"
