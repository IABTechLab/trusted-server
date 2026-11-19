#!/usr/bin/env bash
# Measure this checkout's actual managed runner using disposable local state.
set -euo pipefail

SCRIPT_ROOT=$(cd "$(dirname "$0")" && pwd)
MEASURE_REPO=$(cd "$SCRIPT_ROOT/../.." && pwd)
export MEASURE_REPO
# Check the integration rather than ancestry, so squash merges remain eligible.
node --input-type=module - "$MEASURE_REPO" <<'CHECK'
import {readFileSync} from 'node:fs';
import {resolve} from 'node:path';
const root=resolve(process.argv[2],'crates/trusted-server-adapter-axum/src');
if (!readFileSync(resolve(root,'main.rs'),'utf8').includes('run_app::<TrustedServerApp>') || !readFileSync(resolve(root,'platform.rs'),'utf8').includes('request_outbound_client(ctx)')) {
  throw new Error('The managed runner/client integration from PR 1506 is required.');
}
CHECK
MEASURE_NODE=$(awk '$1 == "nodejs" {print $2}' "$MEASURE_REPO/.tool-versions")
MEASURE_RUST=$(awk '$1 == "rust" {print $2}' "$MEASURE_REPO/.tool-versions")
[ "$(node --version)" = "v$MEASURE_NODE" ] || { echo "Select the repository-pinned Node." >&2; exit 1; }
export RUSTUP_TOOLCHAIN="$MEASURE_RUST"
export NEXT_TELEMETRY_DISABLED=1
OUTPUT_INPUT=${1:?usage: run.sh EMPTY_OUTPUT_DIRECTORY}
mkdir -p "$OUTPUT_INPUT"
MEASURE_OUTPUT=$(cd "$OUTPUT_INPUT" && pwd)
[ -z "$(ls -A "$MEASURE_OUTPUT")" ] || { echo "Output directory must be empty." >&2; exit 1; }
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$MEASURE_REPO/target}"
mkdir -p "$CARGO_TARGET_DIR"
CARGO_TARGET_DIR=$(cd "$CARGO_TARGET_DIR" && pwd)
export CARGO_TARGET_DIR

cp "$SCRIPT_ROOT/"*.ts "$SCRIPT_ROOT/package.json" "$MEASURE_OUTPUT/"
cp -R "$SCRIPT_ROOT/origin-app" "$MEASURE_OUTPUT/origin-app"
cp "$MEASURE_REPO/crates/trusted-server-integration-tests/fixtures/frameworks/nextjs/package.json" \
   "$MEASURE_REPO/crates/trusted-server-integration-tests/fixtures/frameworks/nextjs/package-lock.json" \
   "$MEASURE_OUTPUT/origin-app/"
# Next.js 14 consumes this configuration format; keep it disposable.
cat > "$MEASURE_OUTPUT/origin-app/next.config.mjs" <<'NEXT'
export default {experimental:{cpus:2,serverActions:{allowedOrigins:['127.0.0.1:18982']}}};
NEXT

MEASURE_PHASE=build
cleanup() {
    local status=$?
    if [ "$status" -ne 0 ] && [ -f "$MEASURE_OUTPUT/$MEASURE_PHASE.log" ]; then
        tail -n 60 "$MEASURE_OUTPUT/$MEASURE_PHASE.log" >&2
    fi
    rm -rf "$MEASURE_OUTPUT/origin-app/node_modules" "$MEASURE_OUTPUT/origin-app/.next"
}
trap cleanup EXIT

echo "Building the current checkout's native Axum runner and CLI."
(cd "$MEASURE_REPO" && cargo build --locked -p trusted-server-adapter-axum -p trusted-server-cli) > "$MEASURE_OUTPUT/build.log" 2>&1
MEASURE_PHASE=next-build
echo "Installing and building the locked Next.js fixture."
(cd "$MEASURE_OUTPUT/origin-app" && npm ci && npm run build) > "$MEASURE_OUTPUT/next-build.log" 2>&1
MEASURE_PHASE=typescript
echo "Type-checking the measurement scripts with the fixture's locked compiler."
node "$MEASURE_OUTPUT/origin-app/node_modules/typescript/bin/tsc" --noEmit --module nodenext --moduleResolution nodenext --target es2022 --allowImportingTsExtensions --skipLibCheck --types node --typeRoots "$MEASURE_OUTPUT/origin-app/node_modules/@types" "$MEASURE_OUTPUT/"*.ts > "$MEASURE_OUTPUT/typescript.log" 2>&1
MEASURE_PHASE=browser-install
echo "Installing the locked browser; launch will validate system libraries."
if [ -z "${PLAYWRIGHT_PACKAGE:-}" ]; then
    PLAYWRIGHT_PACKAGE="$MEASURE_REPO/crates/trusted-server-integration-tests/browser/package.json"
    (cd "$(dirname "$PLAYWRIGHT_PACKAGE")" && npm ci) > "$MEASURE_OUTPUT/browser-install.log" 2>&1
fi
export PLAYWRIGHT_PACKAGE
# The supported runner image supplies system libraries. Chromium launch retains
# Playwright's own dependency validation; do not upgrade runner packages here.
(cd "$(dirname "$PLAYWRIGHT_PACKAGE")" && npx --no-install playwright install chromium) >> "$MEASURE_OUTPUT/browser-install.log" 2>&1

MEASURE_PHASE=config
echo "Validating a disposable local configuration."
MEASURE_TS="$CARGO_TARGET_DIR/debug/ts"
"$MEASURE_TS" config init --app-config "$MEASURE_OUTPUT/trusted-server.toml" > "$MEASURE_OUTPUT/config.log" 2>&1
cat >> "$MEASURE_OUTPUT/trusted-server.toml" <<'CONFIG'

[integrations.nextjs]
enabled = true
CONFIG
export TRUSTED_SERVER__PUBLISHER__DOMAIN=127.0.0.1
export TRUSTED_SERVER__PUBLISHER__COOKIE_DOMAIN=127.0.0.1
export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=http://127.0.0.1:18981
export TRUSTED_SERVER__PUBLISHER__PUBLIC_ORIGIN=http://127.0.0.1:18982
# Resolve manifest-relative paths without changing the operator's local config.
sed -e "s|= \"crates/|= \"$MEASURE_REPO/crates/|g" \
    -e "s|manifest = \"fastly.toml\"|manifest = \"$MEASURE_REPO/fastly.toml\"|" \
    "$MEASURE_REPO/edgezero.toml" > "$MEASURE_OUTPUT/edgezero.toml"
(cd "$MEASURE_OUTPUT" && "$MEASURE_TS" config validate --manifest edgezero.toml --app-config trusted-server.toml --strict && \
    "$MEASURE_TS" config push --adapter axum --local --manifest edgezero.toml --app-config trusted-server.toml --yes --no-diff) >> "$MEASURE_OUTPUT/config.log" 2>&1
export MEASURE_ENVELOPE="$MEASURE_OUTPUT/.edgezero/local-config-trusted_server_config.json"
export MEASURE_BIN="$CARGO_TARGET_DIR/debug/trusted-server-axum"
node "$MEASURE_OUTPUT/metadata.ts" "$MEASURE_OUTPUT" "$MEASURE_REPO"
MEASURE_PHASE=run
echo "Exercising the real managed runner, Next.js and isolated browser."
node "$MEASURE_OUTPUT/launch.ts" > "$MEASURE_OUTPUT/run.log" 2>&1
MEASURE_PHASE=analysis
echo "Validating raw receipts and publishing descriptive measurements."
node "$MEASURE_OUTPUT/analyze.ts" > "$MEASURE_OUTPUT/analysis.log" 2>&1
test -s "$MEASURE_OUTPUT/summary.json"
test -s "$MEASURE_OUTPUT/summary.md"
cat "$MEASURE_OUTPUT/analysis.log"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    cat "$MEASURE_OUTPUT/summary.md" >> "$GITHUB_STEP_SUMMARY"
fi
