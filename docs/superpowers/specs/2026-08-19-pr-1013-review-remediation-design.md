# PR #1013 Review Remediation Design

## Goal

Resolve every technically sound actionable finding in the two change-request reviews on PR #1013 without broadening the ESI validation spike or changing unrelated runtime behavior.

## Scope

The remediation covers the two blocking regressions, the terminal-privacy behavior question, all actionable inline hardening and consistency comments, the experimental-configuration clarification, and archival of the two superseded design documents.

Three explicitly separate follow-ups remain outside this PR:

- dependency-version deduplication introduced by the pinned ESI fork;
- gating C2 diagnostic headers before a production rollout;
- upstreaming the ESI parser fixes or publishing a tagged fork release.

Suggestions that do not fit the current runtime architecture will receive a technical response instead of speculative code. In particular, Fastly constructs `AppState` inside a per-request Wasm instance, so a cache attached to that state does not eliminate cross-request fingerprint work. Fingerprint memoization will be added only if investigation identifies a genuinely longer-lived owner that preserves configuration invalidation.

## Approach

Use narrow, test-driven changes grouped by behavioral boundary. Preserve the existing C2 architecture and fail-open/fail-closed contracts. Avoid a general response-policy or template-cache redesign.

### Cache read and storage safety

The Fastly cache adapter will consume a found body through fallible `Read::read_to_end`. A stream read error will become the existing cache miss/error path instead of trapping through the SDK's panicking `into_bytes` helper. The declared body length check remains after the read.

Both transactional and direct inserts will declare `known_length`. The purge-all surrogate key will be exported once from core and consumed by the adapter. Tests will cover a fulfilled reservation not cancelling on drop.

### Encoding and injection preservation

`restrict_accept_encoding` will run for every proxied publisher request, including ESI-mode readers that cannot negotiate an assembly encoding. Its existing identity fallback preserves the main-branch guarantee that the response remains processable and receives TSJS injection.

Regression coverage will exercise malformed or fully refused `Accept-Encoding` input through the relevant request path, not only the helper in isolation.

### Terminal response privacy

The response pipeline will distinguish responses that Trusted Server deliberately stamps `private, no-store` from ordinary origin responses that merely arrived with `private` or `no-store` policy.

A typed response extension will record the former condition at the point where synthesized or assembled HTML is stamped. The Fastly terminal hook will re-enforce `private, no-store` only when that marker is present, after late filter effects. Ordinary proxied responses retain their original cache directives and validators unless another existing policy, such as `Set-Cookie`, independently requires a downgrade.

Tests will pin both directions: late effects cannot make an assembled response public, and an unrelated origin `private, max-age=600` response keeps its browser-cache semantics and validators.

### Publisher-content refusal

C2 authorization will reject documents that already contain the inert TS seam marker rather than rewriting publisher bytes. It will also recognize both case-insensitive `<esi:` directives and `<!--esi` comment blocks.

The directive detector will have one shared implementation used by core authorization and the Fastly parser adapter. Existing last-line assembly validation remains as defense in depth. Tests will cover script-string seam collisions, ordinary ESI tags, ESI comment blocks, and case variants.

### Cache-key and metadata hardening

`TemplateCacheKey::to_cache_key` will normalize Vary names to lowercase at the serialization boundary so public fields cannot silently split keys by header-name case. The existing test will pass a literal uppercase name and verify the normalization.

`TemplateMetadata::encode` will reject or prevent carriage-return/newline-bearing values through a typed or fallible boundary consistent with its callers. The implementation will avoid a production panic. Tests will cover injected line breaks and ordinary round trips.

The stale schema-prefix assertion will derive its prefix from `TEMPLATE_SCHEMA_VERSION`. The default cache trait behavior will explicitly document its unsupported/null-object role or be made explicit on implementations, whichever is smaller after checking all implementors.

### GPT initial scheduling

The TypeScript scheduler and bootstrap fallback will share the same one-shot contract. The first generation-zero scheduling call claims the initial pass; later calls do nothing and cannot register another load/double-rAF chain or overwrite initial state.

Tests will cover duplicate schedule calls, duplicate load events, omitted `initialSlots` preserving existing slots, and an explicit empty array replacing existing slots. The API documentation will retain the durable generation and hydration invariants and remove historical bug narration.

### Focused cleanup

The remediation will:

- remove the unused `RuntimeServices` argument from template storage;
- fold adjacent duplicate implementation blocks together;
- remove the redundant test-only `must_use` and add required assertion messages;
- move the ESI dependency declaration to workspace dependencies;
- make the example Vary set complete and explain fail-closed coverage;
- correct the request `max-age` documentation and label the mode experimental under #1009;
- remove or simplify the single-variant `PageBidsFormat` residue;
- remove the dead cached-response content-encoding branch while preserving the identity invariant;
- repair non-test rustdoc links;
- document the local harness coupling at `build_seam_script`;
- list `PlatformTemplateCache` in the platform module roster and consolidate exports;
- reduce CI timing flake risk without weakening the streaming assertion;
- move the two self-labelled historical documents to `docs/superpowers/archive/` and update all references.

## Error Handling

Runtime failures continue to use the existing concrete error types and `error-stack` boundaries. Cache corruption and backend read failures degrade to origin processing. Invalid publisher content bypasses C2 rather than failing the page. Metadata encoding validation is handled before storage and cannot introduce a new panic path.

## Verification

Each behavioral fix starts with a regression test and runs the narrowest relevant suite immediately. Final verification includes:

- `cargo fmt --all -- --check`;
- all six target-matched clippy aliases;
- `cargo test-fastly`, `cargo test-axum`, `cargo test-cloudflare`, and `cargo test-spin`;
- cross-adapter parity and host CLI tests;
- Fastly template-cache and ESI assembly suites;
- JavaScript Vitest, build, and formatting;
- documentation formatting and `cargo doc --no-deps --all-features` on an appropriate target;
- the C2 local harness or its CI-equivalent commands when local prerequisites are available.

Any unavailable local prerequisite will be reported explicitly rather than represented as a passing check.
