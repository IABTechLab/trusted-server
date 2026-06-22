# Osano Consent Mirror for Edge Cookie Generation

**Issue:** #772
**Date:** 2026-06-17
**Status:** Implemented

## Problem

Edge Cookie (EC) generation and withdrawal depend on request-visible consent
signals. Trusted Server currently reads these standard signals from incoming
requests:

- `euconsent-v2`
- `__gpp`
- `__gpp_sid`
- `us_privacy`
- `Sec-GPC`

Osano exposes consent in the browser through `window.Osano.cm` and the IAB APIs
(`__tcfapi`, `__uspapi`, `__gpp`). In observed Osano deployments, the CMP stores
its durable consent state in Osano-managed browser storage and a CMP UUID cookie,
but it does not necessarily write the standard cookies Trusted Server reads.

This creates a transport gap: browser JavaScript can see the user's consent
choice, but the edge request does not carry that choice. In regulated
jurisdictions, Trusted Server correctly fails closed and skips EC creation. That
means EC may remain disabled even after a user accepts permitted storage/identity
use. Conversely, explicit opt-out choices must reach the edge so Trusted Server
can expire an existing EC cookie and write withdrawal tombstones.

## Goals

- Add an explicitly-enabled Osano client-side consent mirror modeled after the
  existing Sourcepoint mirror pattern.
- Use this work to refactor Sourcepoint so CMP consent mirrors are opt-in via
  integration configuration rather than always shipped.
- Translate Osano/IAB browser API output into standard first-party cookies that
  Trusted Server already understands.
- Avoid server-side consent-gating changes by reusing the existing consent
  extraction and EC gating pipeline.
- Preserve cookies written by another CMP unless Trusted Server knows its Osano
  mirror owns them.
- Keep behavior fail-safe: never fabricate consent when Osano or an IAB API is
  unavailable or not ready.

## Non-goals

- No Rust-side Osano proxy in v1.
- No parsing or reverse-engineering of Osano's opaque `osano_consentmanager`
  storage payload.
- No new server-side consent framework or EC gating semantics.
- No changes to how `allows_ec_creation()` interprets TCF, GPP, US Privacy, or
  GPC.
- No persistent consent storage in Trusted Server KV.
- No customer-specific configuration, domains, IDs, or test fixtures.

## Approach

Build a JS-only `osano` integration that runs in the browser, detects Osano, and
mirrors IAB-compatible consent values into first-party standard cookies for the
next request.

The first page request after a new consent choice cannot use the mirrored values,
because that request has already reached the edge before browser JavaScript runs.
The mirror enables the next page view, auction, integration request, or other
eligible request to carry the consent state to Trusted Server.

## Design

### 1. New JS integration module

Add a new module:

```text
crates/js/lib/src/integrations/osano/index.ts
crates/js/lib/test/integrations/osano/index.test.ts
```

The module is JS-only but should be explicitly enabled through integration
configuration. Unlike the current Sourcepoint mirror behavior, `osano` should not
be included in the always-shipped JS module list.

Refactor Sourcepoint as part of this work so consent mirror modules are included
through normal integration/module configuration rather than unconditional
`JS_ALWAYS` behavior. The implementation should preserve Sourcepoint's current
runtime behavior for deployments that enable it, but make that enablement
explicit.

Suggested runtime behavior:

1. If `window`/`document` are unavailable, do nothing.
2. If `window.Osano?.cm` is present, register listeners and mirror immediately.
3. If Osano is not present yet, perform bounded retries because TSJS may run
   before the CMP script on some pages.
4. On Osano lifecycle events, schedule a debounced mirror attempt.
5. On `visibilitychange` and `focus`, refresh mirrored cookies to avoid stale
   session cookies after mid-session consent changes.

### 2. Osano event hooks

Use public Osano CMP events exposed through `window.Osano.cm.addEventListener`:

- `osano-cm-initialized`
- `osano-cm-consent-saved`
- `osano-cm-consent-new`
- `osano-cm-consent-changed`
- `osano-cm-opt-out`
- `osano-cm-storage`

`osano-cm-consent-saved` is especially important because Osano calls the listener
immediately when consent has already been saved, which covers returning visitors.

The event callback payload is useful for diagnostics, but the mirror should read
canonical IAB API outputs rather than infer legal signals directly from Osano's
category object.

### 3. Consent extraction from browser APIs

Read from IAB APIs when available.

#### US Privacy

Call:

```ts
window.__uspapi('getUSPData', 1, callback)
```

If the callback succeeds and returns a non-empty `uspString`, mirror it to:

```text
us_privacy=<uspString>
```

Observed examples:

- No sale opt-out: `1YN-`
- Sale opt-out: `1YY-`

Trusted Server already interprets `us_privacy` in US-state jurisdictions:

- opt-out sale `Y` blocks EC and can withdraw an existing EC
- opt-out sale `N` allows EC when no stronger opt-out signal is present

#### GPP

Call:

```ts
window.__gpp('ping', callback)
```

Only mirror GPP when:

- callback succeeds
- `signalStatus === 'ready'`
- `gppString` is non-empty

Write:

```text
__gpp=<gppString>
__gpp_sid=<comma-separated applicableSections>
```

Do not write `__gpp_sid` when `applicableSections` is empty or `[-1]`.

This is primarily pass-through for downstream compatibility and for deployments
where Osano includes TCF/GPP sections. For US Privacy section 6, the Osano mirror
also writes `us_privacy`, so no server-side GPP USP decoding is required for EC
in v1.

#### TCF

Call:

```ts
window.__tcfapi('getTCData', 2, callback)
```

If the callback succeeds and returns a non-empty `tcString`, mirror it to:

```text
euconsent-v2=<tcString>
```

Do not synthesize a TC string from Osano's `STORAGE` category. For GDPR/TCF,
Trusted Server should continue to rely on a real TC string and existing TCF
Purpose 1 decoding.

### 4. Cookie ownership marker

The Osano mirror must not clobber consent cookies written by another CMP or
publisher script. Use a single source marker cookie to track Trusted
Server-owned Osano writes:

```text
_ts_consent_src=osano
```

Ownership rules:

- If all target standard consent cookies are absent, the Osano mirror may write
  mirrored values and set `_ts_consent_src=osano`.
- If `_ts_consent_src=osano`, the Osano mirror may update or clear the standard
  consent cookies it manages: `us_privacy`, `__gpp`, `__gpp_sid`, and
  `euconsent-v2`.
- If `_ts_consent_src` exists and is not `osano`, preserve existing standard
  consent cookies and log a debug message.
- If any target standard consent cookie exists but `_ts_consent_src` is absent,
  preserve existing cookies and log a debug message. This protects unknown
  external writers.
- Clear stale Osano-owned cookies only after Osano is initialized and the
  relevant IAB API definitively reports no usable value. Do not clear cookies
  merely because Osano has not loaded yet.

This intentionally assumes one active CMP integration per site. Multiple active
CMPs on a single publisher site are treated as an unsupported/misconfigured edge
case for v1. A future version can split this into per-signal ownership markers if
real deployments need mixed ownership of `us_privacy`, `__gpp`, and
`euconsent-v2`.

### 5. Cookie attributes

Write mirrored cookies as session cookies:

```text
Path=/; Secure; SameSite=Lax
```

Use session scope because Osano remains the source of truth. The integration
re-mirrors on page load, consent events, focus, and visibility refresh.

Cookie values should be written raw, not URL-encoded, because Trusted Server's
server-side decoders expect the standard string values as-is.

### 6. Runtime sequencing

Recommended mirror loop:

1. `initializeOsanoConsentMirror()` guards against double initialization.
2. Install Osano listeners once Osano is present.
3. Schedule `mirrorOsanoConsent()` with a short debounce after relevant events.
4. `mirrorOsanoConsent()` attempts USP, GPP, and TCF independently.
5. API reads are independent so a ready USP value can still mirror while GPP or
   TCF is unavailable. Cookie writes use the shared `_ts_consent_src` ownership
   marker described above.
6. If an API is missing or not ready, leave existing cookies alone. Startup
   retries are bounded to Osano listener discovery; focus and visibility events
   also refresh the mirror later in the session.

Independent API reads matter because Osano may expose US Privacy before GPP is
ready, or TCF only in GDPR jurisdictions.

## Files touched

| File                                                      | Change                                                                    |
| --------------------------------------------------------- | ------------------------------------------------------------------------- |
| `crates/js/lib/src/integrations/osano/index.ts`           | New JS-only Osano consent mirror                                          |
| `crates/js/lib/test/integrations/osano/index.test.ts`     | New Vitest coverage for mirroring and ownership rules                     |
| `crates/trusted-server-core/src/integrations/osano.rs`    | New minimal integration config/registration for explicit JS enablement    |
| `crates/trusted-server-core/src/integrations/registry.rs` | Remove consent mirrors from unconditional `JS_ALWAYS`; include via config |
| `trusted-server.toml`                                     | Document disabled-by-default Osano integration block                      |
| `crates/js/lib/src/integrations/*` build output           | Generated bundle output changes via existing JS build pipeline            |

No changes are expected in Rust consent decoding or EC gating.

## Testing

### JS unit tests

Add Vitest tests for:

- no-op when Osano is unavailable
- bounded retry when Osano appears after TSJS initialization
- mirrors `us_privacy` from successful `__uspapi('getUSPData')`
- mirrors opt-out `us_privacy` values without changing semantics
- mirrors `__gpp` and `__gpp_sid` only when GPP `signalStatus` is `ready`
- does not mirror GPP while signal status is `not ready`
- mirrors `euconsent-v2` from successful `__tcfapi('getTCData')`
- preserves pre-existing cookies when Osano marker is absent
- updates/clears cookies when the corresponding Osano marker is present
- refreshes mirrored cookies on `osano-cm-consent-saved` and returning consent
- handles callback failure, timeout, malformed callback payloads, and missing APIs

Use example values only. Do not use real publisher domains, customer IDs, or
production CMP configuration IDs in tests.

### Local/manual verification

Use a local HTML fixture or controlled test page that stubs Osano/IAB APIs:

1. Start `fastly compute serve`.
2. Load a page with the Osano mirror bundle and stubbed APIs.
3. Trigger an accept-like USP response (`1YN-`).
4. Verify the browser writes `us_privacy=1YN-` and `_ts_consent_src=osano`.
5. Make a subsequent request and confirm EC gating sees the consent cookie.
6. Trigger an opt-out-like USP response (`1YY-`).
7. Verify the browser updates `us_privacy=1YY-` and subsequent requests block or
   withdraw EC according to existing server behavior.

### Existing checks

Run when implementing:

```bash
cd crates/js/lib && npx vitest run
cd crates/js/lib && node build-all.mjs
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## Risks and mitigations

- **First-request limitation:** mirrored cookies are only available after browser
  JavaScript runs. Mitigation: document that EC starts on subsequent eligible
  requests after consent.
- **Multiple CMPs:** another CMP may own standard cookies. Mitigation: a single
  marker cookie plus preserve-by-default behavior for unknown external writers.
- **API readiness races:** GPP may be `not ready` while USP is available.
  Mitigation: handle signals independently and retry boundedly.
- **Over-broad consent inference:** Osano categories are not equivalent to full
  IAB strings. Mitigation: mirror only real IAB API outputs.
- **Stale mirrored cookies:** session cookies may outlive in-memory API state for
  a tab. Mitigation: refresh on load, focus, visibility, and consent events;
  clear only Osano-owned stale values after Osano is initialized.

## Review decisions and open questions

1. **Decision:** Osano must be explicitly enabled. Use this work to refactor the
   Sourcepoint consent mirror so CMP consent mirrors are opt-in through normal
   integration configuration rather than always shipped.
2. **Decision:** Use one shared marker cookie, `_ts_consent_src=osano`, for v1.
3. **Decision:** Follow existing TSJS logging behavior only. Do not add a new
   Osano-specific debug logging flag.

### Marker-cookie strategy rationale

The mirror writes standard cookies such as `us_privacy`, `__gpp`, `__gpp_sid`,
and `euconsent-v2`. Those names are not Osano-specific; another CMP or publisher
script could already be writing them. The marker cookie tells Trusted Server's JS
mirror whether it "owns" the standard consent cookies and is allowed to update or
clear them.

Use a single marker for v1 because the expected deployment model is one active
CMP per publisher site. That keeps the implementation simple, reduces cookie
count, and makes debugging easier. If a target cookie exists without the Osano
marker, preserve it rather than guessing ownership.
