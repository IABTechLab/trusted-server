#!/usr/bin/env bash

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tool_versions="$repository_root/.tool-versions"

node_version="$(awk '$1 == "nodejs" { print $2 }' "$tool_versions")"
rust_version="$(awk '$1 == "rust" { print $2 }' "$tool_versions")"

test -n "$node_version"
test -n "$rust_version"
test -n "${GITHUB_OUTPUT:-}"

printf 'node=%s\n' "$node_version" >> "$GITHUB_OUTPUT"
printf 'rust=%s\n' "$rust_version" >> "$GITHUB_OUTPUT"
