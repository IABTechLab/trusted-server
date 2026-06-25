#!/usr/bin/env bash
#
# Ensure the excluded trusted-server-integration-tests crate resolves the same
# shared dependency versions as the workspace, so the integration tests exercise
# the same dependency versions the production build ships.
#
# Two checks run:
#   1. Direct shared dependencies must resolve to identical versions (via
#      `cargo tree --depth 1`).
#   2. Transitive parity: every (name, version) the workspace lockfile resolves
#      must also be present in the integration lockfile for any crate the two
#      share, except a documented allowlist of crates the integration crate's
#      own dependency tree forces to a different version (and which therefore
#      cannot be aligned). This catches accidental drift when the integration
#      lockfile is regenerated and silently bumps shared crates to newer
#      versions than production uses.
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
    --manifest-path "$REPO_ROOT/crates/trusted-server-integration-tests/Cargo.toml" \
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
        echo "Shared dependency version mismatch for $dependency_name: workspace=$workspace_versions trusted-server-integration-tests=$integration_versions" >&2
        status=1
    fi
done <"$shared_names"

# -----------------------------------------------------------------------------
# Transitive parity check (lockfile-based)
# -----------------------------------------------------------------------------
#
# Crates that the integration crate's own dependency tree pins to a different
# version than the workspace, and which therefore cannot be aligned. Each is a
# `name=reason` entry so the exemption stays self-documenting. Keep this list as
# small as possible: prefer pinning the integration lockfile to the workspace
# version (`cargo update -p <crate> --precise <version>`) over adding an entry.
transitive_parity_allowlist=(
    # Forced newer by the integration crate's wasm-bindgen-based test deps
    # (reqwest's wasm tooling); the workspace stays on an older wasm-bindgen.
    "js-sys"
    "wasm-bindgen"
    "wasm-bindgen-macro"
    "wasm-bindgen-macro-support"
    "wasm-bindgen-shared"
    # Forced newer by an integration-only dependency.
    "num-conv"
    # The workspace pins an older 0.10.x via a production-only dependency; the
    # integration tree only needs 0.13/0.14, so the 0.10 line is never resolved.
    "itertools"
)

is_allowlisted() {
    local name="$1" entry
    for entry in "${transitive_parity_allowlist[@]}"; do
        [ "$name" = "$entry" ] && return 0
    done
    return 1
}

# Print "name<TAB>version" for every [[package]] entry in a Cargo.lock.
extract_lock_packages() {
    awk '
        /^name = / { gsub(/"/, "", $3); pkg_name = $3 }
        /^version = / { gsub(/"/, "", $3); print pkg_name "\t" $3 }
    ' "$1" | sort -u
}

workspace_lock_packages="$(mktemp)"
integration_lock_packages="$(mktemp)"
integration_lock_names="$(mktemp)"
trap 'rm -f "$workspace_dependencies" "$integration_dependencies" "$workspace_names" "$integration_names" "$shared_names" "$workspace_lock_packages" "$integration_lock_packages" "$integration_lock_names"' EXIT

extract_lock_packages "$REPO_ROOT/Cargo.lock" >"$workspace_lock_packages"
extract_lock_packages "$REPO_ROOT/crates/trusted-server-integration-tests/Cargo.lock" \
    >"$integration_lock_packages"
cut -f1 "$integration_lock_packages" | sort -u >"$integration_lock_names"

while IFS="$(printf '\t')" read -r name version; do
    # Only consider crates the two lockfiles share.
    grep -qxF "$name" "$integration_lock_names" || continue
    is_allowlisted "$name" && continue
    if ! grep -qxF "$(printf '%s\t%s' "$name" "$version")" "$integration_lock_packages"; then
        integration_versions="$(
            awk -F "$(printf '\t')" -v dep="$name" '$1 == dep { print $2 }' \
                "$integration_lock_packages" | sort -u | paste -sd ',' -
        )"
        echo "Transitive dependency drift for $name: workspace resolves $version but trusted-server-integration-tests has [$integration_versions]" >&2
        echo "  Fix: cargo update --manifest-path crates/trusted-server-integration-tests/Cargo.toml -p $name --precise $version" >&2
        echo "  (or, if the integration tree genuinely requires a different version, add $name to transitive_parity_allowlist in $0)" >&2
        status=1
    fi
done <"$workspace_lock_packages"

exit "$status"
