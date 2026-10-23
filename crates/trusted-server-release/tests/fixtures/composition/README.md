# Cargo observation fixtures

The Cargo 1.95 metadata and JSONL fixtures are actual Cargo outputs. Absolute
checkout and target roots are relocated to `/build/source` and `/build/target`;
other observed fields are preserved. These are parser regressions, not release
qualification evidence.

The `cargo-1.95-cross-*` fixture uses resolver 2 and three fictional packages:

- `example-app` has a build script that calls its build dependency,
  `example-host-helper`.
- `example-host-helper` depends on `example-host-unix` under
  `[target.'cfg(unix)'.dependencies]`.
- Both helper packages are excluded from workspace membership but remain path
  dependencies. All functions are empty except the calls above.

The commands are `cargo metadata --format-version 1 --frozen --offline`, the
same command with `--filter-platform wasm32-wasip1`, and
`cargo build --release --target wasm32-wasip1 --frozen --offline --message-format json`.
The cross-build runs on a Unix host. Cargo compiles `example-host-unix` for that
host, but the target-filtered metadata omits its identity. The unfiltered
catalog preserves the identity needed for an exact join. Metadata membership
alone never marks a package as a contributor.

The original `cargo-1.95-metadata.json` and `cargo-1.95-messages.jsonl` use the
`cargo-denial` fixture's `control` package, with a host build script and a target
Wasm executable.
