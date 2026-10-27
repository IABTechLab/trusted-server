# Strict integration validation implementation plan

**Goal:** Refuse silently ignored configuration typos while retaining runtime
startup for unknown integration IDs.

**Architecture:** Use the existing complete ID inventory in deploy/runtime
validation and serde strictness for known table fields. Preserve open payload
maps, disabled-block semantics, and the current configuration schema.

**Tech stack:** Rust, serde, error-stack, validator, TOML, existing warning capture.

## 1. Regression evidence

Files: `crates/trusted-server-core/src/config.rs`,
`crates/trusted-server-core/src/settings.rs`, CLI config tests where appropriate.

- [x] Add deploy rejection tests for unknown IDs and enabled/disabled unknown fields.
- [x] Assert the ordinary CLI-facing error text names the offending key.
- [x] Add runtime warning capture and successful validation for unknown IDs.
- [x] Add strict tester-cookie/Tinybird regressions and open-map preservation tests.
- [x] Run focused tests against unchanged production code and retain failures.

Run: `cargo +1.98.1 test -p trusted-server-core --lib --locked -- config::tests`
and focused settings tests. New regressions should fail for the expected cause.

## 2. Minimal implementation

Files: `config.rs`, `settings.rs`, and config structs in integration modules
`adserver_mock`, `didomi`, `google_tag_manager`, `gpt`, `js_asset_proxy`, `lockr`,
`nextjs/mod`, `permutive`, `sourcepoint`, and `testlight`.

- [x] Promote the known-ID inventory; reject unknown map keys during deploy validation.
- [x] Warn for unknown map keys during runtime validation without rejecting startup.
- [x] Add `#[serde(deny_unknown_fields)]` to the twelve identified config structs.
- [x] Include serde errors in enabled and disabled `get_typed` outer contexts.
- [x] Rerun regressions and native core suite; preserve minimal disabled blocks,
      aliases, nested schemas, and open parameter maps.

## 3. Documentation and entry points

Files: `docs/guide/configuration.md`, `docs/guide/integrations/lockr.md`,
`docs/guide/error-reference.md`, `CHANGELOG.md`.

- [x] Explain strict known-field validation and runtime unknown-ID warnings.
- [x] Replace unsupported Lockr fields and custom-integration example.
- [x] Document validate, clean up, push, deploy rollout order and startup risk.
- [x] Exercise CLI config validation with fictional invalid configuration fixtures.

## 4. Verification and review

- [x] Run formatting and target-matched checks using repository-pinned tools.
- [x] Run applicable CI gates from `AGENTS.md`; record blockers explicitly.
- [x] Request independent local review of the combined changes and resolve findings.
- [x] Inspect final diff and preserve unrelated checkout changes.
- [x] Report tested base/content, passing evidence, blocked gates and retained worktree.
