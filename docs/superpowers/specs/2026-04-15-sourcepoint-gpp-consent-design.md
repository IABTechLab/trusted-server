# Sourcepoint GPP Consent for Edge Cookie Generation

**Issue:** #640
**Date:** 2026-04-15
**Status:** Approved

## Problem

Edge Cookie (EC) generation fails for sites using Sourcepoint when consent is
stored only in `localStorage` and not surfaced via the standard cookies Trusted
Server reads. Sourcepoint stores US consent under `_sp_user_consent_*` keys in
`localStorage`, including a full GPP string and applicable section IDs.

Today, Trusted Server only reads consent from `euconsent-v2`, `__gpp`,
`__gpp_sid`, `us_privacy` cookies and the `Sec-GPC` header. Even if `__gpp` /
`__gpp_sid` were present, the server only decodes the EU TCF v2 section from
GPP — it does not use GPP US sections as a consent signal for EC gating.

This creates two gaps:

1. **Transport gap:** The server cannot read browser `localStorage`, so no
   consent reaches the backend unless client code mirrors it into cookies.
2. **Semantics gap:** Even with `__gpp` / `__gpp_sid` cookies present, current
   US-state EC gating does not recognize GPP US sections as valid consent.

## Approach

Thin GPP pass-through: mirror Sourcepoint localStorage consent into standard
cookies on the client, and extend server-side EC gating to recognize GPP US
`sale_opt_out` as a consent signal. No compatibility bridge (`us_privacy`
derivation) — both client and server changes ship together.

## Design

### 1. Client-side: Sourcepoint JS integration

New JS-only integration at `crates/js/lib/src/integrations/sourcepoint/index.ts`.
No Rust-side `IntegrationRegistration` (same pattern as `creative`).

**On page load:**

1. Scan `localStorage` keys matching `_sp_user_consent_*`.
2. Take the first valid match, parse the JSON value.
3. Extract `gppData.gppString` and `gppData.applicableSections` from the payload.
4. Write first-party cookies:
   - `__gpp=<gpp string>` (path `/`, `SameSite=Lax`)
   - `__gpp_sid=<comma-separated section IDs>` (path `/`, `SameSite=Lax`)
5. Log what was written for debugging.

Cookies are session-scoped (no `max-age` / `expires`) since the source of truth
stays in `localStorage` and we re-mirror on each page load. This design assumes
a single active Sourcepoint property per page; if multiple `_sp_user_consent_*`
entries coexist, the first valid one wins. The integration runs once — no
polling or event listeners.

### 2. Server-side: GPP US section decoding

**`crates/trusted-server-core/src/consent/types.rs`** — extend `GppConsent`:

```rust
pub struct GppConsent {
    pub version: u8,
    pub section_ids: Vec<u16>,
    pub eu_tcf: Option<TcfConsent>,
    pub us_sale_opt_out: Option<bool>,  // new
}
```

- `Some(true)` — a US section is present and `sale_opt_out == OptedOut`
- `Some(false)` — a US section is present and `sale_opt_out != OptedOut`
- `None` — no US section exists in the GPP string

**`crates/trusted-server-core/src/consent/gpp.rs`** — add `decode_us_sale_opt_out`:

Checks for any US section ID (7–23) in the parsed `GPPString`. For the first
match, decodes the section via `iab_gpp` and extracts `sale_opt_out`. Maps
`OptOut::OptedOut` to `true`, everything else to `false`.

The `iab_gpp` crate uses different structs per state (`UsNat`, `UsCa`, `UsTn`,
etc.) but they all have `sale_opt_out: OptOut` via `us_common`. We match on the
decoded `Section` enum to extract it.

### 3. Server-side: EC gating update

**`crates/trusted-server-core/src/consent/mod.rs`** — update `allows_ec_creation()`
for `Jurisdiction::UsState(_)`.

New precedence chain:

```
GPC → TCF → GPP US sale_opt_out → us_privacy → fail-closed
```

Insert between the existing TCF and `us_privacy` branches:

```rust
// Check GPP US section for sale opt-out.
if let Some(gpp) = &ctx.gpp {
    if let Some(opted_out) = gpp.us_sale_opt_out {
        return !opted_out;
    }
}
```

Semantics:

- GPP US `sale_opt_out != OptedOut` → EC allowed
- GPP US `sale_opt_out == OptedOut` → EC blocked
- No GPP US section → falls through to `us_privacy`
- GPC still short-circuits at the top (unchanged)
- TCF still takes priority for CMPs like Didomi (unchanged)

### 4. Files touched

| File | Change |
|---|---|
| `crates/js/lib/src/integrations/sourcepoint/index.ts` | New — localStorage auto-discovery, cookie mirroring |
| `crates/js/lib/src/integrations/sourcepoint/index.test.ts` | New — Vitest tests |
| `crates/trusted-server-core/src/consent/types.rs` | Add `us_sale_opt_out: Option<bool>` to `GppConsent` |
| `crates/trusted-server-core/src/consent/gpp.rs` | Add US section decoding, extract `sale_opt_out` |
| `crates/trusted-server-core/src/consent/mod.rs` | Add GPP US branch in `allows_ec_creation()`, tests |

No config changes, no new crate dependencies, no `IntegrationRegistry` changes.

### 5. Testing

**JS (Vitest):**

- Mirrors `__gpp` and `__gpp_sid` from `_sp_user_consent_*` localStorage
- No cookies written when no `_sp_user_consent_*` key exists
- Graceful handling of malformed JSON in localStorage

**Rust — EC gating (`consent/mod.rs`):**

- EC allowed: US state + GPP `us_sale_opt_out = Some(false)`
- EC blocked: US state + GPP `us_sale_opt_out = Some(true)`
- EC blocked: GPC overrides permissive GPP US
- TCF takes priority over GPP US when both present
- GPP US takes priority over `us_privacy` when both present
- No GPP US section falls through to `us_privacy`
- No signals → fail-closed

**Rust — GPP decoding (`consent/gpp.rs`):**

- Extracts `us_sale_opt_out` from GPP string with UsNat section (ID 7)
- `us_sale_opt_out` is `None` when GPP has no US sections

### 6. Non-goals

- No `us_privacy` compatibility bridge (skipped per decision)
- No richer US GPP field extraction (sharing, targeted advertising opt-outs)
- No publisher configuration for Sourcepoint property ID (auto-discovery)
- No Sourcepoint CMP API integration (localStorage-only approach)
