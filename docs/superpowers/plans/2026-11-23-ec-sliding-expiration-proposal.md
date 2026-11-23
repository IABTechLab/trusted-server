# EC sliding-expiration proposal implementation plan

## Goal and boundaries

Implement the approved configurable lifetime and opt-in every-visit renewal in
[the spec](../specs/2026-11-23-ec-sliding-expiration-proposal-design.md). Keep
#1646's default behavior, record operations and platform capabilities. The PR
remains draft; its public title/description are not edited by this work.

## Tasks

- [ ] Restore the affected baseline files and remove abandoned proposal additions,
      including obsolete code, tests, settings and documentation.
- [ ] Add failing load/serialization tests in `settings.rs` and `config.rs` for
      default/missing 365, valid 1/400/custom lifetime, invalid 0/401 and types.
- [ ] Implement checked `MaxAgeDays` and default-disabled `sliding_refresh`,
      omitting both defaults from serialization; use error-stack diagnostics.
- [ ] Route positive issuance in `ec/cookies.rs` through the configured lifetime.
      Test both cookie setter entry points and preserve withdrawal zero.
- [ ] Split `ec/finalize.rs`'s mirror lifetime into an independent one-year
      constant. Preserve SharedID and its consent cookie policy.
- [ ] Add failing shared-app tests for same-value renewal, private final headers,
      method/status/304 handling, API/assets, creation/rotation and withdrawal.
- [ ] Test unread, unavailable, missing, revoked, failed and write-failed states
      separately with observed record operations; implement the approved no-read
      rule using existing write outcomes and request-local state.
- [ ] Gate renewal only with the existing publisher document authorization.
      Keep the public API finalization contract and disabled path unchanged.
- [ ] Test actual Axum/Fastly routed responses and Fastly's final privacy guard.
      Test Cloudflare/Spin status/header/body parity across default and enabled
      configuration, including unsupported EC/API routes. Add no issuer there.
- [ ] Test EC/configured SharedID template-key rejection and a valid control;
      retain neutral raw-template caching semantics.
- [ ] Update configuration guidance and example values. Use a repository spec
      path instead of a branch-absolute link to an excluded documentation page.
- [ ] Run only targeted core, `cargo test-axum`, `cargo test-cloudflare`,
      `cargo test-spin` and Fastly tests under isolated Viceroy 0.21.0.
- [ ] Run touched-target clippy, `cargo fmt --all -- --check`, changed-doc
      prettier, and `scripts/lint-runtime-env.sh`.
- [ ] Recheck the live base, merge it if moved, sign commits, verify signatures,
      push normally and confirm GitHub Verified. Watch every hosted check.
- [ ] Independently review the complete PR against its merge base and resolve
      findings. Draft new PR title/description without applying either.
- [ ] Report full source-tree search results for removed design identifiers,
      preserving unrelated existing record/HTML/privacy terminology.
- [ ] Remove owned worktrees, test resources and scratch after publication;
      confirm `git worktree list` and preserve other sessions' work.
