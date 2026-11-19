# Runtime environment name guard

## Goal and scope

Issue #1580 adds a CI barrier against reintroducing legacy process-environment
configuration into serving code. Stack on #1635 (`feat/1573-axum-production-serving`),
which includes #1531. Keep #1575 independent: do not change Axum startup,
shutdown, or after-response wiring.

At base `6949eea868`, serving source has seven production `std::env::var`
reads: six in Fastly platform/middleware/app code and one in core DataDome.
They read four Fastly metadata names; none reads a legacy name. Four other
serving-source reads are test-only, including the `vars_os` call in
`Settings::from_toml_and_env`, which is `#[cfg(test)]`.

Count compile-time inputs separately: `TS_GIT_VERSION` uses
`option_env!("TRUSTED_SERVER__GIT_VERSION")`, and `config_payload.rs` uses
`env!("TRUSTED_SERVER_DEFAULT_CONFIG_STORE_ID")`, emitted by core `build.rs`.
The identifier `TS_GIT_VERSION` is not an environment name.

## Decisions

The approved scope is runtime **process-environment** reads in serving crates.
Compile-time `env!` / `option_env!`, build scripts, operator CLI/codegen, tests,
fixtures, and scripts are exempt. Deliberate legacy poison inputs stay intact.
Cloudflare's `worker::Env::var("TRUSTED_SERVER_CONFIG")` reads a Worker binding,
not the process environment, and remains outside this gate.

Three `EnvConfig::from_env()` calls in `settings_data.rs` delegate selectors to
EdgeZero. `EDGEZERO__*` loading and provider preparation belong to EdgeZero;
this change does not implement or lint dependency internals.

Allow `TRUSTED_SERVER__` names and four explicit platform exceptions:

- `FASTLY_IS_STAGING`: Fastly staging metadata used by response tagging and
  DataDome's existing staging protection-test behavior.
- `FASTLY_SERVICE_VERSION`: Fastly deployment version exposed in response headers.
- `FASTLY_HOSTNAME`: Fastly server metadata used by the platform abstraction.
- `FASTLY_REGION`: Fastly server region used by the platform abstraction.

Each exception has a justification beside its definition. Additions require a
reviewed reason, following the domain allowlist policy; no wildcard exception
for platform names. Moving DataDome behind config is outside this XS change.

## Mechanism

Follow `scripts/lint-wasm-clock.sh`: use a dedicated Clippy configuration and a
small shell runner. A new `ts dev lint` parser would add dependencies and still
need name resolution and macro expansion. A text scan cannot reliably handle
aliases. Clippy resolves aliases, imports, reexports, function values, and local
or external macro expansions.

Disallow direct `std::env::{var,var_os,vars,vars_os}` access in serving targets.
Include known process-environment alternatives such as libc getenv and the
`config::Environment` loader. Flag the read site regardless of the argument;
computed names and environment-wide scans cannot evade checking.

Route the seven existing reads through a core `runtime_env!` macro. Its key is
a compile-time `&str` constant expression. Validation in the named key constant accepts
only the approved prefix or four justified exceptions. The macro permits just
its own `std::env::var` expression and returns the unchanged `Result`.
Constant identifiers remain usable; runtime-built arguments cannot compile.

The runner selects production library/binary targets across native Axum,
Fastly WASI (both defaults and reusable sandbox), Cloudflare WASM and Spin WASI.
Explicitly select shared serving packages so Clippy checks their source as
well as adapter source in each applicable target configuration. Omit test,
example and benchmark targets. Rust removes test-only items, including nested
`#[cfg(test)]` methods. Build scripts receive explicit scoped exemptions
because Cargo also checks them during production Clippy runs.

Wire the guard and its fixture tests into the existing Rust format/lint job.
Document the command in AGENTS.md's CI Gates list. No runtime config, consent,
privacy, provider, or deployment behavior changes.

## Verification

Use a dependency-free shell fixture harness invoking the selected toolchain's
Clippy driver. Establish compiler failures before adding enforcement. Assert
nonzero exits and the relevant lint/const diagnostic, rather than accepting
arbitrary compilation errors.

Negative fixtures cover both legacy prefixes, `TS_*`, another foreign name,
module/function aliases, reexports, fully qualified paths, function pointers,
local and external macros, `var_os`, `vars`, `vars_os`, computed names, and
forbidden constants passed to the checked macro. Positive fixtures cover the
approved prefix, each platform exception, constants, compile-time names,
`#[cfg(test)]` helpers and modules, and deliberate poison inputs in test code.

Run the guard against the actual tree. Run only targeted tests, formatting and
linting for changed code; the full CI list is explicitly deferred to hosted CI.
On a disposable copy, inject a legacy read and show the actual guard rejects
it. Build and start the real branch's Axum binary through the local task setup,
verify startup and an application response, and stop owned processes. Never
edit `trusted-server.toml` or `.env`; back up and restore any necessary local
launcher/override edits. Preserve private inputs outside tracked files.

## Limits

This is compiler enforcement for selected production target/feature sets,
not an audit of arbitrary dependency internals or inactive future features.
Like existing Clippy gates, deliberate lint suppressions require code review.
A future environment-reading dependency/API needs an explicit scope decision.

## Review log

- Round 1: independently checked source inventory, exclusions, target selection,
  and external macro enforcement. No blocking or optional findings.
- Round 2: independently checked source claims, shared package coverage, build
  exemptions, local/external macros and function references. No blocking or
  optional findings.

- Round 3: implementation review found that an all-features Fastly check
  excludes existing default-only production branches. Added a separate default
  pass and conditional fixtures; a disposable-copy mutation verifies the gap.
