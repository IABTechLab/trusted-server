# EdgeZero RC431 validation record

Validation date: 2026-10-20. This records local checks, not a deployment or a
production certification. See [the operator guide](./native-rc-demo.md) for inputs
and launch commands.

## Inputs

| Input                                         | Revision                                                                                 |
| --------------------------------------------- | ---------------------------------------------------------------------------------------- |
| TS main base                                  | `6ef07ae52bec3e695ec57fe090243b1a281c314f`                                               |
| Reviewed implementation                       | `067d0fe1bfd32b57acf64d5e09a3f545e61e6bc7`                                               |
| Final Rust and image input commit             | `4a486a0d11525c2466e37b393634b1653ac2c802`                                               |
| Cloudflare smoke HOME fix and regression test | `ec735a24fcbb71ed8a46f04f5a3a58ba50f82d43`                                               |
| All six EdgeZero declarations                 | `https://github.com/IABTechLab/edgezero.git`, `61bfe3b931fd97f5300287e4a91e33f32aa0fbee` |
| ESI dependency-only patch                     | `https://github.com/stackpop/esi`, `75bfbb396d4cb7792610f1287cbec61675f13f38`            |

Later smoke, CI and documentation changes do not alter the image's copied inputs or
Rust sources. Checks were reused when their relevant inputs had not changed.
`cargo metadata --locked --format-version 1` confirms one EdgeZero Git source
across nine resolved EdgeZero packages. Normal native, Fastly, Cloudflare and Spin
graphs contain no `aws-sdk-*` packages. Optional native provider features were
checked separately and together.

Tools used: Rust 1.98.1, Node 24.12.0, Fastly CLI 16.0.0, Viceroy 0.21.0,
Spin 4.1.0, worker-build 0.8.5, wasm-bindgen 0.2.126 and Wrangler 4.151.3.
Spin's emitted component imports and exports `wasi:http@0.3.0`.

PRs #1410 through #1412 and the #777 module/configuration/permission design are
not part of this branch. It does not use #1228 as its base.

## Rust and static checks

All commands below passed. Counts exclude ignored cases.

| Command, from repository root unless stated                                                                                                                   | Result                                                                           |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| `cargo test -p trusted-server-core --lib`                                                                                                                     | 2,990 passed                                                                     |
| `cargo test-axum`                                                                                                                                             | Native unit, HTTP, lifecycle, routing, startup and subprocess suites passed      |
| `cargo test-cloudflare`                                                                                                                                       | 55 passed                                                                        |
| `cargo test-spin`                                                                                                                                             | 99 passed                                                                        |
| `mise exec github:fastly/viceroy@0.21.0 -- cargo test-fastly`                                                                                                 | 222 adapter tests, 2,984 shared WASM tests and integration/doctest checks passed |
| `mise exec github:fastly/viceroy@0.21.0 -- cargo test-fastly-reuse`                                                                                           | 222 passed                                                                       |
| `cargo test -p trusted-server-cli --target x86_64-unknown-linux-gnu`                                                                                          | 890 passed; 29 remain ignored                                                    |
| `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`                                                                 | 17 passed                                                                        |
| `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test integration --no-run`                                                   | Compiled; this is not an end-to-end runtime pass                                 |
| `cargo test -p trusted-server-core --test template_build_digest`                                                                                              | 10 passed                                                                        |
| `cargo clippy -p trusted-server-core --test template_build_digest -- -D warnings`                                                                             | Passed                                                                           |
| `cargo check-axum`, `check-fastly`, `check-cloudflare`, `check-spin`                                                                                          | Passed                                                                           |
| `cargo clippy-fastly`, `clippy-axum`, `clippy-cloudflare`, `clippy-cloudflare-wasm`, `clippy-spin-native`, `clippy-spin-wasm`, `clippy-cli`, `clippy-codegen` | All aliases passed                                                               |
| `cargo fmt --all --check`                                                                                                                                     | Passed                                                                           |
| `cargo doc -p trusted-server-core --no-deps --all-features`                                                                                                   | Passed with 30 existing rustdoc warnings                                         |
| `cargo check -p trusted-server-adapter-axum --features aws-appconfig-agent`                                                                                   | Passed                                                                           |
| `cargo check -p trusted-server-adapter-axum --features aws-secrets-manager`                                                                                   | Passed                                                                           |
| `cargo build -p trusted-server-adapter-axum --features aws-appconfig-agent,aws-secrets-manager --bin trusted-server-axum`                                     | Passed; this binary ran the provider acceptance below                            |
| `node --test scripts/tests/*.test.mjs`                                                                                                                        | Six Linux helper tests passed, including CLI-build HOME isolation                |
| `mise exec shellcheck@0.11.0 -- shellcheck scripts/smoke-{common,axum,fastly,cloudflare,spin}.sh`                                                             | Passed                                                                           |
| `ts dev lint domains --changed-vs 6ef07ae52bec3e695ec57fe090243b1a281c314f`                                                                                   | Passed on the committed changes                                                  |
| `bash scripts/lint-wasm-clock.sh`                                                                                                                             | Passed                                                                           |
| In `crates/trusted-server-js/lib`: `npm run build`, `npm test`, `npm run lint`, `npm run format`                                                              | Passed; 1,211 JS tests. JS library inputs are unchanged since that run           |
| In `docs`: `npm run format`, `npm run lint`, `npm run build`; then `bash scripts/docs-proposal/test.sh` from root                                             | Passed; 20 proposal-script tests                                                 |

The strict probe `RUSTDOCFLAGS="-D warnings" cargo doc -p trusted-server-core
--no-deps --all-features` failed on broken/private documentation links. The same
probe on the untouched TS base reproduced those diagnostics. No strict rustdoc
pass is claimed. Ordinary suite skips remain skips; only the real Next.js case
below was explicitly rerun with `--ignored`.

## Actual local runtime checks

### Prepared native startup, ingress and HTTP

The native suites launched the real binary with owned synthetic configuration.
They checked valid envelopes, missing/tampered configuration, missing secrets,
invalid runtime settings, no legacy environment rescue, restart-only adoption,
request-local client/public-URL metadata and an absolute-target spoof attempt.
The 15 native HTTP tests exercised Host override, duplicate cookies, decoding,
byte limits, deadlines, pending-handle selection, late append and cancellation.
Buffered JSON endpoint probes also passed without making publisher bodies eager.

The real Next.js 14.2.35 fixture was prewarmed on loopback, then run with:

```sh
NATIVE_NEXTJS_ORIGIN="$LOCAL_NEXTJS_ORIGIN" cargo test \
  -p trusted-server-adapter-axum --test native_acceptance \
  real_nextjs_action_and_api_after_truncated_post -- --ignored --exact --nocapture
```

This passed a truncated POST, a subsequent real server action returning 303,
and a subsequent real API response. The fixture retained its explicit
`publisher.example.com` server-action origin allowance. An unknown-action 404
was not accepted as success.

### Independent native providers

```sh
node scripts/test-native-providers.mjs target/debug/trusted-server-axum
```

AppConfig Agent local-development mode and Moto Secrets Manager ran separately
from TS. Agent-only, Secrets-Manager-only and combined selection passed real
TS health, authentication and embedded-TSJS checks. They also passed immutable
snapshot checks, restart adoption, whole secret-string handling, and selected
provider failures before listen without local/environment fallback.

This fixture did not publish a CLI export into the Agent. CLI-generated envelope
acceptance was checked separately through the native first-success smoke. It did
not test DynamoDB, real AWS credentials, IAM or AWS account services.

### Fastly, Spin, Cloudflare and native launchers

```sh
mise exec github:fastly/cli@16.0.0 github:fastly/viceroy@0.21.0 -- bash scripts/smoke-fastly.sh
mise exec spin-framework@4.1.0 -- bash scripts/smoke-spin.sh
bash scripts/smoke-axum.sh
# With the exact worker-build/wasm-bindgen versions above on PATH:
(cd crates/trusted-server-adapter-cloudflare && worker-build --release -- --locked)
bash scripts/smoke-cloudflare.sh
```

All passed. Fastly and the real local Worker passed publisher GET rewriting,
nonempty POST echo and two separate Set-Cookie fields. The Worker also checked
four observable missing-config/secret failures. Its rebuilt bundle's 289 input
hashes were verified after the run. A further full smoke started without a CLI,
`TS_BIN`, `RUSTUP_HOME` or `CARGO_HOME`; it built the CLI using the caller's homes
before isolating Wrangler and passed every runtime assertion.

Spin ran its SDK7/P3 component in Spin 4.1.0. Separate Fastly closed-delivery tests
covered Application versus fallback completion, source failure, refusal and
exactly-once observation. Raw Fastly health, JA4 and optional sandbox metrics
remain the documented SDK-owned exceptions.

### Native image

```sh
docker build \
  --build-arg TRUSTED_SERVER__GIT_VERSION=4a486a0d11525c2466e37b393634b1653ac2c802 \
  -t ts-edgezero431-local:4a486a0d .
node scripts/test-native-container.mjs ts-edgezero431-local:4a486a0d
```

Both passed. The local linux/amd64 image ID was
`sha256:57601dad86d181379b09a497767eb1b5925e4f859136351327637663f019d4c1`,
42,750,041 bytes, configured as UID/GID `10001:10001`. Runtime checks proved
non-loopback published-port access, invalid-input rejection before listen, page
rewriting, embedded TSJS, CA roots, no Node/Rust toolchain, a read-only root with
explicit writable mounts, and a normal in-flight SIGTERM drain in 1,346 ms.
No image was published. The new image workflow only builds and tests locally.

## Failures found and resolved

- The core environment test also failed on untouched main because the build's
  `TRUSTED_SERVER__GIT_VERSION` is not a Settings field. The test-only environment
  reader now excludes that build metadata key.
- Viceroy 0.17 lacked the SDK0.13 cache ABI. Viceroy 0.21 passed it.
- Spin's lazy empty upload reached a buffered-only SDK converter. Bounded owned
  upload collection fixed it. Review then found the same rejection in Fastly
  and Cloudflare. Both now use that shared collector, with source-error and
  over-limit tests plus actual GET/POST checks.
- Fastly's closed store preparation opened the unused `trusted_server_kv`
  declaration and failed. Removing that declaration fixed the real smoke;
  metadata tests were updated together. Dedicated Fastly EC storage is unchanged.
- Worker errors were silent because `log` had no backend. The private Info-level
  console backend made all four negative smoke diagnostics observable.
- Review found that Wrangler's isolated HOME also redirected the CLI-build
  fallback's rustup/Cargo homes. CLI resolution now precedes isolation. A
  regression test and an actual fresh-CLI full smoke passed with default homes.
- Iteration also caught compile, Clippy, formatting, stale metadata assertions
  and fixture/setup failures. Final checks above passed after the relevant fixes.

Initial correctness, architecture, code-smell and style reviews inspected the
first implementation. A source-only correctness follow-up inspected the full
runtime-fix delta. The coordinator verified and fixed C1, C2 and C3 and the Spin
wildcard import. The three nonblocking duplication suggestions remain deferred.
Reviews were not counted as executed runtime evidence.

## Remaining limits

No AWS provisioning/deployment, registry publication, hosted provider deployment,
remote CI certification, new EC graph or native signing backend was tested or
added. The minimal native profile rejects enabled signing/dynamic extra config
stores before listen. Provider acceptance used the host feature-enabled binary;
the tested image used default features. No arm64 image or external HTTPS-origin
round trip was tested. CA-root presence alone is not a TLS handshake test.
The full browser/vendor/container integration matrix was not run.

Coordinator logs, input fingerprints and exit codes are in `/tmp/ts431-checks`.
The Cloudflare worker retained its exact tool, bundle and input hashes separately.
Those local logs are not hosted CI results and may disappear after cleanup.
