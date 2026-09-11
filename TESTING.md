# Testing

This file is the repository test-matrix index. Use the target-specific Cargo
aliases in `.cargo/config.toml`; a bare `cargo test --workspace` is not a valid
cross-adapter gate because the workspace contains incompatible native and WASM
targets.

## Required local gates

[CLAUDE.md](CLAUDE.md#ci-gates) is the single source of truth for the complete local and CI gate matrix. Run the target-matched checks for the files you change; CI runs the full supported operating-system and target matrix.

Run `./scripts/check-documentation.sh` to execute the documentation-specific
subset without rewriting tracked sources. Maintainers can run the same subset
from **Actions → Documentation checks → Run workflow**. That workflow is manual
and is not a required core or adapter check.

Every clippy alias denies warnings. The WASM checks require the targets listed
in `.tool-versions`; Fastly tests also require the pinned Viceroy version.

## End-to-end tests

`./scripts/integration-tests.sh` builds the Fastly WASM adapter and native Axum
adapter, provisions generated Viceroy configuration, builds the WordPress and
Next.js fixtures, and runs the ignored integration suite serially. Docker,
Viceroy, and `wasm32-wasip1` are prerequisites.

Pass test filters after the script name to narrow a run:

```bash
./scripts/integration-tests.sh test_wordpress_axum
./scripts/integration-tests.sh test_nextjs_fastly
```

## Focused runbooks

- [Public testing guide](docs/guide/testing.md)
- [Auction testing](docs/guide/auction-testing.md)
- [Integration-test crate](crates/trusted-server-integration-tests/README.md)

CI remains authoritative for the complete operating-system and target matrix.
