# DataDome IP-excluded client tag suppression — Implementation Plan

> **Status:** Approved for implementation
>
> **For implementers:** Work task by task and keep the workspace buildable.
> Follow `CLAUDE.md`: use target-matched Cargo aliases, do not use bare
> workspace tests, and do not add an internal HTTP header for request state.

**Goal:** When Fastly's authoritative client IP matches a DataDome IP exclusion,
skip the Protection API call and omit only Trusted Server's automatically
injected DataDome client tag from every processed HTML response.

**Issue:** #994
**Design:**
`docs/superpowers/specs/2026-08-03-datadome-ip-excluded-client-tag-design.md`

## Approved behavior

| Request condition                                             | Protection API  | Trusted Server auto-injected tag | Publisher-originated tag |
| ------------------------------------------------------------- | --------------- | -------------------------------- | ------------------------ |
| Inline IP CIDR match                                          | Skipped         | Omitted                          | Unchanged                |
| Config Store IP CIDR-source match                             | Skipped         | Omitted                          | Unchanged                |
| Structured `ip_cidr` match                                    | Skipped         | Omitted                          | Unchanged                |
| Structured `ip_cidr_source` match                             | Skipped         | Omitted                          | Unchanged                |
| ASN, method, path, query, static, or internal-route exclusion | Skipped         | Preserved                        | Unchanged                |
| No exclusion match                                            | Called normally | Preserved                        | Unchanged                |
| Protection API fail-open                                      | Continued       | Preserved                        | Unchanged                |

The Fastly-only scope means that other adapters receive the default
non-suppressed value. Do not add a configuration option and do not modify their
request-filter wiring.

## Runtime contracts

1. **Trusted identity source:** determine exclusion from
   `RuntimeServices::client_info().client_ip`, never a caller-provided header.
2. **Single evaluation:** use the existing `ProtectionScope` decision. Do not
   evaluate CIDRs a second time while injecting HTML; this avoids diverging
   Config Store/cache behavior.
3. **Private marker:** communicate the decision with a typed request extension,
   never a request/response header. The marker cannot leak to the origin or
   client.
4. **Precise scope:** tag suppression is keyed only on decision reasons
   `client_ip`, `client_ip_source`, `ip_cidr`, and `ip_cidr_source`.
5. **Cache safety:** an HTML response with the tag omitted differs by client IP.
   A suppressed processed HTML response must be `private, max-age=0` and have
   `Surrogate-Control` and `Fastly-Surrogate-Control` removed. Do not alter
   cache headers when the response is not processed HTML, because this feature
   does not alter that body.
6. **No behavior drift:** DataDome proxy endpoints, response-header effects,
   `rewrite_sdk`, and DataDome tags that were already in origin HTML retain
   their current behavior.

## File map

| File                                                                          | Change                                                                                                                                                  |
| ----------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/integrations/registry.rs`                     | Permit filters to attach private typed request extensions while retaining header-effect semantics. Extend the HTML context with the propagated boolean. |
| `crates/trusted-server-core/src/integrations/datadome.rs`                     | Define the crate-private marker and have the head injector honor the HTML-context flag.                                                                 |
| `crates/trusted-server-core/src/integrations/datadome/protection.rs`          | Recognize IP scope skips, attach the marker, and add `client_tag=omitted` to the existing info log.                                                     |
| `crates/trusted-server-core/src/html_processor.rs`                            | Carry the per-response suppression boolean from config to all integration HTML contexts.                                                                |
| `crates/trusted-server-core/src/publisher.rs`                                 | Snapshot the marker before origin dispatch, propagate it through every HTML streaming path, and apply cache privacy to suppressed processed HTML.       |
| `docs/guide/integrations/datadome.md`                                         | Document the Fastly IP-exclusion behavior and its limits.                                                                                               |
| `docs/superpowers/specs/2026-08-03-datadome-ip-excluded-client-tag-design.md` | Already updated with the cache-variance safeguard.                                                                                                      |

No changes are expected in `trusted-server.example.toml`, JavaScript bundles,
or non-Fastly adapters.

---

## Task 1: Make the request-filter input capable of private annotations

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/registry.rs`
- Test: its existing `#[cfg(test)]` module

The current `RequestFilterInput` holds `&Request<EdgeBody>`. Change it to hold
`&mut Request<EdgeBody>` so a request filter can add a typed extension. This is
the narrowest safe transport because the registry already has exclusive mutable
access to the request while it invokes each filter.

- [ ] **Step 1: Add a regression test for an extension-producing filter.** Create
      a test-only zero-sized marker and filter that writes it to
      `input.request.extensions_mut()`. Run `IntegrationRegistry::filter_request`
      and assert the original mutable request has the marker afterward. In the
      same test, verify normal `RequestFilterEffects` still apply their request
      header mutation and return their response header mutation.
- [ ] **Step 2: Change `RequestFilterInput::request` to a mutable borrow.** Keep
      the `IntegrationRequestFilter` method signature and `RequestFilterEffects`
      unchanged.
- [ ] **Step 3: Update `IntegrationRegistry::filter_request`.** Pass its existing
      `&mut Request` directly to each `RequestFilterInput`. Keep the ordering:
      filter mutation first, then registry-applied request-header effects, then
      the next filter.
- [ ] **Step 4: Update all direct filter tests and test filters.** Calls that build
      `RequestFilterInput` must construct a mutable request and pass
      `request: &mut request`. Read-only filters should continue to compile by
      simply not mutating the request.
- [ ] **Step 5: Run focused tests.**

```bash
cargo test-fastly integrations::registry
```

**Acceptance:** a filter can retain a typed marker for downstream route handling
without emitting a synthetic `x-*` header, and existing header effects retain
their behavior.

---

## Task 2: Mark IP-based DataDome exclusions and log the outcome

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/datadome.rs`
- Modify: `crates/trusted-server-core/src/integrations/datadome/protection.rs`
- Test: `crates/trusted-server-core/src/integrations/datadome/protection.rs`
- Reuse: `crates/trusted-server-core/src/integrations/datadome/protection_scope.rs`

- [ ] **Step 1: Add a crate-private marker in `datadome.rs`.** Define a
      zero-sized type with a behavior-oriented name, such as
      `DataDomeClientTagSuppressed`. It must be visible to `publisher.rs` and
      `protection.rs` through `pub(crate)`, but must not be exported as public
      integration configuration or API.
- [ ] **Step 2: Add an IP-reason predicate beside protection logging.** Centralize
      the exact four eligible scope reasons in one helper:

```rust
matches!(reason, "client_ip" | "client_ip_source" | "ip_cidr" | "ip_cidr_source")
```

      Do not infer eligibility from rule ID: Config Store source rule IDs are
      operator-configured strings.

- [ ] **Step 3: Make `filter_protection_request` own a mutable input and pass it
      mutably to `is_request_protected`.** In the existing
      `ProtectionScopeDecision::Skip` arm:

  1. determine whether the reason is IP-based;
  2. if so, insert the typed marker into `input.request.extensions_mut()`;
  3. call the updated skip logger with `client_tag_omitted = true`; and
  4. return `false` exactly as today so the Protection API is not called.

     Do not set the marker for the early method/integration/internal-route
     returns. Do not set it when the API call returns a fail-open error.

- [ ] **Step 4: Update `log_protection_skip`.** Keep IP exclusions at `info` and
      non-IP exclusions at `debug`. For the IP branch, extend the existing
      structured text after the reason with `client_tag=omitted`; retain rule,
      reason, method, host, and path, but do not include the client IP. The
      desired shape is:

```text
[datadome] protection decision=skipped rule=excluded-ip-cidrs reason=client_ip client_tag=omitted method=GET
```

- [ ] **Step 5: Add filter-level marker tests.** Add small helpers in the
      protection test module to build `RuntimeServices` with a fixed client IP,
      optional Config Store data, and a mutable request. For each case, call
      `filter_protection_request`, assert it returns `Continue`, and inspect the
      request extension:

  - inline `protection_excluded_ip_cidrs` match → marker present;
  - `protection_excluded_ip_cidr_sources` match → marker present;
  - structured `ProtectionMatcherConfig::IpCidr` match → marker present;
  - structured `ProtectionMatcherConfig::IpCidrSource` match → marker present.

    Clear the process-global CIDR-source test cache before and after source
    tests so cached values cannot affect another case.

- [ ] **Step 6: Add negative filter-level tests.** Assert the marker is absent
      for a non-matching IP, a configured ASN match, a structured path match,
      a structured query match, an excluded method, and an internal/integration
      route. Reuse the existing `ProtectionScope` unit tests for matching
      semantics; these new tests verify only the new side effect.
- [ ] **Step 7: Preserve API-call behavior.** For an IP marker test, use an HTTP
      client double that records calls or errors if called. Assert no Protection
      API request is sent. This protects against accidentally marking a request
      while still invoking DataDome.
- [ ] **Step 8: Run focused tests.**

```bash
cargo test-fastly datadome::protection
cargo test-fastly datadome::protection_scope
```

**Acceptance:** only the four IP decision reasons add the private marker and
produce the augmented informational skip log; all other exclusion and fail-open
paths keep their current tag behavior.

---

## Task 3: Thread suppression through publisher response processing

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/html_processor.rs`
- Modify: `crates/trusted-server-core/src/integrations/registry.rs`
- Test: `publisher.rs` and `html_processor.rs` test modules

### Data flow to implement

```text
DataDomeClientTagSuppressed request extension
  -> bool captured by handle_publisher_request before origin dispatch
  -> OwnedProcessResponseParams
  -> ProcessResponseParams / HtmlStreamProcessorParams
  -> HtmlProcessorConfig
  -> IntegrationHtmlContext
  -> DataDomeIntegration::head_inserts
```

- [ ] **Step 1: Capture the marker once in `handle_publisher_request`.** Read
      `req.extensions().get::<DataDomeClientTagSuppressed>().is_some()` before
      `req` is rewritten and moved into `PlatformHttpRequest`. Store the boolean
      only in the `PublisherResponse::Stream` parameters, because that is the
      only response route that passes through HTML injection.
- [ ] **Step 2: Add a boolean to the owned and borrowed publisher-processing
      parameter structs.** Add a clearly named field such as
      `suppress_datadome_client_side_tag` to:

  - `OwnedProcessResponseParams`;
  - `ProcessResponseParams`; and
  - `HtmlStreamProcessorParams`.

    Pass it through all three existing HTML construction sites:

  - `PublisherBodyProcessor::new` for async buffered processing;
  - `process_response_streaming` for synchronous processing; and
  - `stream_publisher_body_async` for the Fastly streaming auction-hold path.

    Every test fixture that constructs `OwnedProcessResponseParams` directly
    must set `false` unless it explicitly exercises suppression.

- [ ] **Step 3: Extend `HtmlProcessorConfig`.** Add the same boolean, default it
      to `false` in `from_settings`, and add a narrow builder method used by
      `create_html_stream_processor`. Update direct `HtmlProcessorConfig`
      fixtures and the benchmark fixture to set `false` explicitly.
- [ ] **Step 4: Extend `IntegrationHtmlContext`.** Add the boolean as immutable
      request-scoped context. Populate it at both construction sites in
      `html_processor.rs`:

  - the streaming `<head>` element handler; and
  - `HtmlWithPostProcessing::process_chunk` for full-document post-processors.

    Update every test helper that constructs `IntegrationHtmlContext` to set
    `false` by default.

- [ ] **Step 5: Add plumbing tests.**

  - `HtmlProcessorConfig::from_settings` defaults to non-suppressed.
  - A test head injector records the context flag and sees `true` when a config
    is built with suppression.
  - A `publisher.rs` route test inserts the DataDome marker into a request,
    receives a processable HTML `PublisherResponse::Stream`, and verifies the
    owned parameters carry `true`.
  - A buffered and a streaming-body path both preserve `true` to head injection.

- [ ] **Step 6: Run focused tests.**

```bash
cargo test-fastly html_processor
cargo test-fastly publisher
```

**Acceptance:** the decision is read once from a private request extension and
is available to every head injector for every processed HTML response, including
Fastly's streaming path.

---

## Task 4: Omit only Trusted Server's injected DataDome tag

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/datadome.rs`
- Test: `crates/trusted-server-core/src/integrations/datadome.rs`
- Test: `crates/trusted-server-core/src/html_processor.rs` or `publisher.rs`

- [ ] **Step 1: Add a direct head-injector regression test.** With a client-side
      key configured and `ctx.suppress_datadome_client_side_tag = true`, assert
      `head_inserts()` returns an empty vector. The same config with `false`
      must still return exactly one snippet containing both `window.ddjskey` and
      the configured tag URL.
- [ ] **Step 2: Implement the guard as the first condition in
      `DataDomeIntegration::head_inserts`.** Return an empty vector when the
      context flag is true; otherwise retain all current serialization,
      escaping, blank-key, and `inject_client_side_tag` behavior unchanged.
- [ ] **Step 3: Add an end-to-end HTML pipeline test.** Configure the DataDome
      integration with a client-side key, process representative HTML with
      suppression enabled, and assert the result contains neither:

```text
window.ddjskey=
/integrations/datadome/tags.js
```

      Repeat with suppression disabled and assert both appear.

- [ ] **Step 4: Pin publisher-originated-tag behavior.** Feed origin HTML that
      contains a DataDome `tags.js` element. With suppression enabled, assert
      that element remains in output and is rewritten by `rewrite_sdk` exactly
      as before. This distinguishes automatic injection from origin markup.
- [ ] **Step 5: Pin direct route behavior.** Retain or add a DataDome proxy test
      showing that `GET /integrations/datadome/tags.js` remains registered and
      fetches/proxies the SDK normally; suppression affects only HTML injection.
- [ ] **Step 6: Run focused tests.**

```bash
cargo test-fastly datadome
```

**Acceptance:** suppression removes only the generated configuration/script
pair; nothing removes publisher markup or disables DataDome endpoints.

---

## Task 5: Make tag-suppressed processed HTML private

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Test: `crates/trusted-server-core/src/publisher.rs`

The automatic tag makes the processed HTML vary by client IP. Cache privacy is
therefore a correctness and protection requirement, not an optional
optimization.

- [ ] **Step 1: Add a failing cache-privacy test.** Build a `PublisherResponse`
      with a processable HTML content type, suppression `true`, and cacheable
      origin headers (`Cache-Control`, `Surrogate-Control`, and
      `Fastly-Surrogate-Control`). Assert the stream response is:

  - `Cache-Control: private, max-age=0`; and
  - missing both surrogate cache headers.

- [ ] **Step 2: Apply privacy only in the `ResponseRoute::Stream` HTML arm.**
      After response classification confirms a processable HTML stream, use the
      existing per-user ad-stack policy as the model. Do not alter cache headers
      for CSS, RSC, non-processable pass-through, unsupported encodings, HEAD,
      204/205/304, or responses without suppression: none has a body variation
      created by this feature.
- [ ] **Step 3: Add non-regression cache tests.** Verify that:

  - non-suppressed processed HTML keeps its existing cache headers unless
    another existing policy changes them;
  - a suppressed CSS/non-HTML stream is not made private by this feature; and
  - existing ad-stack privacy behavior remains unchanged when both features are
    active.

- [ ] **Step 4: Run focused tests.**

```bash
cargo test-fastly publisher
```

**Acceptance:** a shared cache cannot replay an IP-excluded client's tagless
HTML to a non-excluded visitor, while unchanged responses retain their existing
cacheability.

---

## Task 6: Add Fastly-path regression coverage

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/app.rs` tests only if the
  existing dispatch helpers can exercise the DataDome registry with a stubbed
  publisher response.
- Otherwise, document the existing core filter + publisher pipeline tests as
  the executable behavioral coverage; do not refactor Fastly production code
  merely to enable a duplicate test.

- [ ] **Step 1: Extend the existing Fastly request-filter dispatch regression
      test or add a focused equivalent.** Configure a DataDome request filter,
      insert trusted `ClientInfo` into the request extensions with a matching
      IP, and confirm the filter runs before publisher routing.
- [ ] **Step 2: Assert that the routed request retains the private DataDome
      marker.** The assertion must inspect request extensions or the processed
      HTML result, not an HTTP header.
- [ ] **Step 3: Ensure no actual DataDome API call occurs for the matching IP.**
      Use a recording/failing HTTP client or the existing Fastly test seam.
- [ ] **Step 4: Add the non-matching counterpart.** It must not receive the
      marker and must continue to inject the configured tag when HTML is
      processed.
- [ ] **Step 5: Run Fastly adapter tests.**

```bash
cargo test-fastly
```

**Acceptance:** the production adapter's actual filter ordering preserves the
marker from authoritative Fastly client metadata through publisher HTML
processing. If the current test seam cannot stub a full origin response, retain
this as focused request-filter-order coverage and rely on Task 3's core
pipeline tests for body output rather than expanding adapter production code.

---

## Task 7: Document operator-visible behavior

**Files:**

- Modify: `docs/guide/integrations/datadome.md`
- Do not modify: `trusted-server.example.toml`

- [ ] **Step 1: Add a subsection adjacent to “Protected traffic” or “Client-side
      setup.”** State that, on Fastly, an IP exclusion skips the Protection API
      and suppresses only Trusted Server's automatic DataDome tag injection on
      processed HTML.
- [ ] **Step 2: List the four covered IP sources.** Use the exact configuration
      names and structured rule types.
- [ ] **Step 3: State the exclusions that do not suppress the client tag.** ASN,
      method, path, query, static-asset, and internal-route exclusions retain
      normal auto-injection.
- [ ] **Step 4: State the limits.** Publisher-originated/manual tags are not
      removed; `/integrations/datadome/tags.js` remains available; no new
      configuration is required; and tag-suppressed processed HTML is private
      to prevent shared-cache replay.
- [ ] **Step 5: Add the diagnostic example.** Use an example-only host/IP and
      include `client_tag=omitted` with rule and reason.
- [ ] **Step 6: Format-check the changed documentation.**

```bash
cd docs
npx prettier --check guide/integrations/datadome.md \
  superpowers/specs/2026-08-03-datadome-ip-excluded-client-tag-design.md \
  superpowers/plans/2026-08-03-datadome-ip-excluded-client-tag.md
```

**Acceptance:** operators can predict exactly when the tag will be omitted and
understand that this is an IP-based Fastly behavior, not a general exclusion
side effect.

---

## Final verification

- [ ] Confirm the working tree contains only the intended core, Fastly-test,
      guide, spec, and plan changes.
- [ ] Run formatting.

```bash
cargo fmt --all -- --check
```

- [ ] Run the relevant target-matched test suites.

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
```

- [ ] Run required lint suites.

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
```

- [ ] Run the docs check from Task 7.
- [ ] Review the diff for accidental exposure of the marker as a request or
      response header, duplicate CIDR evaluation, unintended publisher-tag
      removal, or shared-cacheable tag-suppressed HTML.

## Deferred acceptance

Do **not** perform live production/browser verification in this change. After
deployment, the separate testing workflow should verify that a matching
whitelisted IP receives processed HTML without Trusted Server's
`/integrations/datadome/tags.js` injection, while an unlisted IP retains it.
