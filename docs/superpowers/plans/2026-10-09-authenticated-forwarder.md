# Authenticated forwarder implementation plan

> For agentic workers: use `superpowers:subagent-driven-development` for independent implementation and review tasks. Review each completed task for requirements and code quality before moving on.

**Goal:** Let the development proxy preserve browser Origin and authenticate the browser-facing host and scheme, with the server changes in #1107 and CLI changes in #1251.

**Architecture:** Preserve immutable runtime `RequestIngress` transport facts. Capture a separate validated public origin before trace pre-dispatch and forwarded-header sanitation. Share this origin between trace authorization and request information used to construct public URLs. Production behavior remains unchanged when the optional configuration is absent.

**Tech stack:** Rust, HTTP request extensions, existing secret-store envelopes, constant-time digest comparison, Fastly/Axum/Cloudflare/Spin, native CLI proxy and browser integration fixtures.

**Wire contract:** The CLI uses `x-ts-forwarder-auth`; configure the server's optional authentication header to that name when using this CLI. The forwarded sources are `x-forwarded-host` and `x-forwarded-proto`. Secrets contain at least 32 ASCII graphic bytes; a file may end in a single line terminator. Only one field of each kind is accepted, with no lists, whitespace, userinfo or URL suffixes. Schemes are HTTP/HTTPS and origins canonicalize hostname case and default ports. Invalid forwarding falls back to transport facts. Authentication headers cannot overlap forwarded sources, trace controls or configured client-IP trust headers.

**Adapter seam:** Preparation is the first operation of the existing trace pre-dispatch hook, which is registered by every adapter. Fastly captures a typed preparation outcome before its existing native sanitation and inserts that outcome after conversion. Preparation is idempotent and contains no secret material. Startup-error routers remain local-only.

**Authority and evidence:** Credential stamping requires a single valid inbound Host, or an unambiguous absolute URI authority, with agreement when both exist. Never authenticate a guessed CONNECT fallback. Preserve browser ports. Forwarding must not upgrade cookie health, request-target provenance or header fidelity.

## Task 1: Server configuration and origin contract (#1107)

Files: core `settings.rs`, `config.rs`, `config_payload.rs`, `http_util.rs`, a focused `forwarder.rs` module, and `lib.rs`.

- [x] Add failing tests for optional/omitted configuration, redacted secrets, secret-store resolution, placeholder/length validation and unsafe/colliding authentication names.
- [x] Implement optional `TrustedForwarderConfig` using the existing trusted-client-IP secret conventions.
- [x] Add failing tests for authenticated public origin: single auth/host/proto fields; valid publisher domain/subdomains and explicit ports; duplicate/list/malformed/foreign values; default fallback; removal of authentication fields regardless of acceptance.
- [x] Implement a typed authenticated public-origin extension and strict canonical parsing. Compare fixed-size secret digests in constant time. Do not mutate `RequestIngress` or use public forwarding to manufacture target/header fidelity.
- [x] Make `RequestInfo` read the authenticated public-origin extension first.
- [x] Run native core tests and independent requirements/code-quality review; fix findings.

## Task 2: Adapter entry and trace authorization (#1107)

Files: four adapters' entry points/hooks/middleware, core `trace/actions.rs` and `trace/dispatch.rs`, adapter/parity fixtures.

- [x] Write regression tests for real trace enable/end over rewritten Host and plaintext transport with authenticated forwarding. Cover invalid/missing/duplicate auth and Origin, foreign origin, query strings and empty-body/action/Fetch Metadata controls.
- [x] Resolve forwarding before trace pre-dispatch and before any sanitizer removes inputs in every adapter; remove the configured secret before routing/forwarding, including rejected requests.
- [x] Check received URI/Host consistency against transport ingress independently of browser Origin, which must match the effective public origin.
- [x] Keep trace transport-cookie/header-fidelity rules intact.
- [x] Run target-matched tests and independent review; fix findings.

## Task 3: Public-origin consumers and documentation (#1107)

Files determined by complete consumer audit: core publisher/proxy/integrations/HTML paths, configuration guide/example, trace design specification.

- [x] Classify each URI/Host/scheme consumer as public-origin or transport-sensitive. Add behavioral regression tests before changing public-origin consumers (including protocol-relative signing and redirects).
- [x] Use the shared effective origin for generated public URLs and public-scheme decisions. Keep actual upstream routing and ingress evidence tied to transport.
- [x] Document opt-in trust, host bounds, secret-store provisioning, unchanged defaults and rejection/fallback rules. Amend trace specification's forwarded-header prohibition to permit only authenticated forwarding.
- [x] Run relevant core/integration tests and independent review; fix findings.

## Task 4: Proxy credentials and unchanged Origin (#1251)

Files: CLI proxy `mod.rs`, `config.rs`, `rewrite.rs`, `server.rs`, E2E support/tests, proxy guide and design specification.

- [x] Write failing tests for `--forwarder-secret-file`, valid/invalid file contents, redacted/sensitive header values and non-loopback credential rejection.
- [x] Add file-only, prevalidated forwarder credentials. Remove incoming forwarder authentication on mapped traffic and stamp the configured token after hop-by-hop sanitation.
- [x] Preserve and validate the inbound browser authority, including its port, when stamping authenticated forwarding; reject ambiguity rather than authenticating guessed metadata.
- [x] Remove route-specific Origin rewriting and `RewriteOutcome::upstream_origin`. Preserve all browser Origin fields unchanged.
- [x] Replace echo tests with meaningful forwarding contract coverage, including auth overwrite/absence, plaintext/TLS, Host rewriting, ports, near-miss routes and connection reuse.
- [x] Update guide/header tables/specification; remove obsolete caveats about Origin rewriting and document server dependency.
- [x] Run CLI tests/lint and independent requirements/code-quality review; fix findings.

## Task 5: Joint verification and PR updates

- [x] Run real proxy-to-server trace and public-URL tests using both branches: TLS/plaintext, rewritten/preserved Host, public non-default ports, valid auth and hostile/ambiguous headers. Verify enable/end cookies, identify CORS, vendor Origin preservation, auth stripping and signing scheme.
- [x] Run the repository's complete CI gate list on the server branch and the CLI branch, plus production adapter builds. Record failures with evidence and distinguish environmental issues from regressions.
- [x] Obtain independent final reviews of both branch diffs and their combined behavior, resolving actionable findings.
- Final handoff: commit tested changes using repository conventions and push each PR branch without force. Refresh PR descriptions to explain the final contract and dependency.
- Final handoff: recheck remote heads and CI, and report actual validation results and any remaining limitations.

## Reviewed corrections

- Independent configuration review found a collision with the DataDome bypass header; runtime and deployment validation now reject it, including mixed-case names and unresolved key references.
- Origin review caught paired ingress facts being mixed with mutable Host; the resolver now keeps immutable authority and scheme together.
- CLI wire review found authentication leaking through streaming trailers; both declarations and actual frames are filtered while benign trailers remain intact.
- Full core verification found a cache test fixture relying on unauthenticated forwarded HTTPS; the fixture now supplies explicit runtime ingress.
- Cross-surface review found Spin losing HTTPS when its original URI is path-only. A separate validated runtime-origin extension supplies public URL fallback and survives snapshots without granting trace or fidelity trust.
- Operator trace guidance now documents the opt-in offload arrangement, and an actual Fastly router regression keeps configured identify CORS independent of the forwarded public authority.

## Final local verification

Both branch trees passed all required CI gates, target-matched adapter builds, Fastly/Spin release builds and core documentation. Native core: 3,057 passed; cross-adapter parity: 27 passed; JS: 1,757 passed. Final stacked CLI: 731 unit tests, 41 proxy wire tests and three real-server regressions passed, plus its remaining integration and documentation suites. CLI lint and formatting passed again after explicit TLS provider setup was added to test fixtures. A serial proxy unit run passed 111 tests, and an isolated TLS wire regression passed in a fresh process.

The real operator `ts config validate` command accepted the optional forwarder section with a secret-store key reference. A shared-target artifact interruption in the server CLI documentation run cleared on a sequential full-suite rerun. Independent reviews approved every implementation and corrective pass, including the runtime-origin fallback, streaming trailer removal, identify CORS and joint listener fixture.

GitHub CI and the existing deployed/mobile release acceptance are tracked in the PR descriptions; local wire tests do not replace physical mobile, browser session restoration or operator staging acceptance.

The new CodeQL password-hashing alert #204 was independently reviewed and classified as a false positive: SHA-256 normalizes temporary bearer-token digests for comparison; no verifier digest is stored or exposed. The operator guide explicitly requires random-token generation and distinguishes length from entropy.
