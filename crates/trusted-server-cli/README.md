# trusted-server-cli

Host-target operator CLI for Trusted Server. The installed binary is `ts`.

The CLI validates and publishes application configuration through EdgeZero,
delegates platform lifecycle commands, audits public pages and ad-template
configuration, and builds external Prebid artifacts. It is not part of any
adapter WASM artifact. Most commands support Linux and macOS; the production-
hostname development proxy and local CA commands are macOS-only.

Install and test from the repository root:

```bash
cargo install-cli
./scripts/test-cli.sh
```

The test script selects and, when necessary, installs the host Rust target,
runs the CLI suite, and executes the ignored browser-backed audit fixtures
serially. See the [CLI guide](../../docs/guide/cli.md) for the generated
two-platform command inventory and the
[EdgeZero guide](../../docs/guide/edgezero.md) for configuration lifecycle and
store semantics.
