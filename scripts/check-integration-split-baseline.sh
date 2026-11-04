#!/usr/bin/env bash
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo"
host=$(rustc --version --verbose | awk '/^host: / { print $2 }')

# Build the candidate, not a retained baseline binary or an old cache entry.
cargo build --locked -p trusted-server-cli --target "$host" --bin ts
target_dir=$(cargo metadata --locked --no-deps --format-version 1 | node --input-type=module -e '
  import { readFileSync } from "node:fs";
  console.log(JSON.parse(readFileSync(0, "utf8")).target_directory);
')
export INTEGRATION_SPLIT_BASELINE_TS="$target_dir/$host/debug/ts"
cargo test --locked -p trusted-server-integration-tests --target "$host" \
  --test integration_split_baseline -- --include-ignored \
  --skip capture_corrected_baseline "$@"
