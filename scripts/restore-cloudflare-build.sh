#!/usr/bin/env bash
set -euo pipefail

: "${CF_BUILD_ARTIFACT_PATH:?CF_BUILD_ARTIFACT_PATH is required}"
mkdir -p crates/trusted-server-adapter-cloudflare/build
cp -R "$CF_BUILD_ARTIFACT_PATH/." crates/trusted-server-adapter-cloudflare/build/
