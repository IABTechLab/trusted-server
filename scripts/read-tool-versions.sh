#!/usr/bin/env bash
set -euo pipefail

: "${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"

tool_version() {
  local tool="$1"
  local version
  version="$(awk -v tool="$tool" '$1 == tool { print $2 }' .tool-versions)"
  test -n "$version"
  printf '%s\n' "$version"
}

{
  printf 'rust=%s\n' "$(tool_version rust)"
  printf 'node=%s\n' "$(tool_version nodejs)"
  printf 'wrangler=%s\n' "$(tool_version wrangler)"
  printf 'fastly=%s\n' "$(tool_version fastly)"
  printf 'viceroy=%s\n' "$(tool_version viceroy)"
} >> "$GITHUB_OUTPUT"
