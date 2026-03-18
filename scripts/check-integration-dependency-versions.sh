#!/usr/bin/env bash
#
# Ensure the excluded integration-tests crate resolves the same shared direct
# dependency versions as the workspace, and that its own lockfile is current.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

extract_resolved_direct_dependencies() {
    sed -En 's/^([A-Za-z0-9_-]+) v([^ ]+).*/\1\t\2/p' | sort -u
}

workspace_dependencies="$(mktemp)"
integration_dependencies="$(mktemp)"
workspace_names="$(mktemp)"
integration_names="$(mktemp)"
shared_names="$(mktemp)"
trap 'rm -f "$workspace_dependencies" "$integration_dependencies" "$workspace_names" "$integration_names" "$shared_names"' EXIT

cargo tree --workspace --depth 1 --prefix none \
    | extract_resolved_direct_dependencies >"$workspace_dependencies"
cargo tree \
    --manifest-path "$REPO_ROOT/crates/integration-tests/Cargo.toml" \
    --depth 1 \
    --prefix none \
    --locked \
    | extract_resolved_direct_dependencies >"$integration_dependencies"

cut -f1 "$workspace_dependencies" | sort -u >"$workspace_names"
cut -f1 "$integration_dependencies" | sort -u >"$integration_names"
comm -12 "$workspace_names" "$integration_names" >"$shared_names"

status=0
while IFS= read -r dependency_name; do
    workspace_versions="$(
        awk -F "$(printf '\t')" -v dep="$dependency_name" '$1 == dep { print $2 }' \
            "$workspace_dependencies" | sort -u | paste -sd ',' -
    )"
    integration_versions="$(
        awk -F "$(printf '\t')" -v dep="$dependency_name" '$1 == dep { print $2 }' \
            "$integration_dependencies" | sort -u | paste -sd ',' -
    )"

    if [ -z "$workspace_versions" ] || [ -z "$integration_versions" ]; then
        echo "Missing resolved version for shared dependency $dependency_name in one of the lockfiles" >&2
        status=1
        continue
    fi

    if [ "$workspace_versions" != "$integration_versions" ]; then
        echo "Shared dependency version mismatch for $dependency_name: workspace=$workspace_versions integration-tests=$integration_versions" >&2
        status=1
    fi
done <"$shared_names"

exit "$status"
