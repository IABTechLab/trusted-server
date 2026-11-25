# Proxy Disclosure Policy Implementation Plan

> **For agentic workers:** Use the executing-plans workflow to implement this plan
> task by task, or use independent implementation workers with review between
> tasks when authorized. Steps use checkboxes for tracking. Implementation and local verification are complete;
> release qualification is tracked below.

**Goal:** Stop unintended creative-proxy EC disclosure and coordinate explicit
identity delivery with headers, redirects, cache bypass, and response privacy.

**Architecture:** Compile origin/path recipient rules in core. Evaluate them
inside the existing main-branch proxy loop before every send, using request-local
authorization and sticky response privacy. Reuse that evaluator in CLI explain,
while preserving existing integration and publisher-asset contracts.

**Tech stack:** Rust 2024, error-stack, derive_more, serde, url, EdgeZero v0.0.11,
the current platform HTTP abstractions, Viceroy, Node runtime fixtures, Prettier.

**Specification:** [Proxy disclosure and cache policy](../specs/2026-11-16-1233-proxy-disclosure-policy-design.md).

**Base:** main `1810c763add220dd34fd5faaffe2c69aab1d556f`; documentation branch
`docs/1233-proxy-disclosure-policy`. No dependency on the pending outbound cutover.
Reconcile transport-specific tasks if that cutover lands before implementation.

---

## Execution boundaries

Implementation is authorized in the isolated branch above. Commits, pushes,
deployment, and GitHub comments require their applicable authorization. A staging
qualification command does not authorize deployment.

Read the current checkout's AGENTS and CONTRIBUTING before execution. Preserve
unrelated work. Use an isolated implementation checkout and checkout-local
dependencies; never repoint another checkout's node_modules or build output.
Use Rust 1.98.1 and Node 24.20.0 from the selected base's pins, and re-evaluate
pins if the base changes. No new production dependency is expected.

Each implementation task follows regression-first development: add a meaningful
failing assertion, establish the intended failure, implement the smallest change,
then rerun the focused test and the affected adapter alias. Do not treat a syntax
or setup error as a behavioral failure. Keep intermediate changes local until
the combined acceptance works; signed incremental commits may be made only when
authorized and after their applicable checks pass.

## File and responsibility map

All paths below are relative to the implementation repository root. New filenames
are proposed additions, not claims that these modules already exist.

| Files                                                                                                                                 | Responsibility                                                                                        |
| ------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| Create `crates/trusted-server-core/src/proxy_policy.rs`                                                                               | Configuration selectors, compiled matchers, typed decisions, request-local disclosure and chain types |
| Create `crates/trusted-server-core/src/proxy_query.rs`                                                                                | Private raw query EC removal/replacement and Location path checks                                     |
| Modify `crates/trusted-server-core/src/lib.rs`                                                                                        | Export only policy types needed by CLI; keep query helper private                                     |
| Modify `crates/trusted-server-core/src/settings.rs` and `src/config.rs`                                                               | Typed field, compilation, consistent TOML/JSON/blob validation                                        |
| Modify `crates/trusted-server-core/src/consent/mod.rs`                                                                                | Pure proxy disclosure eligibility helper                                                              |
| Modify `crates/trusted-server-core/src/proxy.rs`                                                                                      | Explicit identity selection, per-send policy, headers, bypass, strict exits, response enforcement     |
| Modify `crates/trusted-server-core/src/response_privacy.rs`                                                                           | Shared terminal-private application/reassertion                                                       |
| Modify `crates/trusted-server-core/src/integrations/{js_asset_proxy,prebid,gpt,google_tag_manager,testlight}.rs`                      | Explicit disabled query identity and preservation assertions                                          |
| Modify `crates/trusted-server-adapter-fastly/src/{app,main,middleware}.rs`                                                            | Geo-aware context, existing lifecycle preservation, private errors and terminal invariants            |
| Modify `crates/trusted-server-adapter-{axum,cloudflare,spin}/src/{app,middleware}.rs`                                                 | Context construction, private error conversion and terminal enforcement                               |
| Modify `crates/trusted-server-cli/src/{run.rs,commands/config/mod.rs}`                                                                | Register read-only config proxy subcommand                                                            |
| Create `crates/trusted-server-cli/src/commands/config/proxy.rs`                                                                       | Explain using effective config and shared evaluator                                                   |
| Create `crates/trusted-server-cli/tests/config_proxy.rs`; extend `tests/config_env_overlay.rs`                                        | Real CLI parsing, output, exits and overlay behavior                                                  |
| Modify `crates/trusted-server-integration-tests/tests/parity.rs`                                                                      | Three native adapter route/response-policy parity                                                     |
| Create `crates/trusted-server-adapter-fastly/src/proxy_policy_runtime.rs`                                                             | Test-only actual-handler/transport network cases                                                      |
| Create `crates/trusted-server-adapter-fastly/tests/proxy-policy-origin.mjs`                                                           | Owned local origin, WASM test discovery, bounded Viceroy runner                                       |
| Modify `crates/trusted-server-adapter-fastly/src/main.rs` under `cfg(test)`                                                           | Register the runtime test module                                                                      |
| Modify `crates/trusted-server-cli/tests/proxy_trusted_server.rs`                                                                      | CLI transport preserves actual TS resource URL and privacy behavior                                   |
| Modify `trusted-server.example.toml`, `docs/guide/{first-party-proxy,configuration}.md`, `scripts/README.md`, integration-test README | Operator configuration, migration and qualification commands                                          |
| Modify relevant existing format/test workflow                                                                                         | Run the new bounded network fixture in PR CI                                                          |

Prefer extending existing test doubles in `proxy.rs` to adding a public fixture
framework. If shared observations are needed, use the existing test-utils boundary
in `crates/trusted-server-core/src/test_support.rs`; never add production bypasses.

## Task 1: Establish the main-base consumer and test seams

**Read:** core `proxy.rs`, `platform/http.rs`, `consent/mod.rs`, `ec/mod.rs`,
`ec/finalize.rs`; adapter `app.rs`/middleware and Fastly `main.rs`; CLI
`app_config.rs`, `run.rs`; current CI and tool pins.

- [x] Confirm HEAD/base and verify the loop is still `proxy_with_redirects` over
      `PlatformHttpRequest`. Stop transport-specific implementation and reconcile
      the plan if the SDK cutover has landed; do not copy an API from another branch.
- [x] Inventory every `ProxyRequestConfig::new`, struct literal, and
      `forward_ec_id` reference. Record the intended preserved contract for each.
- [x] Trace Fastly `execute_named` through `attach_dispatch_extensions`, final
      EC finalization and terminal effects. Identify the named-route error conversion
      points in all adapters, including pre-route filter short circuits.
- [x] Establish a canonical valid cookie fixture with
      `ec::is_valid_ec_id`, serialized consent parsed from a request, and explicit
      permitting geo. Prove a positive context, then deny Purpose 4 and remove geo
      to prove the controls reach different authorization outcomes.
- [x] Extend the existing header-aware recording stub to capture actual URI,
      headers, method/body and `PlatformCacheIntent` for each dispatch. Reuse
      `RuntimeServices` injection; do not simulate the result inside an assertion.
- [x] Establish a handler-level regression: the signed static target with two
      valid cookies currently yields reader-specific upstream URIs. Assert the
      intended identical URI and establish the behavioral failure.

Run from root:

```bash
cargo test -p trusted-server-core proxy_policy -- --nocapture
```

The tests added in this task use the `proxy_policy` name prefix so this filter
selects a nonzero set. Verify test enumeration, then inspect the failure output;
an empty selected set is a setup failure, not a passing baseline.

**Acceptance:** valid positive identity inputs and a discriminating static
regression, with the source-to-observation path recorded. No new lifecycle I/O.

## Task 2: Add validated configuration and immutable matching

**Files:** new `proxy_policy.rs`, core `lib.rs`, `settings.rs`, `config.rs`.
**Spec:** sections 3–5; A8, A18.

- [x] Add tests for default omission and typed TOML/JSON/environment list input,
      unique IDs, exact/default-port origin equivalence, alternate-port rejection,
      root prefix, segment boundary, exact-versus-prefix diagnostic precedence,
      malformed host/path, duplicate selector and unsupported query-based matching.
- [x] Add a valid nonempty endpoint list to a real typed app-config round-trip;
      assert the nested fields themselves, not only successful deserialization.
- [x] Implement the shape below, with public-item docs, validated ID/origin fields,
      deny-unknown-fields, existing sequence/map deserialization and exact-one-path
      validation. Avoid allocating lowercase hostnames on every request.

```text
ProxyIdentityEndpoint { id, origin, path_exact?, path_prefix? }
CompiledEndpoint { rule, normalized_host, effective_https_port }
ProxyIdentityEndpoint retains exactly one validated exact/prefix path selector
CompiledProxyPolicy::evaluate(target) -> endpoint match or default denial
```

- [x] Add the default-empty, omit-empty raw field to `Proxy`. Store compiled
      state with private matcher internals and a hidden public, nonserialized storage
      field for external struct-update compatibility. Compile during settings normalization and
      typed validation; propagate concrete configuration errors with error-stack.
- [x] Reconcile any struct-literal API impact of private compiled state through
      the hidden storage field and the supported preparation method. Ensure programmatic route
      construction validates/compiles before use; do not retain a stale matcher after
      a test mutates raw settings.
- [x] Test an older-reader shape that rejects unknown fields: empty new output
      remains readable, nonempty output intentionally does not. Never weaken
      `deny_unknown_fields` globally to make compatibility tests pass.

Run:

```bash
cargo test -p trusted-server-core proxy_policy
cargo test-axum
cargo clippy-axum
```

**Acceptance:** one compiled rule source, stable diagnostics, no empty-list
fail-open behavior, and truthful writer/reader compatibility.

## Task 3: Implement request authorization and precise query edits

**Files:** consent `mod.rs`, new `proxy_query.rs`, `proxy_policy.rs`, core `lib.rs`.
**Spec:** sections 4.1, 6, 7.1; A4, A5, A7, A8.

- [x] Write table-driven tests for the exact consent composition in spec §6:
      granting GDPR P1+P4, denied P4, expired context, malformed/proxy-mode signals,
      unknown jurisdiction, known non-regulated, GPC, and US opt-out.
- [x] Implement the pure helper by composing existing
      `allows_ec_creation`/`allows_eid_persistence`, explicit expiry and GPC guards.
      Do not edit those existing consumers' semantics or invent legal vendor rules.
- [x] Build `ProxyDisclosureContext` from the canonical existing cookie and
      `EcContext::consent`. No header fallback, generation, store or partner call.
- [x] Write query cases with duplicate `ts-ec`, encoded keys, uppercase keys,
      nested encoding, empty/key-only components, `%20` versus `+`, empty query and
      unrelated escape sequences. Define the input boundary as the authenticated
      reconstructed URL, not unavailable pre-sign text.
- [x] Implement the private raw-component edit: split components, decode only
      keys once for reserved-key comparison, retain unaffected raw slices/order,
      remove matches, and append one encoded authorized ID when requested. Preserve
      an untouched query without rebuilding it.
- [x] Add raw Location path tests distinct from query escapes. Inspect raw
      Location before resolution, deny identity eligibility for encoded/backslash
      paths, and separately evaluate the resolved path. Do not add token provenance.

Representative query contract:

```text
input:  /asset?a=%20&ts%2Dec=old&flag&b=+
deny:   /asset?a=%20&flag&b=+
grant:  /asset?a=%20&flag&b=+&ts-ec=<encoded-authorized-cookie>
```

Run:

```bash
cargo test -p trusted-server-core proxy_policy
cargo test -p trusted-server-core proxy_query
cargo test-axum
```

**Acceptance:** meaningful authorization controls, no legacy header override,
precise reserved-parameter handling, and no claimed recovery of lost bytes.

## Task 4: Enforce typed policy before every creative send

**Files:** core `proxy.rs`, `proxy_policy.rs`, listed integration files.
**Spec:** sections 5, 7, 8, 10, 11; A1–A3, A6, A9, A12, A17.

- [x] Add failing signed-handler tests for default static A/B, matched event,
      matched `.gif`, same-host unmatched path, static→event, event→static with echoed
      EC, event→event, invalid signatures, blocked fetch hosts and userinfo.
- [x] Replace `forward_ec_id` with explicit `ProxyQueryIdentity`. Generic
      construction defaults to `Disabled`; preserve `without_ec_id`. Migrate every
      production constructor/literal and its assertions explicitly.
- [x] Put an optional creative execution input struct into the existing proxy
      policy: compiled rules, immutable disclosure context and mutable chain state.
      Disabled integrations do not need an EC context. Keep function arity ≤7.
- [x] Remove unconditional initial creative EC appending. At the top of every
      loop iteration, validate destination and userinfo, compute the decision,
      transform EC query, set the effective current URL to the URL actually sent,
      build the whitelist headers, and mark endpoint visitation before dispatch.
- [x] Set `.with_cache_bypass()` on each matched endpoint's actual
      `PlatformHttpRequest`, even with missing identity. Do not merely set a field
      in the evaluator's explanation. Keep resource requests platform-default.
- [x] Preserve all existing method/body and streaming options. Apply strict
      creative headers from spec §8 without changing integration additional-header
      semantics. No credential or referrer/IP restoration on redirects.
- [x] Centralize strict creative response exits: any returned 3xx with Location
      becomes a private 502. Cover limit, unsupported scheme and early-return paths;
      no Location is emitted. Preserve noncreative no-follow behavior.
- [x] Replace full-URL creative logging with origin/path/rule/reason. Include a
      log-capture assertion using a query sentinel, not only an EC string check.

Run the focused tests, then:

```bash
cargo test-fastly
cargo test-axum
cargo clippy-fastly
cargo clippy-axum
```

**Acceptance:** actual dispatched requests obey the policy on every hop; existing
integration contract tests remain meaningful after the typed API migration.

## Task 5: Carry privacy through finalization and preserve withdrawal

**Files:** core `proxy.rs`, `response_privacy.rs`; all adapter `app.rs` and
middleware; Fastly `main.rs`.
**Spec:** sections 6, 9–11; A4, A10–A14, A17.

- [x] Add a public personalized-origin fixture and a repeated-event fixture.
      Assert actual cache intent and private/no-store output with validators,
      expiry/age and all runtime edge-cache directives absent.
- [x] Apply terminal-private success response enforcement after rewrite and
      strip upstream state-setting headers before serving the creative response.
      Keep CORS stripping. Do not strip later TS-owned withdrawal cookie expiry.
- [x] Add route-aware error conversion that retains each error's existing
      classification and marks all creative errors terminal-private. Cover origin
      failure after a matched event, rewrite failure, invalid signature and strict
      redirect rejection. Handle pre-route creative errors/short circuits through
      final response privacy without weakening authentication or filters.
- [x] Pass Fastly's existing geo-aware EC context into the creative handler;
      add the existing context builder at the native/Worker route calls. Context
      extraction failure withholds identity. Do not silently use unregulated geo.
- [x] Preserve Fastly's existing `EcFinalizeState` attachment and withdrawal
      handling. Add a real dispatch-plus-finalize test: explicit withdrawal with an
      existing cookie still expires it and tombstones the record; disclosure stays
      withheld. Marker handling remains supported. Do not require total zero store
      calls for this preexisting revocation path.
- [x] Reassert terminal-private policy after configurable headers and late
      effects on each adapter. Test a hostile late public Cache-Control and each
      platform edge-cache header. Preserve unrelated origin-private behavior.
- [x] Confirm no proxy-policy path generates identity, renews cookies or adds
      provider/store I/O. Retain existing route finalization, not new parity work
      that enables unsupported identity lifecycle on other adapters.

Run:

```bash
cargo test-fastly
cargo test-fastly-reuse
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

**Acceptance:** policy survives the actual final response path, including errors
and withdrawal. Fastly reuse cannot retain reader A's ID or chain state for B.

## Task 6: Add shared CLI explanation and configuration tests

**Files:** CLI `run.rs`, `commands/config/{mod,proxy}.rs`, new
`tests/config_proxy.rs`, existing `tests/config_env_overlay.rs`.
**Spec:** section 12; A18–A20.

- [x] Add real CLI tests invoking `config proxy explain <URL> --json` with a
      temporary manifest/config. Require version 1 fields from the spec, no raw URL
      query/fragment, no identity/consent input, no fetch and the specified exit codes.
- [x] Exercise default denial, matched exact/prefix rules, disallowed host,
      malformed URL/config, effective environment overrides and `--no-env` behavior.
      Existing overlays require TOML leaves to exist: seed real leaves in fixtures,
      not an overlay that silently fails to reach the config consumer.
- [x] Implement the Clap subcommand using `AppConfigArgs` and
      `app_config::load_settings`. Evaluate the same compiled policy as runtime with
      an explicitly hypothetical eligible request context for conditional fields;
      never report actual request consent as granted.
- [x] Implement deterministic JSON and escaped text output. Use `rule_id: null`
      for no match and `identity_eligibility: conditional` only where endpoint and
      fetch policy allow it. A fetch-denied target reports no effective grant even
      if its destination selector would otherwise match.
- [x] Add parity cases comparing CLI decisions to exact runtime hop observations
      for the same endpoint settings. Keep audit output disabled by default; test
      that using the updated template does not create identity grants.

Use the actual host target:

```bash
cargo test --package trusted-server-cli --target aarch64-apple-darwin --test config_proxy
cargo test --package trusted-server-cli --target aarch64-apple-darwin --test config_env_overlay
cargo clippy-cli
```

On Linux substitute the detected Rust host triple; do not compile CLI for WASM.
Run `scripts/test-cli.sh` for complete CLI/browser handoff qualification once
browser prerequisites are available; missing Chrome is a recorded gap.

## Task 7: Prove adapter route wiring and CLI transport preservation

**Files:** integration `tests/parity.rs`, adapter route tests; CLI
`tests/proxy_trusted_server.rs`.
**Spec:** A2, A4, A6, A11–A14, A17, A20.

- [x] Extend the existing three-adapter native parity harness using
      `routes_with_settings_and_services` and a recording PlatformHttpClient. Inject
      geo, canonical Cookie and serialized consent through the real route input.
- [x] Require a distinguishable positive event result through each adapter and
      a denied control; an always-absent EC must fail the positive case. Inspect
      outbound URI, cache intent and final response headers.
- [x] Include strict terminal redirects, matched→static privacy and per-request
      state isolation. Confirm response-state and operator-header enforcement after
      the adapter's normal middleware, not an isolated helper.
- [x] Add Fastly dispatch/finalize coverage using existing internal seams;
      never substitute native parity for actual Fastly terminal behavior.
- [x] Extend the CLI-to-real-Axum fixture with a signed resource request and
      seeded canonical reader cookie. Compare direct and CLI-mediated upstream
      resource URIs and final headers. The CLI does no identity classification;
      disclose any test-only geo/transport injection separately from real runtime.

Run:

```bash
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
cargo test --package trusted-server-cli --target aarch64-apple-darwin --test proxy_trusted_server
cargo test-fastly
```

**Acceptance:** no positive case is accidentally a missing-cookie/missing-geo
fallback, and the development transport does not change runtime policy.

## Task 8: Add bounded Viceroy network and release cache qualification

**Files:** new Fastly runtime module and Node runner from the file map, Fastly
test-module registration, relevant existing test workflow, `scripts/README.md`.
**Spec:** A21, A22 and qualification section.

- [x] Follow the existing `tests/deadline-origin.mjs` pattern: discover the exact
      Cargo test artifact from JSON, create an owned temp configuration and local
      origin, pass origin connection facts through fixture-only inputs, run a
      nonzero exact test set under Viceroy, and terminate all children/origin sockets
      on success, failure or signal. Use an independent outer timeout.
- [x] In the test-only Rust module invoke the real signed core handler with
      real Fastly PlatformHttpClient and the owned HTTP origin. Two canonical reader
      cookies and one signed static URL must yield identical observed upstream URIs
      with no EC. Assert origin observations, not only built request strings.
- [x] Add a real-network redirect that carries `ts-ec` toward a static path and
      an exhausted chain. Observe removal/no raw browser Location. Do not use an
      HTTPS identity rule against a plain HTTP local origin and call that positive
      identity delivery.
- [x] Keep positive HTTPS identity/bypass observations in injected transport
      route tests and separately authorized Fastly staging. Local production-TLS
      qualification is not assumed: no certificate-check disabling, production
      hostname override, or pretend cache implementation to make fixtures pass.
- [x] Add runner self-checks for missing test selection, origin failure, child
      timeout and cleanup. Register the bounded network runner in the existing
      Fastly test CI job with pinned Node/Viceroy.

Planned local command:

```bash
node crates/trusted-server-adapter-fastly/tests/proxy-policy-origin.mjs "$(command -v viceroy)"
```

For separately authorized Fastly staging, document and execute this exact
experiment with operator-provided fictional test URLs/configuration:

1. Deploy the final binary/config to an isolated test service and start an
   origin that exposes per-path request counts outside the proxied responses.
2. Serve a static object with `Cache-Control: public, max-age=300`, no
   Set-Cookie/private directive and controlled identical negotiation/Vary.
3. Request one stable signed URL from two clients with distinct valid EC cookies;
   require correct bytes for both and a single origin fetch. Keep the test in
   one cache locality and record purge/warmup conditions.
4. Serve an HTTPS allowlisted event returning reader-distinct bytes and public
   origin headers. With permitting real request consent/geo, require two origin
   deliveries for repeated requests by A, then B's own identity/bytes. Require
   downstream private/no-store and no validators on every response.
5. Exercise event→static and rule removal, recording actual destinations and
   privacy. Purge or version the fixture between incompatible policy states.

Record binary/config revisions and origin observations. Viceroy's lack of
readthrough caching means its origin count must not be used to certify step 3.
Unavailable staging is an explicit release-qualification gap, not a reason to
claim cache reuse or to deploy without authorization.

## Task 9: Document configuration, migration and evidence boundaries

**Files:** example TOML, proxy/configuration guides, scripts and integration
READMEs; audit generator template assertions if needed.
**Spec:** sections 11–14, 16; A16, A18–A22.

- [x] Add commented fictional identity-endpoint examples, default behavior,
      exact origin/path rules, consent composition, raw-header rejection and reserved
      query behavior. Do not add an active grant to the default template.
- [x] Correct guide diagrams and automatic-forwarding statements, including
      the inaccurate missing-ID generation and redirect assumptions. Update the
      existing configuration claim about old signed URLs still forwarding EC.
- [x] Explain header and terminal-redirect compatibility changes, no automatic
      audit grants, Click preservation, vendor-signature consequences and CLI usage.
- [x] Document the writer/reader matrix, binary-first/config-second activation,
      blue/green option for avoiding identity gaps, config-first rollback cleanup,
      restored legacy behavior on rollback, edge purging and browser-cache limits.
- [x] Document local-network versus real Fastly cache qualification. Keep #982
      as a separate capability dependency, with no CORS promise for this release.

Run with pinned Node:

```bash
cd docs
npm run format
npm run build
```

**Acceptance:** operator instructions describe shipped defaults and limitations;
generated default configuration adds no recipient grants.

## Task 10: Reconcile all acceptance, controls and handoff checks

- [x] Reconcile A1–A22 against exact test names, command, target, input reach,
      result and remaining runtime qualification. Separate scripted transports,
      native route tests, local Viceroy observations and real Fastly evidence.
- [x] In an owned disposable implementation copy, verify meaningful controls:
      unconditional EC, initial-hop-only evaluation, always-absent identity, removed
      bypass, lost chain privacy and late public override. Record which assertion
      rejects each; setup/compilation errors do not count.
- [x] Run the full selected-base AGENTS gate list below after the focused checks.
      Record exact missing tools, unrelated baseline failures and unexecuted gates;
      do not silently waive them or declare whole-PR readiness from a partial pass.
- [x] Request independent implementation review with the spec, actual diff and
      verification evidence. Resolve material concerns and rerun affected checks.
- [x] Prepare a truthful PR description and compatibility/release notes if
      requested. Do not commit/push/create PR/deploy without the relevant authority.
      New authorized commits must be signed and locally verified; GitHub Verified
      is checked only after an authorized push.

### Required implementation gate inventory

Run from root unless a directory is shown. These commands are execution tasks,
not results from authoring these documents. Read live AGENTS again at handoff.

| Gate                           | Commands                                                                                                                                                                                                                                                                                |
| ------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Rust format                    | `cargo fmt --all -- --check`                                                                                                                                                                                                                                                            |
| Adapter lints                  | `cargo clippy-fastly`; `cargo clippy-axum`; `cargo clippy-cloudflare`; `cargo clippy-cloudflare-wasm`; `cargo clippy-spin-native`; `cargo clippy-spin-wasm`                                                                                                                             |
| Host member lints              | `cargo clippy-cli`; `cargo clippy-codegen`                                                                                                                                                                                                                                              |
| Adapter tests                  | `cargo test-fastly`; `cargo test-fastly-reuse`; `cargo test-axum`; `cargo test-cloudflare`; `cargo test-spin`                                                                                                                                                                           |
| Native parity                  | `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`                                                                                                                                                                                           |
| Native build digest            | `cargo test -p trusted-server-core --test template_build_digest`; `cargo clippy -p trusted-server-core --test template_build_digest -- -D warnings`                                                                                                                                     |
| CLI                            | `./scripts/test-cli.sh` with supported installed Chrome for ignored browser cases                                                                                                                                                                                                       |
| JS build/test/format           | In `crates/trusted-server-js/lib`: `node build-all.mjs`; `npx vitest run`; `npm run format`                                                                                                                                                                                             |
| Docs                           | In `docs`: `npm run format`; `npm run build`                                                                                                                                                                                                                                            |
| Other Markdown                 | `docs/node_modules/.bin/prettier --config docs/.prettierrc --check "*.md" ".claude/**/*.md" ".github/**/*.md" "crates/**/*.md" "scripts/**/*.md" "tinybird/**/*.md"`                                                                                                                    |
| Clock                          | `scripts/lint-wasm-clock.sh`                                                                                                                                                                                                                                                            |
| Documentation proposal scripts | `shellcheck scripts/docs-proposal/*.sh`; `node --test scripts/docs-proposal/hunks.test.mjs`; `scripts/docs-proposal/test.sh`                                                                                                                                                            |
| Changed URL hosts              | `cargo run --manifest-path crates/trusted-server-cli/Cargo.toml --target aarch64-apple-darwin --bin ts -- dev lint domains --changed-vs origin/main` (use actual host triple)                                                                                                           |
| CI Node pins                   | `node --test scripts/check-ci-node-pin.test.mjs`; `node scripts/check-ci-node-pin.mjs`                                                                                                                                                                                                  |
| Fastly deadlines               | `node crates/trusted-server-adapter-fastly/tests/deadline-origin.mjs "$(command -v viceroy)"`                                                                                                                                                                                           |
| Cloudflare deadlines           | `cargo build --locked -p trusted-server-adapter-cloudflare --example deadline-runtime --target wasm32-unknown-unknown --features cloudflare`; `node scripts/test-worker-auction-deadlines.mjs cloudflare --runtime "$(command -v wrangler)"`; corresponding fixture clippy per workflow |
| Spin deadlines                 | `cargo build --locked -p trusted-server-adapter-spin --example deadline-runtime --target wasm32-wasip1 --features spin`; `node scripts/test-worker-auction-deadlines.mjs spin --runtime "$(command -v spin)"`; corresponding fixture clippy per workflow                                |
| Release producer               | `node --test scripts/release/phase1.test.mjs`; `shellcheck scripts/release/phase1-build.sh`                                                                                                                                                                                             |
| New creative policy runtime    | Command in task 8; target-matched clippy for the new test-only fixture                                                                                                                                                                                                                  |

Install locked dependencies within the implementation checkout before dependent
checks. Baseline AGENTS requires HTML EOF runtime checks when changing EOF,
publisher or HTML-processor code; this plan leaves those modules unchanged. If
implementation expands into them, add all prescribed EOF checks before handoff.
Cloudflare runtime prerequisites include Wrangler and the lock-matched
wasm-bindgen CLI; Spin requires its pinned runtime. Missing tools remain named
verification gaps. The selected main does not contain the response-diff script
present on a separate cutover line; add its check only if that code is in the
eventual implementation base.

## Completion criteria

The implementation is ready for review only when the default and positive
identity paths, redirect/cache/header/privacy interactions, configuration/CLI
consumer paths and preservation matrix have evidence for the final revision.
Local review readiness and hosted CI are separate from real Fastly cache and
other platform runtime qualification. Report the exact remaining release gaps;
do not equate reviewed documents with an implemented or production-qualified fix.

## Implementation acceptance traceability

`core` below means `crates/trusted-server-core/src`; `parity` means
`crates/trusted-server-integration-tests/tests/parity.rs`. Scripted tests record
`PlatformHttpClient` requests. Native route tests exercise the Axum, Cloudflare,
and Spin routers and final middleware using injected services. Network tests use
an actual local HTTP origin and the Fastly transport under Viceroy.

| Acceptance | Test or source group                                                                                                                                                                                                           | Evidence boundary                                                                                                                                                |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A1         | `core/proxy.rs::proxy_policy_static_readers_share_target`; Fastly `network_two_readers_share_static_origin_uri`                                                                                                                | Canonical cookies and permitting geo-aware context; identical static origin URLs. No cache-hit claim.                                                            |
| A2         | `proxy_policy_identity_delivery_and_chain_privacy`; parity `proxy_policy_routes_deliver_authorized_identity_and_reassert_terminal_privacy`                                                                                     | Scripted and native positive EC, bypass and terminal privacy.                                                                                                    |
| A3         | `proxy_policy_static_redirect_to_event_authorizes_new_destination`; `core/proxy_policy.rs` matcher tests                                                                                                                       | Resource versus explicit event path, independent of extension.                                                                                                   |
| A4         | `proxy_policy_event_without_authorized_identity_still_delivers_uncached`; `identity_policy_disclosure_uses_only_valid_cookie_and_current_consent`; Fastly `proxy_policy_named_route_preserves_withdrawal_and_terminal_privacy` | Missing/invalid cookie and raw-header controls; actual Viceroy KV tombstone and cookie expiry through Fastly finalization.                                       |
| A5         | `core/consent/mod.rs::proxy_disclosure_requires_current_consent_beyond_creation`; existing consent pipeline tests; native GPC control                                                                                          | Pure gate composition and existing conflict, purpose and US opt-out behavior.                                                                                    |
| A6         | Core positive/redirect tests; parity `proxy_policy_event_redirects_and_ambiguous_paths_keep_cache_safety`                                                                                                                      | Static→event, event→static and event→event; sticky privacy and per-hop authorization.                                                                            |
| A7         | Both `proxy_query_*` tests in `core/proxy_query.rs`                                                                                                                                                                            | Reserved-key decoding/removal and unrelated raw-query byte preservation.                                                                                         |
| A8         | `identity_policy_*`; `proxy_policy_rejects_selectors_changed_by_url_serialization`; `proxy_redirect_path_checks_before_url_normalization`; native ambiguous-path test                                                          | Normalization, ports and path boundaries; ambiguous raw paths withhold EC while known event cache protection remains.                                            |
| A9         | `reconstruct_rejects_tampered_tstoken`; `reconstruct_rejects_expired_tsexp`; `proxy_initial_target_blocked_by_allowlist`; native private-error test                                                                            | Signature/fetch rejection precedes dispatch.                                                                                                                     |
| A10        | Native positive route test and core identity/chain test                                                                                                                                                                        | Repeated readers have their own EC destinations; every event bypasses and is private. Actual HTTPS reader-distinct bytes and edge non-reuse remain staging work. |
| A11        | Parity `proxy_policy_event_failures_remain_private_through_adapter_finalization` and private-error test                                                                                                                        | Event→transport failure, gzip rewrite failure and terminal redirect remain private through native finalization.                                                  |
| A12        | `proxy_policy_terminal_redirect_does_not_escape`; native private-error test; Fastly `network_exhausted_redirect_never_returns_raw_location`                                                                                    | Scripted/native rejection and real network exhaustion.                                                                                                           |
| A13        | Native positive route test; `proxy_policy_terminal_privacy_survives_operator_validators`; Fastly withdrawal/finalization test                                                                                                  | Operator and late filter overrides cannot restore public caching or validators.                                                                                  |
| A14        | Core identity/chain and native positive-route tests; existing CORS and asset-response tests                                                                                                                                    | State-setting and CORS grants stripped at the creative boundary.                                                                                                 |
| A15        | Existing HTML/CSS rewrite, gzip and Brotli tests; CLI transport fixture                                                                                                                                                        | Rewrite/compression preservation and actual static HTTP cache-header parity.                                                                                     |
| A16        | Parity `proxy_policy_old_signed_url_obeys_removed_identity_rule`                                                                                                                                                               | Removes a prepared grant and reuses the old signature through each native router.                                                                                |
| A17        | Existing click/rebuild, integration and asset/optimizer/S3 tests                                                                                                                                                               | Explicit identity-disabled helper callers; intentional integration payload identity and S3 signing order preserved.                                              |
| A18        | `proxy_identity_policy_is_prepared_and_never_serialized`; runtime validation and sequence/map overlay tests; CLI typed validation/push test                                                                                    | Typed round-trip, omitted defaults and older-reader shape rejection. This is not an old-binary execution claim.                                                  |
| A19        | Six subprocess tests in `crates/trusted-server-cli/tests/config_proxy.rs`                                                                                                                                                      | Shared policy, overlays, blocked hosts, malformed inputs, stable JSON and redacted output. No destination fetch.                                                 |
| A20        | `build_draft_config_writes_disabled_js_asset_proxy_candidates`; `proxy_policy_cli_transport_preserves_static_fetch_and_cache_headers`                                                                                          | Audit preservation and actual CLI→Axum→HTTP origin comparison with two readers.                                                                                  |
| A21        | Three Fastly `proxy_policy_runtime::network_*` tests; Node origin runner and four runner self-checks                                                                                                                           | Real local HTTP destinations, stripping and exhaustion. Does not establish HTTPS event delivery or readthrough caching.                                          |
| A22        | Isolated Fastly experiment in task 8                                                                                                                                                                                           | Unexecuted release qualification: controlled static origin, two readers, one origin fetch and exact deployed revisions.                                          |

Positive HTTPS delivery through real Fastly, Cloudflare, and Spin transports also
remains release qualification. Native injected transports establish policy and
route behavior; they do not establish production TLS or cache behavior. Hosted CI
results are separate from local verification.

### Regression controls

All seven disposable-copy controls produced assertion failures, rather than
setup or compilation failures. Unconditional identity failed the two-reader
static URL assertion. Initial-hop-only query editing failed echoed-identity
removal. Always-absent identity failed the positive recipient assertion. Missing
bypass failed the event cache-intent assertion. Nonsticky chain privacy failed
the terminal cache policy assertion. Removing late terminal enforcement failed
validator removal; additionally disabling the public-cache override guard failed
the explicit `no-store, private` assertion. The unchanged baseline passed, and
all mutations were restored in the disposable copy. These controls do not modify
the implementation branch or replace real cache qualification.

### Local verification and handoff

The implementation passed all five adapter test aliases and all eight
adapter/host lint aliases, including the reusable Fastly sandbox suite and the
Cloudflare/Spin WASM targets. Native core tests passed 3,408 with two existing
ignored tests; native adapter parity passed 34; the build-digest suite passed 10.
The complete CLI suite and its Chrome fixtures passed. JavaScript passed 3,299
tests across 80 files. Build, formatting, documentation build, clock, Node pins,
release producer, documentation proposal scripts, and added-line URL-host checks
passed. The added-line check uses the staged mode for uncommitted changes.

Local runtime checks passed the five existing Fastly deadline cases, three new
creative-proxy network cases, four origin-runner self-checks, and the Cloudflare
and Spin auction deadline fixtures. Worker runners require the actual Cargo
output directory when it differs from `target`; passing `--target-dir` selects
the isolated artifacts. These deadline fixtures preserve existing runtime
behavior and do not qualify positive HTTPS identity delivery.

Independent implementation reviews have no unresolved actionable findings.
At local implementation handoff, hosted CI, publication and deployment had not
run. The release experiments
above remain open; local verification is not evidence of production cache reuse.
