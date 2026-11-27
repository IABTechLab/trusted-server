# Adapter store inventory rework plan

**Goal:** Rework existing #1627 against production Axum startup while preserving
its supported store paths and documenting unsupported Axum KV.

1. Pin #1627/#1635/#1637 revisions and verify AX-2, ID-2/DEC-16 and startup
   claims against code and authoritative requirements. Inventory the complete
   PR delta and local tools; preserve the existing clean branch worktree.
2. Independently review this spec and plan, resolve blocking and optional
   findings, and record the resulting engineering decisions.
3. Add a failing Axum context-path test in `src/platform.rs` requiring KV reads
   and writes to return `Unavailable`. Run it on the old implementation with
   its file path enabled in a fresh test process (`--exact`) to demonstrate
   rejection without OnceLock contamination. Save the patch privately, restore
   the test file before merging, then reapply the final context test without the
   obsolete variable.
4. Merge #1635 using `git merge --no-commit --no-ff`; resolve Axum `app.rs`,
   `platform.rs`, Cargo manifest/lock and docs conflicts by preserving #1635's
   prepared config/secret startup and restoring its unavailable KV. Remove all
   file-specific tests and the production `bytes` dependency; keep `tempfile`
   required by #1635 startup tests and do not restore `temp-env`. Retain the new
   unavailable regression, and
   preserve other adapters' store changes.
5. Update `docs/guide/{edgezero,axum-dev,architecture,api-reference}.md`, Axum
   README, and the existing inventory design/plan. Search the full current docs/source tree
   for stale environment/file-store/startup statements, including `request-signing.md`, `spin.md`, `cloudflare.md` and `fastly.md`.
   Preserve Axum startup rejection of enabled signing and management writes.
6. With scoped pinned tools, run the new Axum context test and relevant startup
   tests (including snapshot immutability and signing rejection), targeted Fastly store tests under Viceroy, Cloudflare binding/signing
   tests and Spin route/config tests. Check Cloudflare production WASM and run a
   local Worker KV binding round trip/absence probe, since host tests cannot
   execute that binding. Run rustfmt and targeted changed-adapter Clippy checks
   (not the full CI list). Build the production Axum binary. Run
   #1637's complete compiler guard in an owned disposable merge with the
   candidate, including its macro and existing-read migrations; copying just
   the script would reject preexisting Fastly reads. Report this as compatibility
   evidence and keep the sibling PR out of the shipping branch. Do not run the full CI gate list.
7. Run real local application probes: Fastly Viceroy KV round trip and missing
   binding plus pages; Spin discovery and signed auction from `default` plus
   missing-key control. Check captured KID against `default` and cryptographically
   verify the signature with the fixture public key; production Axum prepared
   stores, unavailable KV and
   pages. Prefer existing harnesses. Use temporary instrumentation only for
   infrastructure with no normal consumer; restore exact shipping source and
   rebuild affected artifacts. Preserve sanitized evidence and limitations.
8. Review combined diff, explain changes/evidence, sign commits (including the
   merge) and push existing branch without force; retarget/update existing PR
   using `.claude/agents/pr-creator.md` and the template. Record decision, current
   runtime evidence and CI pending status; no board, issues or other PR comments.
9. Perform fresh independent self-review of the complete PR against its new
   merge base. Resolve findings under the existing implementation authorization,
   rerun affected checks, and update PR description. Report exact revision,
   remaining CI uncertainty, draft readiness, EdgeZero-owned gaps and cleanup.
