#!/usr/bin/env bash
# Measure this checkout's actual managed runner using disposable local state.
set -euo pipefail

SCRIPT_ROOT=$(cd "$(dirname "$0")" && pwd)
MEASURE_REPO=$(cd "$SCRIPT_ROOT/../.." && pwd)
export MEASURE_REPO
# Check the actual integration, rather than ancestry: a squash merge of PR 1506
# must remain eligible while old main must not measure its private client.
python3 - "$MEASURE_REPO" <<'CHECK'
import sys
from pathlib import Path
root = Path(sys.argv[1]) / "crates/trusted-server-adapter-axum/src"
if "run_app::<TrustedServerApp>" not in (root / "main.rs").read_text() or "request_outbound_client(ctx)" not in (root / "platform.rs").read_text():
    raise SystemExit("The managed runner/client integration from PR 1506 is required.")
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

cp "$SCRIPT_ROOT/launch.py" "$SCRIPT_ROOT/exercise.mjs" "$SCRIPT_ROOT/relay.mjs" "$SCRIPT_ROOT/analyze.py" "$MEASURE_OUTPUT/"
cp -R "$SCRIPT_ROOT/origin-app" "$MEASURE_OUTPUT/origin-app"
cp "$MEASURE_REPO/crates/trusted-server-integration-tests/fixtures/frameworks/nextjs/package.json" \
   "$MEASURE_REPO/crates/trusted-server-integration-tests/fixtures/frameworks/nextjs/package-lock.json" \
   "$MEASURE_OUTPUT/origin-app/"

# Keep dependencies and generated bundles out of the retained evidence.
cleanup() {
    rm -rf "$MEASURE_OUTPUT/origin-app/node_modules" "$MEASURE_OUTPUT/origin-app/.next"
}
trap cleanup EXIT

(cd "$MEASURE_REPO" && cargo build --locked -p trusted-server-adapter-axum -p trusted-server-cli) > "$MEASURE_OUTPUT/build.log" 2>&1
(cd "$MEASURE_OUTPUT/origin-app" && npm ci && npm run build) > "$MEASURE_OUTPUT/next-build.log" 2>&1
if [ -z "${PLAYWRIGHT_PACKAGE:-}" ]; then
    PLAYWRIGHT_PACKAGE="$MEASURE_REPO/crates/trusted-server-integration-tests/browser/package.json"
    (cd "$(dirname "$PLAYWRIGHT_PACKAGE")" && npm ci) > "$MEASURE_OUTPUT/browser-install.log" 2>&1
fi
export PLAYWRIGHT_PACKAGE
if [ "${CI:-}" = true ]; then
    (cd "$(dirname "$PLAYWRIGHT_PACKAGE")" && npx --no-install playwright install --with-deps chromium) >> "$MEASURE_OUTPUT/browser-install.log" 2>&1
else
    (cd "$(dirname "$PLAYWRIGHT_PACKAGE")" && npx --no-install playwright install chromium) >> "$MEASURE_OUTPUT/browser-install.log" 2>&1
fi

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
python3 - "$MEASURE_OUTPUT" "$MEASURE_REPO" <<'PY'
import hashlib
import json
import os
import platform
import subprocess
import sys
import tomllib
from pathlib import Path
out, repo = map(Path, sys.argv[1:])
metadata = {
    "edgezero_source": next(package["source"] for package in tomllib.loads((repo / "Cargo.lock").read_text())["package"] if package["name"] == "edgezero-adapter-axum"),
    "fixture_inputs_sha256": {str(path.relative_to(repo)): hashlib.sha256(path.read_bytes()).hexdigest() for path in (repo / "scripts/measure-axum-nextjs").rglob("*") if path.is_file() and path.suffix in [".py", ".mjs", ".tsx", ".ts", ".sh"]},
    "next_version": json.loads((out / "origin-app/node_modules/next/package.json").read_text())["version"],
    "runtime_revision": subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip(),
    "runtime_source_diff": subprocess.check_output(["git", "-C", str(repo), "diff", "HEAD", "--", "crates", "Cargo.toml", "Cargo.lock"], text=True),
    "rust": subprocess.check_output(["rustc", "--version"], text=True).strip(),
    "node": subprocess.check_output(["node", "--version"], text=True).strip(),
    "platform": platform.platform(),
    "axum_binary_sha256": hashlib.sha256(Path(os.environ["MEASURE_BIN"]).read_bytes()).hexdigest(),
    "github_run_id": os.environ.get("GITHUB_RUN_ID"),
    "limits": "Local HTTP/1; allowed proxy origin; sequential targets; timings are descriptive, not pooling's causal benefit",
}
(out / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
PY
python3 "$MEASURE_OUTPUT/launch.py" > "$MEASURE_OUTPUT/run.log" 2>&1
python3 "$MEASURE_OUTPUT/analyze.py"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    cat "$MEASURE_OUTPUT/summary.md" >> "$GITHUB_STEP_SUMMARY"
fi
