#!/usr/bin/env bash
set -euo pipefail

wrangler_version="$(awk '$1 == "wrangler" { print $2 }' .tool-versions)"
test -n "$wrangler_version"
npm install --global "wrangler@$wrangler_version"
test "$(wrangler --version)" = "$wrangler_version"
