#!/bin/sh
# Rust release tooling must be source-approved before this launcher can execute it.
# Keep release execution closed while the replacement is implemented and reviewed.
set -eu
# An extensionless pin overrides rust-toolchain.toml during rustup selection,
# so reject it in the invocation directory and in the repository root that
# owns this script before any future toolchain selection can happen.
script_directory=$(dirname -- "$0")
repository_root=$(CDPATH='' cd -P -- "$script_directory/../.." && pwd -P)
for candidate in rust-toolchain "$repository_root/rust-toolchain"; do
    if [ -e "$candidate" ] || [ -L "$candidate" ]; then
        printf '%s\n' 'Extensionless rust-toolchain is rejected before toolchain selection' >&2
        exit 1
    fi
done
printf '%s\n' 'Release bootstrap is pending the reviewed Rust tooling replacement' >&2
exit 1
