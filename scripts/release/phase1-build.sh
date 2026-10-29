#!/usr/bin/env bash
# Phase 1 ordinary hosted build; no reproducibility or complete composition claim.
set -euo pipefail
build_package=${1:?package required}
build_target=${2:?target required}
release_asset=${3:?asset required}
case "$build_package/$build_target/$release_asset" in
  trusted-server-adapter-fastly/wasm32-wasip1/trusted-server-fastly.wasm) executable=trusted-server-adapter-fastly.wasm ;;
  trusted-server-cli/x86_64-unknown-linux-gnu/ts-x86_64-unknown-linux-gnu|trusted-server-cli/aarch64-apple-darwin/ts-aarch64-apple-darwin|trusted-server-cli/x86_64-apple-darwin/ts-x86_64-apple-darwin) executable=ts ;;
  trusted-server-adapter-axum/x86_64-unknown-linux-gnu/trusted-server-axum-x86_64-unknown-linux-gnu|trusted-server-adapter-axum/x86_64-unknown-linux-musl/trusted-server-axum-x86_64-unknown-linux-musl|trusted-server-adapter-axum/aarch64-unknown-linux-gnu/trusted-server-axum-aarch64-unknown-linux-gnu) executable=trusted-server-axum ;;
  *) echo 'Unsupported release build selection' >&2; exit 1 ;;
esac
: "${RUNNER_TEMP:?}"
: "${SOURCE_COMMIT:?}"
[[ "$SOURCE_COMMIT" =~ ^[a-f0-9]{40}$ ]] || { echo 'Invalid source commit' >&2; exit 1; }
[[ "$(git rev-parse HEAD)" == "$SOURCE_COMMIT" ]] || { echo 'Source commit mismatch' >&2; exit 1; }
for variable in TSJS_SKIP_BUILD TSJS_PREBUILT_DIR TSJS_TEST; do
  if printenv "$variable" >/dev/null; then
    echo "Refusing $variable in the release build" >&2
    exit 1
  fi
done
node scripts/release/phase1.mjs audit
# No operator values enter through inherited environment; these fictional canaries
# additionally catch known config embedding regressions in the final executable.
export TRUSTED_SERVER__RELEASE_BUILD=1
export TRUSTED_SERVER__GIT_VERSION="$SOURCE_COMMIT"
export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=https://phase1-origin-canary.example.com
export TRUSTED_SERVER__PUBLISHER__PROXY_SECRET=phase1-secret-canary-32-characters
export EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME=phase1-physical-store-canary
export FASTLY_SERVICE_ID=phase1-service-account-canary
export MACOSX_DEPLOYMENT_TARGET=13.0
inventory="crates/$build_package/phase1.json"
# Refuse existing paths, including dangling symlinks, before claiming cleanup ownership.
for owned_path in trusted-server.toml "$inventory"; do
  if [[ -e "$owned_path" || -L "$owned_path" ]]; then
    echo "Refusing existing release temporary path: $owned_path" >&2
    exit 1
  fi
done
trap 'rm -f trusted-server.toml "$inventory"; if [[ -n "${build_container:-}" ]]; then docker rm -f "$build_container" >/dev/null 2>&1 || true; fi' EXIT
cat > trusted-server.toml <<'CONFIG'
# Fictional config must never become a build input.
[publisher]
origin_url = "https://phase1-origin-canary.example.com"
proxy_secret = "phase1-secret-canary-32-characters"
[permissions]
policy = "phase1-permission-policy-canary"
CONFIG
export CARGO_TARGET_DIR="$RUNNER_TEMP/phase1-target"
[[ ! -e "$CARGO_TARGET_DIR" ]]
mkdir -p release-assets
[[ -z "$(ls -A release-assets)" ]]
# The pinned Node distribution includes npm. npm verifies lockfile integrity for
# fetched packages; no generator dependencies are installed with unpinned npx.
npm --prefix crates/trusted-server-js/lib ci
if [[ "$build_target" == *-linux-gnu ]]; then
  builder_base=registry.access.redhat.com/ubi9/ubi:9.6@sha256:dec374e05cc13ebbc0975c9f521f3db6942d27f8ccdf06b180160490eef8bdbc
  docker build --build-arg "BASE_IMAGE=$builder_base" --iidfile "$RUNNER_TEMP/phase1-builder-id" - <<'DOCKERFILE'
ARG BASE_IMAGE
FROM ${BASE_IMAGE}
RUN dnf install -y gcc gcc-c++ glibc-devel git-core cmake make perl && dnf clean all
DOCKERFILE
  builder=$(cat "$RUNNER_TEMP/phase1-builder-id")
  echo "GNU builder: $builder_base $builder"
  build_container=phase1-build-$RANDOM-$$
  mkdir "$CARGO_TARGET_DIR"
  docker run --rm --name "$build_container" --workdir /source \
    --mount "type=bind,src=$PWD,dst=/source" \
    --mount "type=bind,src=$CARGO_TARGET_DIR,dst=/target" \
    --mount "type=bind,src=$(dirname "$(dirname "$(node -p process.execPath)")"),dst=/opt/node,readonly" \
    --mount "type=bind,src=$(readlink -f "$(command -v rustup)"),dst=/usr/local/bin/rustup,readonly" \
    --env TRUSTED_SERVER__RELEASE_BUILD --env TRUSTED_SERVER__GIT_VERSION --env TRUSTED_SERVER__PUBLISHER__ORIGIN_URL \
    --env TRUSTED_SERVER__PUBLISHER__PROXY_SECRET --env EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME \
    --env FASTLY_SERVICE_ID --env CARGO_TARGET_DIR=/target \
    "$builder" bash -euc '
      export PATH=/usr/local/bin:/opt/node/bin:/usr/bin:/bin CARGO_HOME=/tmp/cargo RUSTUP_HOME=/tmp/rustup
      git config --global --add safe.directory /source
      ldd --version | head -1 | grep -F "2.34"
      rust_version=$(sed -n '\''s/^channel = "\([^"]*\)"/\1/p'\'' rust-toolchain.toml)
      rustup toolchain install "$rust_version" --profile minimal --no-self-update
      rustup target add --toolchain "$rust_version" "$2"
      export PATH="$(dirname "$(rustup which --toolchain "$rust_version" rustc)"):$PATH"
      rustup run "$rust_version" cargo build --package "$1" --release --target "$2" --locked
    ' phase1 "$build_package" "$build_target"
else
  if [[ "$build_target" == x86_64-unknown-linux-musl ]]; then
    sudo apt-get update
    sudo apt-get install --yes --no-install-recommends musl=1.2.4-2 musl-dev=1.2.4-2 musl-tools=1.2.4-2
    export REALGCC=gcc-13 CC_x86_64_unknown_linux_musl=musl-gcc CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS='-C target-feature=+crt-static'
    musl-gcc -dumpfullversion
  fi
  cargo build --package "$build_package" --release --target "$build_target" --locked
fi
cp "$CARGO_TARGET_DIR/$build_target/release/$executable" "release-assets/$release_asset"
shasum -a 256 "release-assets/$release_asset" > "$RUNNER_TEMP/binary.sha256"
node scripts/release/phase1.mjs check "release-assets/$release_asset"
node scripts/release/phase1.mjs smoke "release-assets/$release_asset" "$build_target"
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
  "$inventory" "$tool_root/npm.cdx.json" \
  "release-assets/$release_asset" "$tool_root/combined.cdx.json"
# Convert the internal dependency graph into real SPDX 2.3 before publication.
case "$build_target" in
  wasm32-wasip1|x86_64-unknown-linux-gnu|x86_64-unknown-linux-musl)
    converter_asset=cyclonedx-linux-x64
    converter_sha=bfc8b2538da86fe239bc53658bbb63c1c8c510a293c1e6891aa5bea5d3c58746 ;;
  aarch64-unknown-linux-gnu)
    converter_asset=cyclonedx-linux-arm64
    converter_sha=b2e9fdf9665ef49868a2ec012171c6e785dcd69745bc5869e53e4f4bfb096a5f ;;
  aarch64-apple-darwin)
    converter_asset=cyclonedx-osx-arm64
    converter_sha=750c148780154833f6401f9067d08c5a4c31567b6ee3c26c062c3a95c62d741c ;;
  x86_64-apple-darwin)
    converter_asset=cyclonedx-osx-x64
    converter_sha=0ea92306fc7d7c30ed112a7463781a200585a3bf1cdaf77229181c5aabd1d4b6 ;;
esac
curl --fail --silent --show-error --location --retry 3 \
  "https://github.com/CycloneDX/cyclonedx-cli/releases/download/v0.33.1/$converter_asset" \
  --output "$tool_root/converter"
printf '%s  %s\n' "$converter_sha" "$tool_root/converter" | shasum -a 256 --check
chmod +x "$tool_root/converter"
"$tool_root/converter" convert --input-file "$tool_root/combined.cdx.json" \
  --input-format json --output-format spdxjson \
  --output-file "release-assets/$release_asset.spdx.json"
node scripts/release/phase1.mjs validate-spdx \
  "release-assets/$release_asset.spdx.json" "release-assets/$release_asset" \
  "$tool_root/combined.cdx.json"
shasum -a 256 --check "$RUNNER_TEMP/binary.sha256"
git diff --exit-code -- Cargo.lock crates/trusted-server-js/lib/package-lock.json edgezero.toml
