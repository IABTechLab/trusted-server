# Runtime environment guard implementation plan

**Goal:** prevent serving code from reading unapproved process-environment names.

**Architecture:** dedicated Clippy configuration and shell runner, following the
wasm clock guard. A compile-time checked core macro preserves approved reads.

**Tech stack:** Rust/Clippy 1.98.1, Bash, existing adapter build targets.

## Files

- Create `scripts/clippy-runtime-env/clippy.toml`, `scripts/lint-runtime-env.sh`
  and `scripts/test-runtime-env.sh` for enforcement and compiler fixtures.
- Create `crates/trusted-server-core/src/runtime_env.rs`; register it in `lib.rs`.
- Mechanically replace seven reads in Fastly `src/platform.rs`,
  `src/middleware.rs`, `src/app.rs`, and core `src/integrations/datadome.rs`.
- Exempt core and JS `build.rs` from this runtime-only rule.
- Add the gate/test steps to `.github/workflows/format.yml` and AGENTS.md.
- Do not change Axum startup/shutdown/after-response code or poison inputs.

## Steps

- [ ] Write compiler fixtures first. Run negative direct-read cases without
      the policy and confirm they wrongly compile (red), using a valid Rust
      positive control to establish fixture reach.
- [ ] Add dedicated disallowed methods/types configuration. Rerun fixtures;
      require the expected lint diagnostic for each forbidden read. Include
      aliases, reexports, local/external macros, function values, both scans,
      other foreign names, dynamic keys and known equivalent APIs.
- [ ] Add checked-macro fixtures first. Show forbidden constants compile with
      an unchecked fixture macro, then fail with the production checked macro;
      show valid prefix/platform constants compile. Reject dynamic arguments
      through Rust const evaluation, checking that diagnostic explicitly.
- [ ] Implement `runtime_env!`: evaluate key as a constant `&str`, validate
      prefix/exceptions in the named key constant, and narrowly allow its `var` call.
      Document its contract, errors, compile-failure behavior and example.
- [ ] Convert only the seven approved runtime calls. Preserve their arguments
      and result handling. Exempt build scripts explicitly.
- [ ] Add the runner: use `CLIPPY_CONF_DIR`, production `--lib --bins`, shared
      serving packages as primary packages, and native/WASI/Cloudflare/Spin
      target/features matching repository aliases. Keep test code excluded.
- [ ] Wire fixtures and runner into the existing format workflow Rust job;
      append the CI gate to AGENTS.md. Run `scripts/test-runtime-env.sh`,
      `scripts/lint-runtime-env.sh`, shellcheck on both new scripts, and
      `cargo fmt --all -- --check`. Run targeted Axum startup tests.
- [ ] Run the full guard on a disposable source copy containing a deliberate
      legacy read; verify failure is the policy lint at the injected source.
- [ ] Build/start the real Axum binary using the operator task's selected-source
      path. Record source/artifact identity and startup/application response.
      Back up and restore necessary local launcher/override changes, without
      editing `trusted-server.toml` or `.env`. Stop owned servers.
- [ ] Inspect the complete stack-relative diff, explain evidence and limits,
      create signed commits and verify signatures. Push and create a draft PR
      against `feat/1573-axum-production-serving`, assign the current user,
      and verify GitHub signature status. No board/issue/comment writes.
- [ ] Conduct fresh independent self-review against the merge base, resolve
      findings with targeted regression checks, and update the PR description.
      Confirm remote head/base, clean up owned worktrees/scratch, and report
      roadmap placement (A9, wave 5) plus next dependent work.

## Review log

- Round 1: independently checked source inventory, exclusions, target selection,
  and external macro enforcement. No blocking or optional findings.
- Round 2: independently checked source claims, shared package coverage, build
  exemptions, local/external macros and function references. No blocking or
  optional findings.

- Round 3: implementation review found that an all-features Fastly check
  excludes existing default-only production branches. Added a separate default
  pass and conditional fixtures; a disposable-copy mutation verifies the gap.
