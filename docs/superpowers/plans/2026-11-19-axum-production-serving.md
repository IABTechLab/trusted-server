# Axum production serving (A1) implementation plan

Spec: [Axum production serving design](../specs/2026-11-19-axum-production-serving-design.md).
Branch: `feat/1573-axum-production-serving`, stacked on #1531
(`baseline/native-aws-20261110`).

Each task runs `cargo test-axum` (and the listed extra check) before the next.

## Task 1: Name the missing secret (spec 1)

- `src/startup.rs`: the missing-value and failed-read messages name the
  reference key (an envelope reference name, never a value).
- `tests/native_acceptance.rs` (missing-input loop), `tests/startup.rs` and
  `scripts/smoke-axum.sh`: each case asserts its own key and that the other
  keys are absent. Red first.

## Task 2: Snapshot helper, visibility, wording (spec 2 to 4)

- One `read_only_snapshot(PlatformError)` helper replaces both functions.
- `NativeStoreSnapshot` and `empty()` become `pub(crate)`.
- `src/app.rs` 501 bodies and comments describe the native snapshots.

## Task 3: DataDome fallback (spec 5)

- `crates/trusted-server-core/src/integrations/datadome/protection.rs`: when
  the payload tried `RequestInfo` (ingress or public origin present) and it
  failed, `Host` and `Protocol` are empty.
- Red test: `RequestIngress` without an origin, absolute-form URI
  `https://target.example.com/…`, non-text `Host` header. The old code sends
  `target.example.com`/`https`; the new code sends empty values.
- Control test: the same ingress with a valid `Host` keeps that host.
- Check: `cargo test-fastly`, `cargo test-cloudflare`, `cargo test-spin`, parity.

## Task 4: Startup test corrections (spec 6, 9)

- `tests/startup.rs`: self-connection-aware probe; missing-config test with
  the exact local-provider diagnostic (read it from a real run first) and a
  fixture that poisons every legacy config and secret name with valid values;
  rename `…_until_restart` to `…_while_running`; reword `reserve_port`
  comments here and in `native_acceptance.rs`; `should …` messages on bare
  asserts in touched tests.
- Remove `temp-env` from Axum dev-dependencies.

## Task 5: Acceptance test strengthening (spec 7)

- Truncated-POST test asserts the origin's total `/action` count is 3.
- Both test files: a distinctive short passphrase; redaction checks cover it
  and the proxy/admin values, including after a successful run.

## Task 6: Integration environment diagnostics (spec 8)

- `crates/trusted-server-integration-tests/tests/environments/axum.rs`: pipe
  stdout and stderr into a bounded shared tail (for example the last 40
  lines); attach it when the process exits before readiness.
- Check: parity test and clippy for the integration-tests crate.

## Task 7: Provider acceptance script (spec 10 to 15)

- `import.meta.main` entry; drop `pathToFileURL`.
- `agent: false` on loopback requests.
- Failure launches: bounded buffer of both streams at `info`; assert cause and
  absence of every secret value; never print it.
- `--dns 127.0.0.1` on `docker run`; export a `dockerEnvironment(work, env)`
  helper that passes `DOCKER_HOST` when set; unit test it.
- `listening()` ignores a self-connection.
- `format.yml` job: `node --test 'scripts/tests/*.test.mjs'`; add it to the
  CLAUDE.md gate list.
- Check: unit tests plus the full Docker run against the release binary.

## Task 8: Docs (spec 16 to 18)

- `getting-started.md` Option B uses `EDGEZERO__ADAPTER__CONFIG_DIR` and
  `EDGEZERO__SECRETS__*`; remove the legacy table.
- `axum-dev.md`, `api-reference.md`, crate README and `Cargo.toml`
  description: production startup supported; identity storage with A2.
- `native-rc-demo.md`: operator rewrite, sidebar entry, "Run locally".
- Check: docs format and the markdown prettier check.

## Verification before PR

- CI gates from `CLAUDE.md`: fmt, all eight clippy aliases, all adapter test
  aliases, parity, vitest, JS and docs format, the markdown prettier check,
  native build-digest, wasm clock lint, docs-proposal scripts, the domain
  lint on changed lines and the Node pin guard.
- `./scripts/smoke-axum.sh`.
- `scripts/test-native-providers.mjs` with Docker.
- Runtime verification through the publisher proxy against the Axum
  production binary.
