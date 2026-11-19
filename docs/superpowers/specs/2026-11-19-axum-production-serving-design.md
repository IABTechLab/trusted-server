# Axum production serving (A1) design

Issue: #1573 (plan ID A1, requirements AX-1, AX-2 and AX-7 of the
[server-side identity design](https://github.com/IABTechLab/trusted-server/blob/3314edbd7ae6f2db83daebeb96e37e1731c28ab0/docs/superpowers/specs/2026-11-11-server-side-identity-all-adapters-design.md)).

## Starting point

PR #1531 carries most of A1. Its branch is already merged forward through the
identity integration chain (#1506 to #1623), so the issue's first two tasks
are done on that branch:

- It contains #1506's head and pins EdgeZero by `rev = 9d02d7b3` on
  `rc/identity` (R1). It no longer pins an RC tag or carries its own cutover.
- `src/main.rs` starts Axum through EdgeZero's
  `run_production_app_with_initializer`, and `src/startup.rs` loads the config
  envelope and every required secret from the prepared stores before the
  listener opens.
- `cargo test-axum` runs the missing-secret and SIGTERM tests in CI.

This change is a follow-up PR stacked on #1531's branch
(`baseline/native-aws-20261110`). It finishes A1 without rewriting #1531.

## Requirement status

| Requirement                                                                            | State on #1531                                                                                                                                                            | This change                                                                                                                                             |
| -------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| AX-1 missing secret exits non-zero before the port opens                               | `missing_secret_fails_before_listen_without_legacy_fallback`; runs in CI                                                                                                  | The missing-secret diagnostic names the config path of the missing secret; the before-listen probe stops counting a Linux self-connection as a listener |
| AX-1 stop during a request completes it within the configured grace                    | `production_sigterm_drains_the_admitted_publisher_response` checks completion and the grace; runs in CI                                                                   | None                                                                                                                                                    |
| AX-1 stop completes a pending record write                                             | Not possible in A1: Axum has no record store until A2 (#1574), and draining after-response tasks is A3 (#1575) with its CI test in A8 (#1579)                             | Out of scope; the PR records the narrowing                                                                                                              |
| AX-2 startup through the AWS providers of IABTechLab/edgezero#414                      | `scripts/test-native-providers.mjs` (moto and the AppConfig agent in Docker); local only                                                                                  | Repairs the script's entry point and diagnostics; CI runs its unit tests                                                                                |
| AX-2 no runtime read of `TS_*`, `TRUSTED_SERVER_CONFIG_*` or `TRUSTED_SERVER_SECRET_*` | Met. The remaining names in tests and scripts are poison inputs that must be ignored. `TS_GIT_VERSION` is a constant set at build time from `TRUSTED_SERVER__GIT_VERSION` | Removes the legacy names from the getting-started guide                                                                                                 |
| AX-7 lockfile resolves EdgeZero to `rc/identity`                                       | Met (`9d02d7b3`)                                                                                                                                                          | None                                                                                                                                                    |

`TRUSTED_SERVER__` overrides are applied by `ts config push` when it builds
the envelope. The running server never reads them, and the native tests keep
treating them as poison. The CI guard for this is A9 (#1580).

## Changes

### Startup and serving

1. **Name the missing secret.** When a required app-config secret is absent,
   the startup error names the config field paths that reference it, never
   the key or the value. A key is an operator-supplied string, so a secret
   pasted into a reference field would otherwise be logged. Each
   missing-secret case in the Rust tests and `scripts/smoke-axum.sh` asserts
   its own path, that the other paths are not named and that no key appears.
2. **Read-only snapshot errors.** Replace `read_only_secret` and
   `read_only_config` with one helper whose name does not contain "secret".
   On #1528 CodeQL flagged the rotation rollback log as cleartext logging,
   with this helper as the source. The logged text is static. CodeQL runs only
   on PRs to main, so this stacked PR cannot show the alert gone.
3. **Visibility.** `NativeStoreSnapshot` becomes `pub(crate)`; only `app.rs`
   and `platform.rs` use it.
4. **Wording.** The 501 bodies and comments in `app.rs` still say "Axum dev
   server" and "read-only env vars"; they describe the native snapshots.

Discovery (`/.well-known/trusted-server.json`) keeps answering 500 without
JWKS. Cloudflare and Spin answer 500 in the same state, and
`discovery_route_status_parity` requires Axum to match them. A 501 would be a
deliberate cross-adapter divergence for no operator gain, since native startup
already rejects `request_signing.enabled`.

### DataDome payload

5. `build_protection_payload` computes `RequestInfo` when a request has an
   ingress record or a public origin. `RequestInfo::from_request` returns
   `Ok` whenever a public origin exists, so it fails only when the ingress
   has no origin and the `Host` header is not valid text or not a valid host.
   The fallback then reads the raw `Host` (or the URI authority when `Host`
   is not text) and the URI scheme, so an absolute-form request target sends
   a host and scheme that no other part of the request trusted.
   Send empty `Host` and `Protocol` in that case. Every adapter attaches
   `RequestIngress`, so this edge case changes on all four. It is a
   consistency fix: with a valid `Host` and no ingress origin the payload
   still carries that header, as today.

### Tests

6. `tests/startup.rs`: the before-listen probe uses the same self-connection
   rule as `native_acceptance.rs`. The missing-config test asserts the
   specific diagnostic, and the fixture poisons the legacy config and every
   legacy secret name with valid values, so a legacy fallback would make the
   server start and the test fail. Rename the
   `…_until_restart` test, which never restarts. Reword the `reserve_port`
   comments: the port is released, not held. Add `should …` messages to
   bare assertions.
7. `native_acceptance.rs`: the truncated-POST test also asserts that the
   origin saw exactly three `/action` requests. In both test files the short
   EC passphrase gets a distinctive value, and the no-secret-in-logs checks
   cover it and the proxy and admin values, also after a successful run.
8. Integration-test Axum environment: when Axum exits before readiness,
   attach a bounded tail of its output to the failure. EdgeZero's logger
   writes to stdout, so both streams are captured.
9. Drop the unused `temp-env` dev-dependency.

### Provider acceptance script

10. Use `import.meta.main`, so the script runs when invoked through a
    symlink instead of exiting 0 without running.
11. Loopback requests use `agent: false`.
12. Failure launches buffer the child's output and assert the expected cause
    and that no secret value appears. Nothing from that output is printed.
13. `docker run` gets `--dns 127.0.0.1`, so containers cannot resolve
    external names (IP egress is not blocked). `DOCKER_HOST` passes through
    to the Docker CLI when set. A Docker context is not passed: the script
    isolates `HOME`, where contexts live.
14. The `listening()` probe ignores a self-connection, as the Rust probes do.
15. CI runs `node --test 'scripts/tests/*.test.mjs'` in `format.yml`, and the
    CLAUDE.md gate list names it.

### Docs

16. `getting-started.md` replaces the legacy environment variables and the
    naming-convention table with `ts config push --local`,
    `EDGEZERO__ADAPTER__CONFIG_DIR` and `EDGEZERO__SECRETS__*`.
    The current text exports names the server ignores, so following it fails
    startup.
17. `axum-dev.md`, `api-reference.md`, the crate README and its `Cargo.toml`
    description stop calling Axum a development-only target. Identity storage
    on Axum arrives with A2 (#1574).
18. `native-rc-demo.md` is rewritten for operators (no per-unit notes), gets
    a sidebar entry and a "Run locally" block.

## Preservation

| Scenario                           | Must stay true                                       | Evidence                                             |
| ---------------------------------- | ---------------------------------------------------- | ---------------------------------------------------- |
| Valid prepared inputs              | Starts, serves health, TSJS and rewritten pages      | Existing startup and acceptance tests                |
| Any missing input                  | Exits non-zero and never listens                     | Startup tests with the corrected probe               |
| Legacy env names present           | Ignored                                              | Poison inputs, now including a valid legacy envelope |
| DataDome with a valid `Host`       | Payload unchanged                                    | Existing tests plus a new control                    |
| DataDome with valid ingress origin | Payload uses the checked host and scheme             | Existing tests                                       |
| Fastly, Cloudflare, Spin           | Only the DataDome invalid-`Host` edge case changes   | Adapter test aliases, parity                         |
| Discovery on every adapter         | Same status on Axum, Cloudflare and Spin             | `discovery_route_status_parity`                      |
| `/verify-signature`                | Still the body-limit probe in `native_acceptance.rs` | Existing test                                        |
| Secrets never logged               | Unchanged                                            | Log assertions, extended                             |

## Out of scope

- Pending record writes on SIGTERM (A3, A8) and Axum record stores (A2).
- A CI job that runs the Docker provider acceptance. The script runs locally;
  a hosted job would pull third-party images on every PR.
- The `TRUSTED_SERVER__` runtime-name guard (A9).
- EdgeZero work: pre-dispatch and head-limit responses that skip finalize, and
  the EdgeZero side of the remaining review asks.
- RustSec advisories already on the stack base.

## Review log

- **Round 1 (independent agent review of spec and plan).** Six blocking
  findings, all accepted:
  - The DataDome red test as first written could not fail:
    `RequestInfo::from_request` succeeds whenever a public origin exists.
    The condition was restated as an ingress without an origin, an invalid
    `Host` and an absolute-form target.
  - The DataDome change reaches every adapter, because all four attach
    `RequestIngress`. The preservation table had said it did not.
  - A 501 for discovery would break `discovery_route_status_parity`; the
    change was dropped (see Startup and serving).
  - `node --test scripts/tests/` fails on Node 24; the job uses a glob.
  - EdgeZero's logger writes to stdout, so the integration environment
    captures both streams.
  - `smoke-axum.sh` needed per-key missing-secret cases.

  Optional findings were also taken:
  - Assert that the other keys are not named.
  - Drop `DOCKER_CONTEXT` and state the `--dns` limit.
  - Ignore self-connections in the Node probe.
  - Poison every legacy name with valid values.
  - Check the short passphrase in both test files.
  - Make the docs consistent.
  - Pin the design link to a commit.

- **Round 2 (same reviewer, re-check).** No blocking items remain. A
  wording fix for the DataDome fallback description was applied.
- **Round 3 (independent review of the implementation).** No blocking
  findings. Three optional ones were taken: name config paths instead of
  reference keys in the missing-secret diagnostic (a key is an
  operator-supplied string and could hold a misplaced secret); update the
  Axum section of the architecture guide; attach the output tail when the
  integration environment's readiness deadline expires.
