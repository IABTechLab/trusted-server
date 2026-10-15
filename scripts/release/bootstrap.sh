#!/bin/sh
# Rust release tooling must be source-approved before this launcher can execute it.
# Keep release execution closed while the replacement is implemented and reviewed.
set -eu
# An extensionless pin overrides rust-toolchain.toml during rustup selection,
# so reject it before any future toolchain selection can happen.
if [ -e rust-toolchain ] || [ -L rust-toolchain ]; then
    printf '%s\n' 'Extensionless rust-toolchain is rejected before toolchain selection' >&2
    exit 1
fi
printf '%s\n' 'Release bootstrap is pending the reviewed Rust tooling replacement' >&2
exit 1
