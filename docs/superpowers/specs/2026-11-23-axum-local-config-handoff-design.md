# Axum local configuration handoff

Issue: #1141. Base: #1646, which requires a writable local record directory.

## Contract

`ts config push --adapter axum --local` already writes the prepared local
config map consumed by Axum. `ts serve --adapter axum` already runs the Axum
command in `edgezero.toml` and inherits the caller's environment. The remaining
work is to make that existing contract explicit in help and operator guides,
and exercise syntactically malformed envelopes in the local smoke.

The supported sequence is: publish the app config, export
`EDGEZERO__ADAPTER__CONFIG_DIR`, export writable `EDGEZERO__ADAPTER__DATA_DIR`,
export the referenced `EDGEZERO__SECRETS__*` values, then run `ts serve
--adapter axum` in the same shell. The repository manifest launches
`cargo run -p trusted-server-adapter-axum`. Both launch commands consume the
same prepared stores. Serve neither pushes config nor loads dotenv files nor
generates secrets; configuration changes require restarting the process.

## Scope and alternatives

Document the existing explicit environment setup rather than introduce implicit
directory selection or secret generation. Automatic setup would need rules for
custom manifests and remote provider bindings and could obscure operator input.
Retaining only the direct Cargo command would leave the issue's CLI path unclear.
No EdgeZero changes, new runtime environment reads, identity behavior changes,
or changes to other adapters are needed. Legacy bridge variables remain poison
inputs in tests, not supported fallback inputs.

## Verification

Audit #1635 and #1646 separately. Run their existing Axum startup integration
tests and first-success smokes. The smoke's configured origin sentinel and URL
rewrite prove the pushed settings reach request handling. Startup integration
tests cover missing files and tampered envelopes; the smoke covers missing map
keys and individual secrets. Retain those controls. Add a malformed
inner envelope case that must exit nonzero with a parse diagnostic before any
HTTP response. The positive smoke must consume the pushed file unchanged.

Run the documented setup from a clean shell in a disposable copy with a local
fixture origin, through `ts serve --adapter axum`, and check publisher output.
Verify explicit prepared paths and secrets are inherited, not synthesized.
Run targeted formatting, CLI/Axum checks, the runtime environment guard and hosted
PR checks; do not run the full local gate list. Stop owned processes and remove
disposable files and worktrees after delivery.

## Review log

- Round 1: independent source review found no blocking concerns. Clarified which
  failure cases belong to integration tests and which belong to the smoke, and
  made the unchanged positive publication input explicit in the plan.
- Round 2: full revised design and plan review found no unresolved concerns.
