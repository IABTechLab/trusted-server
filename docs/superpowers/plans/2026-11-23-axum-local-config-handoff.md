# Axum local configuration handoff implementation plan

**Goal:** Finish #1141 by explaining and verifying the existing local handoff.

**Architecture:** Preserve prepared-store startup and inherited environment.
Clarify the CLI entry point; extend the first-success smoke without adding a
second configuration conversion or changing provider behavior.

**Stack:** #1646, because the documented local contract includes required
`EDGEZERO__ADAPTER__DATA_DIR` for its declared record store.

## Tasks

- [x] Audit pinned #1635 and #1646 heads; record each acceptance outcome.
- [x] Independently review this plan and the linked
      [design](../specs/2026-11-23-axum-local-config-handoff-design.md), resolving
      blocking and optional findings before implementation.
- [x] Update `crates/trusted-server-cli/src/run.rs` serve and config-push help
      to describe publication, inherited prepared directories and secret values.
- [x] Update `docs/guide/getting-started.md` Option B to launch with
      `ts serve --adapter axum`; explain its Cargo equivalent. Link that setup
      from `docs/guide/cli.md` and `docs/guide/axum-dev.md`, including dotenv,
      restart, and explicit environment responsibilities.
- [x] Extend `scripts/smoke-axum.sh` with malformed-envelope rejection before
      listen. Run the positive case through `ts serve --adapter axum` from the
      isolated project, with `CONFIG_DIR` pointing at the original push directory.
      Do not extract and reconstruct the positive envelope with jq.
- [x] Run `cargo test -p trusted-server-adapter-axum --test startup`,
      `scripts/smoke-axum.sh`, `cargo fmt --all -- --check`,
      `cargo clippy-cli`, shellcheck for the changed smoke, the runtime env
      guard and targeted Prettier for changed Markdown. Inspect actual CLI help.
- [x] From a clean environment with only tool PATH and the existing HOME, run
      documented Option B setup verbatim in the disposable worktree with an
      owned config copy; a local origin serves the tracked overlay's port. Verify
      health, configured-origin response and public URL rewrite through serve.
- [x] Explain final changes, sign and locally verify commits, push without
      rewriting history, confirm GitHub Verified, create a draft PR against
      #1646 using the repository template and watch every hosted check finish.
- [x] Independently self-review the complete PR, resolve findings and update
      its description. Clean up owned processes, scratch files and worktrees.
