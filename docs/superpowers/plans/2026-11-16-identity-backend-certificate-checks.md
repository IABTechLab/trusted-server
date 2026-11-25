# Identity backend certificate checks implementation plan

**Goal:** Close #1556 on the cutover base with a regression test showing that a
relaxed `proxy.certificate_check` has no effect on pull sync, plus operator
wording that covers partner and identity traffic.

**Architecture:** No production behavior change. The cutover (#1506) already
removed the setting from pull sync, and EdgeZero always verifies TLS. A
differential unit test pins the invariant at the only identity backend on this
base.

**Tech stack:** Rust unit tests in `trusted-server-core` with the existing
`ScriptedHttpClient`, `TestClock` and in-memory `KvIdentityGraph`.

**Specification:**
[identity backend certificate checks](../specs/2026-11-16-identity-backend-certificate-checks-design.md).

## Base and execution gates

- Branch `fix/1556-identity-backend-tls` from `spec/edgezero-pr275-migration`
  at `cc811fc7aa0d234e447a868af5b8ab4ea359326a`. Before coding, confirm with
  `gh pr view 1506` that #1506 is still open and that its head is still
  `cc811fc7a`. If the head moved, merge it in. If #1506 merged, branch from
  main and recheck that `git grep certificate_check -- crates/` shows no
  runtime reader.
- Recheck the spec's source table against the chosen base: `pull_request` in
  `ec/pull_sync.rs`, `reject_disabled_certificate_check` and
  `warn_ignored_certificate_check` in `config.rs`, and the EdgeZero pin.
- Commit only with the user's approval; sign every commit.

## File map

| Path                                                                                     | Change                                                   |
| ---------------------------------------------------------------------------------------- | -------------------------------------------------------- |
| `crates/trusted-server-core/src/ec/pull_sync.rs`                                         | New test in the scripted-client test module              |
| `crates/trusted-server-core/src/config.rs`                                               | Reject and warn message wording, plus their doc comments |
| `crates/trusted-server-core/src/settings.rs`                                             | `Proxy::certificate_check` doc comment                   |
| `docs/guide/configuration.md`                                                            | `certificate_check` table row and section wording        |
| `docs/guide/error-reference.md`, `docs/guide/edgezero.md`, `trusted-server.example.toml` | Same wording at lines 195, 100 and 213                   |

## Task 1: Differential pull-sync regression test

**File:** `crates/trusted-server-core/src/ec/pull_sync.rs`, the test module that
imports `scripted_client` (around line 852).

- [ ] **Step 1: Add a run helper.** Add a test-local function
      `pull_once(settings: &Settings) -> (RecordedRequest, KvEntry)`. It builds
      the registry from `pull_enabled_ec_partner("alpha.example.com")`, a fresh
      `KvIdentityGraph::in_memory("pull_store")`, seeds it with
      `seed_present_snapshot`, scripts `ScriptedReply::status(StatusCode::OK).body(r#"{"uid":"synced-uid"}"#)`
      for `pull_url("alpha.example.com")`, and calls `run_pull_sync`. It
      destructures `client.requests()` into exactly one request
      (`let [request] = requests.as_slice() else { panic!("should send one pull request, got {requests:?}") }`)
      and returns `request.clone()` (`RecordedRequest` is `Clone`) with the
      stored entry from `graph.get(&ec_id)`. Add `RecordedRequest` to the
      existing `crate::test_support::scripted_client::{...}` import of that
      module. Everything else the test needs is already in scope through
      `use super::*` and the existing imports.

- [ ] **Step 2: Write the test.**

  ```rust
  #[test]
  fn dispatch_pull_sync_ignores_relaxed_certificate_check() {
      let strict = create_test_settings();
      let mut relaxed = create_test_settings();
      relaxed.proxy.certificate_check = false;
      assert!(strict.proxy.certificate_check, "should default to verified TLS");

      let (strict_request, strict_entry) = pull_once(&strict);
      let (relaxed_request, relaxed_entry) = pull_once(&relaxed);

      assert_eq!(relaxed_request.method, strict_request.method, "should pull with the same method");
      assert_eq!(relaxed_request.uri, strict_request.uri, "should pull the same HTTPS URL");
      // Compare without printing header values, which carry the bearer token.
      assert!(relaxed_request.headers == strict_request.headers, "should send the same headers");
      assert_eq!(relaxed_request.body, strict_request.body, "should send the same body");
      for request in [&strict_request, &relaxed_request] {
          request.assert_transport(ExpectedTransport {
              timeout: PULL_SYNC_GROUP_CUTOFF,
              response_mode: ResponseMode::Buffered { max_bytes: MAX_PULL_RESPONSE_BYTES },
              response_decoding: ResponseDecoding::Decode,
              cache_policy: OutboundCachePolicy::PlatformDefault,
          });
      }
      for entry in [&strict_entry, &relaxed_entry] {
          assert_eq!(
              entry.ids.get("alpha.example.com").map(|id| id.uid.as_str()),
              Some("synced-uid"),
              "should persist the partner UID with either setting"
          );
      }
  }
  ```

  Adjust field access to the real `RecordedRequest` and `KvEntry` APIs found in
  Step 1. Keep comments above code, not inline.

- [ ] **Step 3: Run it.** For fast iteration, run core natively:
      `cargo test -p trusted-server-core --lib --target <host-triple> ec::pull_sync`.
      The gate is `cargo test-fastly -- ec::pull_sync`. The test passes on the
      first run because #1506 already fixed the behavior. That is expected, and
      Step 4 is what shows the test can fail.

  Scope of the proof: the pinned `OutboundRequest` and `RecordedRequest` have
  no TLS field. So the test proves that the relaxed setting changes nothing
  observable in the request (method, URI, headers, body and transport). It
  does not test TLS verification itself, which is EdgeZero's adapter contract.
  The PR body must say so.

- [ ] **Step 4: Mutation check (local only, reverted).** Temporarily make
      `dispatch_pull_sync` add a header or change the timeout when
      `!settings.proxy.certificate_check`. Confirm the new test fails with a
      message that names the difference. Revert, rerun, and record the failing
      output for the PR description.

## Task 2: Operator wording

- [ ] In `config.rs`, change the reject message to
      `"proxy.certificate_check = false is not supported; TLS is always verified on every outbound HTTPS request, including partner and identity requests"`.
      Change the warning in the same way, and keep the word "ignored" in it:
      `runtime_ignores_certificate_check_false_with_warning` (config.rs:1800-1803)
      matches both `proxy.certificate_check` and `ignored`. Update both doc
      comments so they say "every outbound HTTPS request" instead of
      "origin TLS". Plain-HTTP local origins stay supported.
- [ ] In `settings.rs`, change the `Proxy::certificate_check` doc comment to
      `/// Must be true. TLS is always verified on every outbound HTTPS request.`
- [ ] Leave `CHANGELOG.md:69` alone. It accurately describes #1506's
      change.
- [ ] In `docs/guide/configuration.md`, the `certificate_check` table row
      (around line 1460) and section (around line 1516): say that verification
      covers the publisher origin, assets, redirects, integrations, auction
      providers and EC pull sync. Keep the existing operator guidance. Apply
      the same wording to `docs/guide/error-reference.md:195`,
      `docs/guide/edgezero.md:100` and `trusted-server.example.toml:213`.
- [ ] Run `cargo test-fastly -- config::` and confirm that
      `validate_rejects_certificate_check_false` and
      `runtime_ignores_certificate_check_false_with_warning` still pass.

## Task 3: Verification

Run the gates that cover the touched crate and docs:

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy-fastly`, `cargo clippy-axum`, `cargo clippy-cloudflare`,
      `cargo clippy-spin-native` (core is compiled into every adapter)
- [ ] `cargo test-fastly`, `cargo test-axum`, `cargo test-cloudflare`,
      `cargo test-spin`
- [ ] `cd docs && npm ci && npm run format`. This checks the edited
      `configuration.md` and also this spec and plan, which are committed with
      the change.
- [ ] `cargo run --manifest-path crates/trusted-server-cli/Cargo.toml --target <host-triple> --bin ts -- dev lint domains --changed-vs origin/spec/edgezero-pr275-migration`
      (the new test uses `example.com` hosts only)

Record any gate that cannot run, with the exact blocker.

## Task 4: Handoff (needs approval for any remote action)

- [ ] PR base `spec/edgezero-pr275-migration`. Title: "Test that identity
      backends ignore a relaxed certificate check". The body explains that
      #1506 already removed the setting from pull sync, describes the new test
      and the mutation evidence, and lists criterion 3 as the #1489 obligation.
      Use "Closes #1556" or "Refs #1556" as the user decides (spec, Delivery
      and stacking).
- [ ] Draft, but do not post without approval, a comment on PR #1487 (where
      `PlatformLookupTransport` lives, which #1489 reuses): "#1556 criterion 3:
      the lookup transport already forces `certificate_check: true` and takes
      no setting, and the pull-sync provider's `PullSyncSettings {}` is empty,
      but no test pins this. When this stack rebases onto the cutover and
      `PlatformBackendSpec` goes away, please keep the transport settings-free
      and add a transport-level test that a relaxed `proxy.certificate_check`
      leaves lookup and pull-sync requests unchanged."
- [ ] Optional, not filed without approval: an EdgeZero issue asking for a
      unit test that Fastly TLS backends always call `check_certificate`.

## Out of scope

- Removing `proxy.certificate_check` from the config schema.
- Runtime TLS probes against an origin with a bad certificate.
- Changes to #1487, #1489 or the identity conformance suite (#1613).
