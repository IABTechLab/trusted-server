# Per-cookie Template Cache Policy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Separate bounded cookie-selected HTML variants in the shared template cache and route session-bearing requests inline while preserving conservative defaults.

**Architecture:** Extend the existing platform cache key with explicit cookie dimensions. A focused core module validates cookie names and evaluates all request cookie fields once; the publisher uses its decision for lookup and storage. Preserve the existing origin-response guards and early request-validation errors.

**Tech Stack:** Rust 2024, existing `http`, `serde`, `sha2`, `hex`, and error-stack boundaries; existing core test harness, Viceroy, adapter-specific Cargo aliases, and Prettier. No new dependencies.

---

## Inputs, scope, and execution rules

- Spec: [Per-cookie shared-template cache policy](../specs/2026-09-08-1138-per-cookie-template-cache-policy-design.md).
- Branch: `feature/1138-per-cookie-template-cache-policy`. Continue on this branch
  in the current clean checkout; do not create a replacement branch or require a
  worktree migration for this already-isolated task.
- Read `CLAUDE.md` before implementation. Use @superpowers:test-driven-development
  for behavior changes and @superpowers:verification-before-completion before
  success claims or commits. Use @superpowers:systematic-debugging for failures.
- The task text records the implementation steps; the execution record below
  reports actual validation. Completed steps are checked. Rust sketches retain
  their planning form; source contains the reviewed implementation with imports,
  documentation, and formatting.
- Only the completed feature is deployable. Intermediate commits may introduce
  configuration or data types before the runtime consumer is connected.
- Do not change unrelated cookie helpers, error types, cache backends, origin
  request headers, template schema version, or final-response privacy behavior.

## Execution record

Implemented on `feature/1138-per-cookie-template-cache-policy` and independently
reviewed for specification compliance and code quality. Runtime, tests, and docs
are committed together after full verification, consolidating the task-level
commit suggestions above. Fingerprint assertions live alongside the cookie-policy
publisher regressions so they also verify cache invalidation across settings.

The existing diagnostics preparation was found to sanitize Cookie fields before
both generic parsing and the policy gate. Its behavior is preserved; raw parser
and evaluator regressions explicitly enter the already-prepared request boundary,
with a separate regression for normal preparation. The specification and Task 6
record this boundary. Cookie values are also redacted from `Debug` output.

Verification completed:

| Check                                           | Result                                                                                                                       |
| ----------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| Initial key isolation regression                | Failed with cookie encoding absent, then passed with encoding                                                                |
| Initial configuration and evaluator regressions | Failed before implementation, then passed                                                                                    |
| Initial key-only publisher regression           | Failed under the old blanket gate, then passed                                                                               |
| A/B mutation removing cookie key dimensions     | Failed when arm B incorrectly hit arm A; implementation restored                                                             |
| New cookie-policy tests                         | 20 passed                                                                                                                    |
| `cargo test-fastly`                             | Passed: 170 Fastly adapter, 2,476 core, 2 JS Rust, 21 OpenRTB tests; doc tests passed; existing ignored tests remain ignored |
| `cargo test-axum`                               | Passed: 38 tests                                                                                                             |
| `cargo test-cloudflare`                         | Passed: 40 tests                                                                                                             |
| `cargo test-spin`                               | Passed: 80 tests                                                                                                             |
| Integration parity                              | 13 passed                                                                                                                    |
| Six target-specific Clippy aliases              | All passed                                                                                                                   |
| Rust formatting                                 | Passed                                                                                                                       |
| JS build, Vitest, JS formatting                 | Passed; 45 test files, 893 tests                                                                                             |
| Docs formatting and diff whitespace             | Passed                                                                                                                       |

Viceroy required access to the macOS certificate keychain, and two Axum tests
required loopback socket binding. Initial sandbox restrictions were resolved by
rerunning those commands with the required access; no product changes were made
for the environment. No merge, push, or deployment is part of this implementation.

## File map

| File                                                                    | Responsibility                                                                                        |
| ----------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/platform/template_cache.rs`             | Public `TemplateCookieValue`, `TemplateCacheKey.cookie_values`, canonical encoding, key tests         |
| `crates/trusted-server-core/src/platform/mod.rs`                        | Re-export the cookie dimension type                                                                   |
| `crates/trusted-server-core/src/cookies/template_cache_policy.rs` (new) | Internal name validation and byte-preserving cookie-policy evaluator, with unit tests                 |
| `crates/trusted-server-core/src/cookies.rs`                             | Declare the internal child module; preserve existing helpers                                          |
| `crates/trusted-server-core/src/creative_opportunities.rs`              | Optional fields, borrowed list accessors, validation and config tests                                 |
| `crates/trusted-server-core/src/publisher.rs`                           | Single request-policy evaluation, key construction, existing test fixtures and end-to-end regressions |
| `crates/trusted-server-adapter-fastly/src/template_cache.rs`            | Update explicit key test fixture; backend behavior unchanged                                          |
| `docs/guide/configuration.md`                                           | Operator policy examples, guard limitations, safe rollback                                            |
| `trusted-server.example.toml`                                           | Commented example settings and bounded-value guidance                                                 |

Keep the new parser out of the already large publisher module. Keep publisher
regressions beside the existing `template_cache_end_to_end_tests` harness so they
exercise real lookup, reservation, transformation, storage, and finalization.

## Test commands and conventions

Run commands from the repository root unless a working directory is stated.
`cargo test-fastly <filter>` includes the shared core tests under the WASI/Viceroy
target. Other selected packages may report zero matching tests; confirm that the
core package actually executes the intended new tests. Do not use bare
`cargo test --workspace`. `cargo test-axum` alone does not run core unit tests.

For each behavior step: add a test, run its filter and confirm the intended
failure, implement the minimum change, then rerun that filter. A missing new symbol
can be the initial compile failure, but verify behavioral regressions fail with a
compiling stub or old logic before relying on them. Do not count dependency,
toolchain, or simulator failures as a successful red test.

After each completed runtime task, run the relevant target-matched suite; run
`cargo fmt --all -- --check` before its commit. Use descriptive assertions and
`expect("should ...")`; no new `unwrap()` or local imports.

## Task 1: Add cookie dimensions to the platform key

**Files:** `crates/trusted-server-core/src/platform/template_cache.rs`,
`crates/trusted-server-core/src/platform/mod.rs`,
`crates/trusted-server-core/src/publisher.rs`, and
`crates/trusted-server-adapter-fastly/src/template_cache.rs`.

- [x] **1.1 Verify the pre-change key contract.** Reuse the existing
      `rendered_key_is_fixed_size_and_contains_no_request_material` test beside
      `platform::template_cache::tests::key`. Its pinned literal is
      `ts-template-cache-v4-54431eb4ea82644d6378717a8c3f18302fafbf739e684598da79e392b16900a6`.
      Run `cargo test-fastly rendered_key_is_fixed_size_and_contains_no_request_material`
      and verify it passes on the old code. Keep the same expected value after adding
      empty cookie dimensions; do not duplicate the fixture or derive the expected
      key with the new implementation under test.
- [x] **1.2 Add failing key-isolation tests.** Use the common prefix
      `template_cookie_key_` for tests comparing changed values/names, absent versus
      present-empty, quoted versus unquoted values, and two cookie dimensions. Verify
      adding a dimension changes the key, while URL/global surrogate keys stay equal.
      Run `cargo test-fastly template_cookie_key_` and confirm failure before the
      encoder is extended.
- [x] **1.3 Add the public domain type and field.** Follow existing platform API
      visibility and documentation conventions:

  ```rust
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct TemplateCookieValue {
      pub name: String,
      pub value: Option<Vec<u8>>,
  }
  ```

  Add `pub cookie_values: Vec<TemplateCookieValue>` to `TemplateCacheKey` and
  re-export the type from `platform/mod.rs`. Document exact case-sensitive names,
  absent/empty distinction, sorted producer order, and bounded variant use. The
  publisher/evaluator owns sorting; the encoder consumes the supplied order as
  the existing header encoder does.

- [x] **1.4 Extend canonical encoding after the complete existing header section.**
      Reuse the existing length-prefix `push` helper:

  ```rust
  if !self.cookie_values.is_empty() {
      push(&mut canonical, b"cookie-variants-v1");
      push(
          &mut canonical,
          &(self.cookie_values.len() as u64).to_be_bytes(),
      );
      for cookie in &self.cookie_values {
          push(&mut canonical, cookie.name.as_bytes());
          match &cookie.value {
              None => push(&mut canonical, b"absent"),
              Some(value) => {
                  push(&mut canonical, b"present");
                  push(&mut canonical, value);
              }
          }
      }
  }
  ```

  Leave the hash, backend prefix, transform schema version, and surrogate methods
  unchanged. Distinct framing of names and values must prevent concatenation
  collisions; add a test with differently partitioned names/values.

- [x] **1.5 Update every literal with `cookie_values: Vec::new()`.** Locate them
      with `rg -n 'TemplateCacheKey \{' crates --glob '*.rs'`. This includes the
      current publisher production constructor temporarily; Task 4 replaces its empty
      value with evaluated dimensions. Do not change adapter algorithms.
- [x] **1.6 Verify and commit.** Run `cargo test-fastly template_cookie_key_`,
      `cargo test-fastly rendered_key_is_fixed_size_and_contains_no_request_material`,
      `cargo test-fastly`, and
      `cargo fmt --all -- --check`. Expected: isolation tests and legacy fixture pass,
      including the Fastly key consumer. Stage only these files and commit
      `Add cookie variant dimensions to template cache keys`.

## Task 2: Add and validate optional cookie policies

**Files:** `crates/trusted-server-core/src/creative_opportunities.rs`,
`crates/trusted-server-core/src/cookies.rs`, new
`crates/trusted-server-core/src/cookies/template_cache_policy.rs`, and explicit
configuration fixtures in `crates/trusted-server-core/src/publisher.rs`.

- [x] **2.1 Add config tests with prefix `template_cookie_config_`.** Deserialize
      TOML with omitted fields, explicit empty fields, each list independently, and
      both lists. Verify missing fields serialize without either new JSON key and
      that `origin_is_cookie_independent` still defaults false. Reject empty names,
      whitespace, separators, control/non-ASCII names, within-list duplicates, and
      exact-name overlap. Accept `session` and `Session` as distinct names. Retain the
      existing header `Cookie`/`Authorization` rejection tests. Run
      `cargo test-fastly template_cookie_config_` and confirm the pre-feature failure.
- [x] **2.2 Add fields and borrowed accessors.** Place them alongside the current
      template cache fields and update boolean documentation to scoped semantics:

  ```rust
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub template_cache_key_cookies: Option<Vec<String>>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub template_cache_bypass_cookies: Option<Vec<String>>,
  ```

  Accessors named `template_cache_key_cookies()` and
  `template_cache_bypass_cookies()` return `&[String]` using
  `self.<field>.as_deref().unwrap_or_default()`. Do not allocate on access or
  materialize absent options. Add `None` fields to existing struct literals;
  locate them with `rg -n 'CreativeOpportunitiesConfig \{' crates`.

- [x] **2.3 Create shared name validation.** Add
      `pub(crate) mod template_cache_policy;` to `cookies.rs`. In that child module,
      define the reusable predicate below and a validation entry point:

  ```rust
  fn is_cookie_name(name: &[u8]) -> bool {
      !name.is_empty()
          && name.iter().all(|byte| {
              byte.is_ascii_alphanumeric()
                  || b"!#$%&'*+-.^_`|~".contains(byte)
          })
  }

  pub(crate) fn validate_cookie_names(
      key_names: &[String],
      bypass_names: &[String],
  ) -> Result<(), String> {
      let mut seen = std::collections::HashSet::new();
      for (field, names) in [
          ("template_cache_key_cookies", key_names),
          ("template_cache_bypass_cookies", bypass_names),
      ] {
          for name in names {
              if !is_cookie_name(name.as_bytes()) {
                  return Err(format!("{field} contains invalid cookie name `{name}`"));
              }
              if !seen.insert(name.as_str()) {
                  return Err(format!(
                      "{field} repeats cookie name `{name}` within or across cookie policies"
                  ));
              }
          }
      }
      Ok(())
  }
  ```

  Call this from `CreativeOpportunitiesConfig::validate_runtime` using the two
  accessors. Preserve its existing `Result<(), String>` interface and the
  `Settings::validate` conversion into `Report<TrustedServerError>`; do not invent
  a new error stack for this helper. Expand the validation method's error docs.

- [x] **2.4 Verify and commit.** Run
      `cargo test-fastly template_cookie_config_`,
      `cargo test-fastly template_cache_vary_rejects_invalid_header_names`,
      `cargo test-fastly`, and `cargo fmt --all -- --check`.
      Commit `Add validated per-cookie template cache configuration` with only the
      files listed for this task.

## Task 3: Implement the pure cookie-policy evaluator

**File:** `crates/trusted-server-core/src/cookies/template_cache_policy.rs`.

- [x] **3.1 Add decision-matrix tests before implementation.** Use prefix
      `template_cookie_policy_` and raw `http::HeaderMap` fixtures. Cover both boolean
      states; both lists empty; either list alone with the other omitted/resolved
      empty; both lists active; key-only requests; unknown cookies; empty bypass
      cookies; bypass mixed with a key cookie; and case-sensitive names. Test the
      evaluator directly so early publisher validation cannot mask a parser test.
      Run `cargo test-fastly template_cookie_policy_` to observe the missing behavior.
- [x] **3.2 Implement an admitted/bypassed result.** Keep it internal:

  ```rust
  pub(crate) enum TemplateCookieDecision {
      Bypass,
      Eligible(Vec<TemplateCookieValue>),
  }

  pub(crate) fn evaluate_cookie_policy(
      headers: &http::HeaderMap,
      key_names: &[String],
      bypass_names: &[String],
      independent: bool,
  ) -> TemplateCookieDecision {
      if key_names.is_empty() && bypass_names.is_empty() {
          return if headers.contains_key(http::header::COOKIE) && !independent {
              TemplateCookieDecision::Bypass
          } else {
              TemplateCookieDecision::Eligible(Vec::new())
          };
      }

      let mut parsed = std::collections::HashMap::<&[u8], &[u8]>::new();
      for field in headers.get_all(http::header::COOKIE) {
          for raw_pair in field.as_bytes().split(|byte| *byte == b';') {
              let pair = trim_pair(raw_pair);
              let Some(separator) = pair.iter().position(|byte| *byte == b'=') else {
                  return TemplateCookieDecision::Bypass;
              };
              let (name, rest) = pair.split_at(separator);
              let value = &rest[1..];
              if !is_cookie_name(name) || !is_cookie_value(value) {
                  return TemplateCookieDecision::Bypass;
              }
              if parsed.insert(name, value).is_some() {
                  return TemplateCookieDecision::Bypass;
              }
              if bypass_names.iter().any(|item| item.as_bytes() == name) {
                  return TemplateCookieDecision::Bypass;
              }
              if !independent && !key_names.iter().any(|item| item.as_bytes() == name) {
                  return TemplateCookieDecision::Bypass;
              }
          }
      }

      let mut ordered_names: Vec<&String> = key_names.iter().collect();
      ordered_names.sort_unstable();
      TemplateCookieDecision::Eligible(
          ordered_names
              .into_iter()
              .map(|name| TemplateCookieValue {
                  name: name.clone(),
                  value: parsed.get(name.as_bytes()).map(|value| value.to_vec()),
              })
              .collect(),
      )
  }
  ```

  Use these SP/HTAB-only trimming and value-validation helpers. Trimming a broader
  whitespace set would erase malformed bytes that must cause bypass:

  ```rust
  fn trim_pair(mut pair: &[u8]) -> &[u8] {
      while pair.first().is_some_and(|byte| matches!(byte, b' ' | b'\t')) {
          pair = &pair[1..];
      }
      while pair.last().is_some_and(|byte| matches!(byte, b' ' | b'\t')) {
          pair = &pair[..pair.len() - 1];
      }
      pair
  }

  fn is_cookie_value(value: &[u8]) -> bool {
      let payload = if value.first() == Some(&b'"') {
          if value.len() < 2 || value.last() != Some(&b'"') {
              return false;
          }
          &value[1..value.len() - 1]
      } else {
          value
      };
      payload.iter().all(|byte| {
          matches!(byte, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e)
      })
  }
  ```

  Import `TemplateCookieValue` from `crate::platform` at module scope. No request
  mutation, cookie decoding, logging of values, or new request errors belong here.
  Early return on any disqualifier is safe because the entire request bypasses;
  every admitted request must have validated every field. Only key-cookie values
  are owned in the result; parsed unlisted values remain borrowed and temporary.

- [x] **3.3 Add malformed/ambiguous-input tests.** Cover repeated fields, duplicate
      names across/within fields (equal and different values), empty fields/pairs,
      trailing semicolons, bare names, bad quoting, forbidden bytes, additional `=`,
      percent escapes, and quoted/unquoted representations. Assert `name= ` becomes
      present-empty, while `name =A` and `name= A` bypass. Input ordering and header
      splitting must not change eligible sorted dimensions. Under an empty policy,
      all of these inputs retain the old header-presence/boolean decision.
- [x] **3.4 Run evaluator tests and the Fastly suite.** Run
      `cargo test-fastly template_cookie_policy_` then `cargo test-fastly`. Expected:
      policy matrix and raw-byte cases pass. Finish Task 4 before committing this
      runtime helper so production consumes it without unused-code lint allowances.

## Task 4: Connect the evaluator to publisher lookup and store

**File:** `crates/trusted-server-core/src/publisher.rs`.

- [x] **4.1 Add failing key-only and bypass-only integration tests.** Use prefix
      `template_cookie_publisher_` inside `template_cache_end_to_end_tests`. Reuse
      `settings_with_mode`, `navigation_request`, `MemoryTemplateCache`, and `run_via`.
      A key-only request with independence false must be eligible, which fails under
      the old gate. A bypass-only session request with independence true must make no
      lookup or store, which fails under the old gate. Repeat with the unused list
      explicitly empty. Run `cargo test-fastly template_cookie_publisher_` before
      replacing the gate and confirm these behavioral failures.
- [x] **4.2 Replace the blanket cookie check at the existing pre-fetch point.**
      Obtain list slices and the boolean from `settings.creative_opportunities`, using
      `&[]`, `&[]`, and false when the table is absent. Call the evaluator exactly once:

  ```rust
  let (cookie_disqualifies, cookie_values) = match evaluate_cookie_policy(
      req.headers(),
      key_cookie_names,
      bypass_cookie_names,
      origin_is_cookie_independent,
  ) {
      TemplateCookieDecision::Bypass => (true, Vec::new()),
      TemplateCookieDecision::Eligible(values) => (false, values),
  };
  ```

  Import the function/enum at module scope. Remove the obsolete
  `request_had_cookie` local and update only directly affected comments. Preserve
  the existing `!cookie_disqualifies` lookup predicate and pass that same boolean
  to the existing `template_cache_ttl` store check. Move `cookie_values` into the
  key inside `request_can_use_shared_template.then(...)`. Leave all other gate
  predicates and the earlier `handle_request_cookies(&req)?` call untouched.

- [x] **4.3 Verify cache-call and inline behavior.** For both cold and warm cache,
      compare `lookups`, `stored_keys`, and origin-request counts before/after the
      session request. The warm test must first prove the anonymous response actually
      stored a template. Bypass adds zero lookups/reservations/stores and one origin
      request. Inspect `X-TS-Template-Cache`, `X-TS-Assembly`, and finalized body to
      prove existing inline/private behavior. Run both `Finalizer::Streaming` and
      `Finalizer::Buffered` for representative bypass cases.
- [x] **4.4 Verify and commit Tasks 3–4 together.** Run
      `cargo test-fastly template_cookie_policy_`,
      `cargo test-fastly template_cookie_publisher_`, `cargo test-fastly`, and
      `cargo fmt --all -- --check`. Commit only the parser and publisher changes as
      `Apply cookie policy to template lookup and storage`.

## Task 5: Prove downstream variant isolation and response guards

**File:** `crates/trusted-server-core/src/publisher.rs`.

- [x] **5.1 Reproduce the original header-only failure in a regression.** Configure
      key cookie `ab_bucket`, header dimension `x-exp-variant`, and independence true.
      Keep that header absent on every incoming request. Queue distinguishable
      shareable HTML bodies `arm-A` and `arm-B`, both declaring
      `Vary: X-Exp-Variant`. Send A/reader1, B/reader2, A/reader3, and B/reader4.
      Assert correct arm content for every response, exactly two origin fetches,
      exactly two stored keys, and warm-hit diagnostics for the last two requests.
      Briefly run the test with cookie dimensions omitted from key construction to
      prove it detects cross-serving, then restore the implementation and rerun.
      Do not commit the deliberate regression.
- [x] **5.2 Cover absence, empty values, and unknown-cookie policy.** Missing
      `ab_bucket` and `ab_bucket=` store/hit separate bodies. With independence false,
      a configured key cookie alone shares, while adding an unknown cookie bypasses.
      With independence true, unknown cookie changes do not fragment a variant.
      Repeat meaningful cases with both lists configured to prove session bypass wins.
- [x] **5.3 Cover response refusal under the new configuration.** With valid keyed
      requests and otherwise public fresh HTML, verify no store for `Vary: Cookie`
      (case variations and repeated fields), `Vary: *`, uncovered
      `X-Exp-Variant`, and origin `Set-Cookie`. A cookie dimension alone must not count
      as header coverage. Preserve the existing gate tests for authorization,
      freshness, GET-only admission, and inline mode; do not duplicate their complete
      matrices unnecessarily.
- [x] **5.4 Verify readers remain separate after a warm hit.** Adapt the existing
      bidding/finalization tests to run two readers in the same keyed variant and
      prove shared stored bytes remain reader-neutral while final output is private
      and uses per-request assembly. At least the A/B test must run through both
      `run_via` finalizers so storage and warm-hit rendering are exercised.
- [x] **5.5 Verify and commit.** Name the new tests with the
      `template_cookie_publisher_` prefix, then run
      `cargo test-fastly template_cookie_publisher_`,
      `cargo test-fastly template_cache_gate_tests`, `cargo test-fastly`, and
      `cargo fmt --all -- --check`. Commit `Verify cookie variant isolation and session bypass`.

## Task 6: Pin early-error boundaries and policy compatibility

**Files:** `crates/trusted-server-core/src/publisher.rs` and
`crates/trusted-server-core/src/creative_opportunities.rs` tests.

- [x] **6.1 Preserve the selected-header error.** Build a Cookie `HeaderValue` from
      raw bytes that `to_str()` rejects, place it in the selected field, and assert the
      publisher retains `InvalidHeaderValue` at the already-prepared request boundary.
      Set the existing default `GptDiagnosticsRequestDecision` extension in a narrow
      test helper to exercise the idempotent preparation path: ordinary preparation
      otherwise removes unsupported fields before generic cookie parsing. Use a narrow fallible test runner or
      call the handler with the existing harness setup; the current `run` helper
      expects success and must not be used to assert an error. Do not change its
      behavior for existing tests. Assert no cache call occurs and no template stores.
- [x] **6.2 Distinguish later-field input.** Send a valid selected field followed
      by a later field with the same unsupported bytes. Under a named policy, the
      request reaching the evaluator must bypass and use existing inline processing.
      Test a warm and cold cache, and prove the later field is not silently ignored.
      Separately test ASCII malformed pairs and duplicate names that reach the gate.
      Use the same prepared-request helper for raw-field cases, then add a normal
      preparation test proving that existing removal of invalid fields/empty pairs
      remains unchanged and identical forwarded cookie inputs share a template.
- [x] **6.3 Pin policy fingerprinting.** Extend `template_fingerprint_tests` to
      show that adding/changing either cookie list changes the fingerprint. Compare
      serialization of omitted-field fixtures against their pre-change shape, and
      verify explicit-empty versus omitted fields have equal runtime decisions even
      if fingerprints differ. Reuse one memory cache across two settings snapshots to
      show a changed key/bypass policy cannot read an old template under the old key.
- [x] **6.4 Verify default compatibility.** Retain existing
      `by_default_a_cookie_bearing_request_uses_no_shared_cache` and
      `a_declared_cookie_independent_origin_lets_repeat_visitors_share` tests. Test
      both lists explicitly empty under both boolean values and assert old behavior.
      Raw unsupported bytes belong in evaluator tests for legacy policy decisions;
      they do not imply the publisher bypasses its earlier validation.
- [x] **6.5 Verify and commit.** Run
      `cargo test-fastly template_cookie_publisher_`,
      `cargo test-fastly template_fingerprint_tests`,
      `cargo test-fastly template_cookie_config_`, `cargo test-fastly`, and
      `cargo fmt --all -- --check`. Commit `Preserve cookie validation and policy compatibility`.

## Task 7: Document deployment and rollback

**Files:** `docs/guide/configuration.md`, `trusted-server.example.toml`, and the
spec/plan status checkboxes when appropriate.

- [x] **7.1 Update operator examples.** Add commented optional lists alongside
      existing template cache settings. Describe bounded values, exact-case names,
      bypass-on-presence including empty values, unlisted-cookie boolean behavior,
      and conservative opt-in parsing. Include key-only A/B and bypass-only session
      examples, using fictional values and domains only.
- [x] **7.2 Explain the downstream header contract.** Show why a header created
      after TS cannot alone distinguish variants. The example must include both
      `ab_bucket` and `x-exp-variant` in their respective lists. Explain that
      `Vary: Cookie` always refuses storage and that cookie configuration does not
      automatically cover a named header. Avoid implying the drift guard verifies
      downstream transformations or checks cached hits against current origin policy.
- [x] **7.3 Document safe rollback.** Add both fields to the existing list that
      must be removed before loading configuration into older binaries. Even empty
      fields are unknown to those binaries. When removing policies for a
      cookie-dependent origin, set independence false or disable ESI. Explain
      fingerprint invalidation and existing purge tools without adding new tooling.
- [x] **7.4 Verify and commit.** Run `npm run format` from `docs`, and
      `git diff --check` from root. If formatting fails, use the installed Prettier on
      only changed Markdown files and rerun the check. Commit
      `Document per-cookie template cache policy and rollback`.

## Task 8: Full verification and implementation handoff

**Files:** Only fixes directly required by the feature and validation evidence.

- [x] **8.1 Inspect the final diff against the feature base.** Check that no raw
      cookie values enter logging, metric labels, diagnostics, or cache Debug output.
      Verify the full Cookie header is never a dimension, no origin headers are
      mutated, and no user-shaped identity field is introduced as an implicit key.
- [x] **8.2 Run the full repository CI command set.** Execute each command and
      record its actual result. Independent target commands may run concurrently if
      Cargo locking/resource usage is acceptable; do not mistake a queued build for
      a passing check.

  ```bash
  cargo fmt --all -- --check
  cargo clippy-fastly
  cargo clippy-axum
  cargo clippy-cloudflare
  cargo clippy-cloudflare-wasm
  cargo clippy-spin-native
  cargo clippy-spin-wasm
  cargo test-fastly
  cargo test-axum
  cargo test-cloudflare
  cargo test-spin
  cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
  ```

  In `crates/trusted-server-js/lib`:

  ```bash
  node build-all.mjs
  npx vitest run
  npm run format
  ```

  In `docs`:

  ```bash
  npm run format
  ```

  If any command is blocked by dependencies, network, missing tools, or baseline
  failures, record the exact command and error. Do not claim the full gate passed.
  Use target-specific aliases; do not substitute all-feature workspace commands.

- [x] **8.3 Request an independent implementation review.** Use
      @superpowers:requesting-code-review with this plan, the spec, the base commit,
      final diff, and actual validation results. Resolve concrete findings and rerun
      affected checks; repeat broader checks only when the changes justify it.
- [x] **8.4 Mark completed tasks and report evidence.** Summarize variant isolation,
      session bypass, unchanged defaults/response guards, and any validation limits.
      Commit only intended changes. Confirm branch and working-tree state with
      `git status --short --branch`. Publishing a PR or deploying follows the user's
      subsequent instruction; neither is part of writing this plan.
