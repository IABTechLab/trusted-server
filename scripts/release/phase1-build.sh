#!/usr/bin/env bash
# Phase 1 ordinary hosted build; no reproducibility or complete composition claim.
set -euo pipefail

build_package=${1:?package required}
build_target=${2:?target required}
release_asset=${3:?asset required}
case "$build_package/$build_target/$release_asset" in
  trusted-server-adapter-fastly/wasm32-wasip1/trusted-server-fastly.wasm) executable=trusted-server-adapter-fastly.wasm ;;
  trusted-server-cli/x86_64-unknown-linux-gnu/ts-x86_64-unknown-linux-gnu|trusted-server-cli/aarch64-apple-darwin/ts-aarch64-apple-darwin|trusted-server-cli/x86_64-apple-darwin/ts-x86_64-apple-darwin) executable=ts ;;
  *) echo 'Unsupported release build selection' >&2; exit 1 ;;
esac

: "${RUNNER_TEMP:?}"
: "${SOURCE_COMMIT:?}"
[[ "$SOURCE_COMMIT" =~ ^[a-f0-9]{40}$ ]]
[[ "$(git rev-parse HEAD)" == "$SOURCE_COMMIT" ]]
for variable in TSJS_SKIP_BUILD TSJS_PREBUILT_DIR TSJS_TEST; do
  if printenv "$variable" >/dev/null; then
    echo "Refusing $variable in the release build" >&2
    exit 1
  fi
done
node scripts/release/phase1.mjs audit

# No operator values enter through inherited environment; these fictional canaries
# additionally catch known config embedding regressions in the final executable.
export TRUSTED_SERVER__GIT_VERSION="$SOURCE_COMMIT"
export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=https://phase1-origin-canary.example.com
export TRUSTED_SERVER__PUBLISHER__PROXY_SECRET=phase1-secret-canary-32-characters
export EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME=phase1-physical-store-canary
export FASTLY_SERVICE_ID=phase1-service-account-canary
export MACOSX_DEPLOYMENT_TARGET=15.0
[[ ! -e trusted-server.toml ]]
cat > trusted-server.toml <<'CONFIG'
# Fictional config must never become a build input.
[publisher]
origin_url = "https://phase1-origin-canary.example.com"
proxy_secret = "phase1-secret-canary-32-characters"
[permissions]
policy = "phase1-permission-policy-canary"
CONFIG
trap 'rm -f trusted-server.toml' EXIT

export CARGO_TARGET_DIR="$RUNNER_TEMP/phase1-target"
[[ ! -e "$CARGO_TARGET_DIR" ]]
mkdir -p release-assets
[[ -z "$(ls -A release-assets)" ]]

# The pinned Node distribution includes npm. npm verifies lockfile integrity for
# fetched packages; no generator dependencies are installed with unpinned npx.
npm --prefix crates/trusted-server-js/lib ci
cargo build --package "$build_package" --release --target "$build_target" --locked
cp "$CARGO_TARGET_DIR/$build_target/release/$executable" "release-assets/$release_asset"
node scripts/release/phase1.mjs check "release-assets/$release_asset"

# Pin and checksum the generator source before extraction or execution. Cargo's
# --locked installation authenticates its transitive crates via its shipped lock.
tool_root="$RUNNER_TEMP/phase1-cyclonedx"
mkdir "$tool_root"
curl --fail --silent --show-error --location --retry 3 \
  https://crates.io/api/v1/crates/cargo-cyclonedx/0.5.9/download \
  --output "$tool_root/generator.crate"
printf '%s  %s\n' 5d162f67705f0f5038759d73bf546a083bf30e8677c2e944b416bca48d9d69a8 "$tool_root/generator.crate" | shasum -a 256 --check

tar -xf "$tool_root/generator.crate" -C "$tool_root"
CARGO_TARGET_DIR="$tool_root/target" cargo install \
  --path "$tool_root/cargo-cyclonedx-0.5.9" --locked --root "$tool_root/install"
# The generator resolves the whole workspace, including unbuilt members. Acquire
# their selected sources under the lock before denying network resolution.
cargo metadata --locked --format-version 1 --filter-platform "$build_target" > "$tool_root/cargo-metadata.json"
# Pinned generator has no --locked flag. Resolve offline after the locked build
# with identical target/default features, then require the lock to be unchanged.
CARGO_NET_OFFLINE=true "$tool_root/install/bin/cargo-cyclonedx" cyclonedx \
  --manifest-path "crates/$build_package/Cargo.toml" --target "$build_target" \
  --format json --spec-version 1.5 --override-filename phase1
npm --prefix crates/trusted-server-js/lib sbom --sbom-format cyclonedx > "$tool_root/npm.cdx.json"
node scripts/release/phase1.mjs merge \
  "crates/$build_package/phase1.json" "$tool_root/npm.cdx.json" \
  "release-assets/$release_asset" "release-assets/$release_asset.cdx.json"
git diff --exit-code -- Cargo.lock crates/trusted-server-js/lib/package-lock.json edgezero.toml
