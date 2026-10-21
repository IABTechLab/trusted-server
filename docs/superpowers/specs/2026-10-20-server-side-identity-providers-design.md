# Server-side identity providers

Status: Draft, revision 10. Revised after review rounds 1 to 9. Section 17
maps each finding to its fix.

Issue: none filed yet. Related: [#1245](https://github.com/IABTechLab/trusted-server/issues/1245)
(LiveRamp server-to-server), [#90](https://github.com/IABTechLab/trusted-server/issues/90)
and [#353](https://github.com/IABTechLab/trusted-server/issues/353) (ID5),
[#1246](https://github.com/IABTechLab/trusted-server/issues/1246) (Lockr IDs to
KV), [#777](https://github.com/IABTechLab/trusted-server/issues/777)
(pluggable modules).

Designed against `spec/1246-prebid-eid-fidelity` at `c84a73da7`, which is
`main` at `6ef07ae52` plus the
[Prebid EID fidelity](2026-10-18-prebid-eid-fidelity-design.md) work. Every
`file:line` reference below is to that commit. Core paths are written without
the `crates/trusted-server-core/src/` prefix, and Fastly adapter paths start
with `adapter-fastly/src/`. Prebid.js references are to version 10.26.0, read
from `crates/trusted-server-js/lib/node_modules/prebid.js/modules/`. This spec
depends on the EID fidelity branch for `merge_auction_eids`, the shared
client EID parser and the `ts-eids` cookie carrying the same set the shim
sends.

This spec does not depend on PR #1260. Its `IdentityCapture` trait,
`identity_owner` field and `ts-eids-s` cookie are not on `main`. Where this
spec reuses a name from #1260, it defines the thing from scratch here.

Terms used throughout:

- **EID / UID**: an OpenRTB extended identifier, `{source, uids[]}`, and one
  entry of its `uids`, `{id, atype?, ext?}` (OpenRTB 2.6 §3.2.27, §3.2.28).
- **Identity provider**: a vendor that turns request signals into an EID,
  such as ID5, LiveRamp ATS, Lockr AIM or UID2.
- **Source**: the EID `source` domain a provider's IDs carry, such as
  `id5-sync.com` or `liveramp.com`. It is also the key of the KV `ids` map. A
  **registered** source has an `[[ec.partners]]` entry.
- **EC / KV row**: the Edge Cookie ID (`ts-ec`) and the KV identity-graph
  entry keyed by it (`ec/kv_types.rs:42`).
- **HEM**: a hashed email, the SHA-256 of a normalized email address. LiveRamp
  ATS, Lockr and UID2 resolve from it. It is personal data, not an anonymous
  value.
- **Page load**: one top-level document navigation and every request the page
  makes because of it: `/auction` calls, SPA view changes reported to
  `/_ts/page-bids`, and integration proxy calls.
- **Writer request**: the one request in a page load allowed to write KV
  (section 5.1).
- **Stager**: every other request. It reads cookies and KV and saves changes
  into `ts-eids-s` for the next writer request to write.
- **KV gap**: the registered sources a browser sent to `/auction` that the
  visitor's KV row does not have yet.
- **Hold mode / after-send mode**: whether a provider is called before the
  auction, with the auction waiting for it, or after the response has been
  sent.
- **Orphan recovery**: what TS does when a visitor's EC cookie has no KV row
  (`ec/finalize.rs:282`).
- **`ts-eids`**: the existing first-party cookie the Prebid shim writes with
  the browser's EIDs. Unchanged by this spec.
- **`ts-eids-s`**: a new server-owned cookie, named `__Host-ts-eids-s`, that
  caches provider-collected IDs and holds staged changes. Defined in section
  6.2.

## 1. Problem statement

Trusted Server (TS) collects EIDs three ways today. Browser modules resolve
IDs that the page forwards, partners push UIDs to batch sync, and pull sync
calls partners from the server. Only pull sync calls a provider from the
server, and it speaks one fixed protocol.

### P1. Browser-only collection fails where browsers block vendors

Each provider ships a browser module, and TS forwards what those modules
produce. Safari's tracking protection blocks requests to some vendor hosts.
#1245 reports Prebid's `identityLink` module failing this way on Safari,
leaving no LiveRamp envelope in the auction. Calling providers from the edge
avoids the browser request.

This spec builds that server-side path. In v1 it helps:

- providers that need no hashed email, such as ID5
- providers whose traffic TS already proxies, such as Lockr
- generic pull partners
- any owning provider that can refresh an ID a browser module obtained
  earlier, so the ID survives when the module later fails in that browser

It does not fix the #1245 Safari case by itself. LiveRamp's initial
resolution needs a hashed email, and v1 has no way to obtain one (section
2.2). A Safari visitor whose browser module never obtained an envelope has
nothing to refresh.

### P2. The only server-side path is one fixed protocol

`ec/pull_sync.rs` calls partners after the response is sent. Its protocol is
fixed: `GET <pull_sync_url>?ec_id=<id>` with a bearer token
(`pull_sync.rs:255`, `:368-371`), response `{"uid":"..."}`
(`pull_sync.rs:54-57`). It fills missing UIDs only and never refreshes
(`pull_sync.rs:7-9`).

None of the providers this spec targets speaks that protocol:

- **ID5** takes a POST body with a partner number, device signals, the page
  URL and consent. It returns up to three IDs (`id5-sync.com`, an EUID ID and
  `true-link-id5-sync.com`) and a `signature` the caller sends back on its
  next call (`id5IdSystem.js:40-41`, `:130-170`, `:454-487`).
- **LiveRamp ATS** takes a placement ID, an identifier type and value,
  consent parameters and the end user's IP. It returns one of several
  envelope products, envelopes expire after 15 or 30 days, and refresh is a
  different call that takes the old envelope (#1245 §9, §10).
- **UID2** returns an advertising token for bidders and a refresh token that
  must never reach them.
- **Lockr**'s API traffic already passes through TS's proxy
  (`integrations/lockr.rs:179-237`). Its IDs can be read from the responses,
  with no outbound call.

Each provider needs its own request building, response parsing, error
handling and, for some, private state kept between calls. Without a shared
contract, each integration would repeat consent revalidation, CAS handling,
rate limits and timing. #1245 §6 rules out a private KV writer per vendor.

### P3. A page load writes KV several times

Today a page load can write the KV row once per step:

| Step                                    | Request                                   | Write              | Code                                                        |
| --------------------------------------- | ----------------------------------------- | ------------------ | ----------------------------------------------------------- |
| New visitor gets an EC                  | publisher navigation, during routing      | `create_if_absent` | `adapter-fastly/src/app.rs:845-857`, `ec/mod.rs:424`        |
| New visitor gets an EC                  | integration-proxy navigation              | `create_if_absent` | `integrations/registry.rs:1046-1050`                        |
| Browser EIDs from `ts-eids` or sharedId | navigation, finalize                      | CAS update         | `ec/finalize.rs:83`, `:183`                                 |
| Prebid body EIDs                        | each `/auction`                           | CAS update         | `auction/endpoints.rs:298-303`, `finalize.rs:183`           |
| `ts-eids` on an SPA view                | each `/_ts/page-bids`                     | CAS update         | `publisher.rs:6925`, `finalize.rs:183`                      |
| Pull sync results                       | after send, on requests with a loaded row | CAS update         | `adapter-fastly/src/main.rs:507-517`, `ec/pull_sync.rs:290` |
| Cluster size                            | `/_ts/api/v1/identify`, once per EC       | CAS update         | `ec/identify.rs:107`, `ec/kv.rs:1484`, `:1524-1529`         |

Ingest runs only on requests that set an EID sync source: navigation,
`/auction`, `/_ts/page-bids` and new ECs (`ec/mod.rs:98-111`). Asset routes
skip EC finalize entirely (`adapter-fastly/src/app.rs:834-835`). Pull sync
runs after any response whose request loaded a consenting row
(`pull_sync.rs:86`).

Each of these skips the write when nothing changed, so most are no-ops, but
each can write. A new visitor can cost three writes on the navigation alone,
plus one for each `/auction` and each SPA view. Adding providers naively adds
one more write each. KV writes cost money, Fastly limits how often one key
can be written, and each extra write is another CAS race between tabs.

## 2. Goals and non-goals

### 2.1 Goals

- **G1.** A trait any identity provider implements to be called from the
  server. LiveRamp, Lockr, ID5, UID2 and generic pull partners all fit it,
  including providers that keep private state between calls.
- **G2.** Providers never do HTTP, KV or cookie work themselves. One
  coordinator owns eligibility, consent, transport limits and write-back.
- **G3.** Operators choose per provider between hold mode and after-send
  mode.
- **G4.** At most one browser-driven KV write per page load, and a write
  that fails is not retried. This is strict. The only page-load exception is
  a consent withdrawal's tombstone (section 5.1). Writers outside page loads
  keep their own writes: batch sync, the admin endpoints and the deferred
  deletion framework (sections 2.2, 5.5).
- **G5.** Provider-collected IDs are stored in both a first-party cookie
  (`ts-eids-s`) and KV, and the cookie is read first.
- **G6.** Each source has at most one owning provider, configured by the
  operator. A valid ID the owner wrote wins in the auction over the
  browser's.
- **G7.** IDs that expire (LiveRamp envelopes) are refreshed, and are never
  sent to bidders or returned by `/_ts/api/v1/identify` after expiry.
- **G8.** Today's pull sync becomes one implementation of the trait. Its
  protocol and fill-missing contract are unchanged. Section 10.4 lists the
  behaviour that does change.
- **G9.** The trait can model a provider that needs a HEM, even though v1
  has no way to obtain one.
- **G10.** No browser EID that reaches KV today is lost. Everything today's
  `/auction` ingest writes still reaches KV, through the next writer request.
  What today's `/_ts/page-bids` ingest writes comes from `ts-eids`, which the
  next navigation ingests; only an SPA session longer than that cookie's
  one-day lifetime can lose such an ID. A body-only ID beyond the `ts-eids`
  trim is lost only when KV-gap entries alone exceed the `ts-eids-s` cap
  and the browser sends no further `/auction` before the next navigation.
- **G11.** Writing a provider is easy, and extending the trait does not
  break one. A simple provider is one struct, one descriptor and two short
  methods (section 4.2).
- **G12.** Removals win. An ID removed from KV (opt-out, invalidation,
  withdrawal, deletion) stops reaching bidders within a bounded time, even
  from the cookie. A removal staged by a stager stops that browser's
  bidstream at once and reaches KV at the next writer navigation; a client
  that never makes one keeps it in `ts-eids-r` until it does.

### 2.2 Non-goals

- **HEM intake.** How a hashed email reaches TS (#1245 §4 options A to D)
  needs its own spec. The trait has a typed slot for it. In v1 the slot is
  always empty and providers that need it are skipped.
- **Concrete vendor implementations.** ID5, LiveRamp and Lockr each get
  their own implementation spec. Section 12 shows how each maps onto the
  trait, to prove the trait fits.
- **Other adapters.** KV identity runs only on Fastly
  (`publisher.rs:6896-6899`). Axum, Cloudflare and Spin get this design when
  EC identity is ported to them (#777 rows 52 to 54).
- **Cross-browser linking.** An ID in one browser's EC row does not appear in
  another browser's row (#1245 §12).
- **LiveRamp ATS Direct segments** and GAM targeting.
- **Batch sync and admin writes.** They keep their own writes. Section 7.2
  changes only how batch sync treats an owned source.
- **Removing `ts-eids` or `x-ts-eids`.** Both stay.

## 3. Background

### 3.1 Where EIDs come from today

1. **Browser modules.** Prebid's userId modules resolve IDs in the browser.
   The `trustedServer` bid adapter sends them in the `/auction` body and the
   shim copies the same set into `ts-eids` (EID fidelity spec §4.2, §4.3).
2. **Pull sync.** After the response is sent, TS calls each pull-enabled
   `[[ec.partners]]` entry whose UID is missing (`ec/pull_sync.rs:116`).
3. **Batch sync.** Partners push UIDs to `/_ts/api/v1/batch-sync`
   (`ec/batch_sync.rs`).

### 3.2 How the auction builds `user.eids`

`handle_auction` takes the client set from the body, else from `ts-eids`
(`auction/endpoints.rs:290-296`). It keeps the full body set for finalize
because the cookie is size-capped (`endpoints.rs:298-303`). It reads the KV
row once when a partner registry exists (`endpoints.rs:307-311`), resolves
KV IDs for registered sources (`endpoints.rs:444`), and merges the two with
`merge_auction_eids` (`endpoints.rs:505`). The navigation and
`/_ts/page-bids` merge in `publisher.rs:5547-5568`. The merge forwards
browser EIDs as sent and adds a KV EID only for a source the browser did not
send. The result is consent-gated and reported in `x-ts-eids`
(`ec/eids.rs:17`, 4 KB cap).

Browser ingest is fill-missing: a different stored value is never replaced,
because a browser value carries no freshness signal (`ec/kv.rs:146-154`).

### 3.3 The existing provider pattern

`AuctionProvider` (`auction/provider.rs:84`) is the house pattern for a
pluggable vendor. The provider builds its request and parses the response,
and the orchestrator owns dispatch, timeouts and response handling. This spec
follows the same split.

### 3.4 Platform and timing facts this design depends on

- **KV identity is Fastly-only.** Axum, Cloudflare and Spin pass no KV graph
  (`publisher.rs:6896-6899`) and run no EC finalize or EC generation.
- **New ECs are created before the cookie goes out.** Generation activates an
  ID only after an Add-confirmed create and re-mints on a collision
  (`ec/mod.rs:407-455`). Finalize refuses a generated cookie without a
  backing row (`ec/finalize.rs:145-149`). This spec keeps that.
- **After-send work starts after the last byte.** `send_edgezero_response`
  streams the whole body before returning (`adapter-fastly/src/main.rs:699-702`),
  and pull sync runs after that (`main.rs:510-517`). The response has been
  moved into the send by then, so after-send code cannot see its status
  unless it is captured first (`main.rs:512-517`).
- **State crosses into finalize only through `EcFinalizeState`**
  (`adapter-fastly/src/app.rs:318-332`, inserted at `:535`). Response
  extensions require `Clone + Send + Sync`, which a pending platform request
  is not (`platform/http.rs:178-183`), so an in-flight provider call cannot
  outlive the router.
- **The navigation auction is dispatched before the origin fetch**
  (`publisher.rs:4886-4889`). The origin overlaps the EC preload only when the
  preload runs, the request has no template-cache key and the platform
  supports concurrent fan-out and pending streaming responses
  (`publisher.rs:4832-4844`).
- **Navigation detection.** `is_navigation_request` checks
  `Sec-Fetch-Dest: document`, which excludes iframes, and falls back to
  `Accept: text/html`, which a `fetch()` can also send (`http_util.rs:93-118`).
  `is_prefetch_request` detects prefetch and prerender and is `pub(crate)`
  in core (`publisher.rs:3200`). EC classification must wait until
  pre-route filters allow dispatch (`ec/mod.rs:91-96`).
- **Orphan recovery rotates.** When a cookie's row is proven absent
  (`finalize.rs:371-400`), recovery mints a new EC ID and sets a new cookie
  (`finalize.rs:304`, `:333`), before the response is sent. Only the
  publisher fallback may rotate (`adapter-fastly/src/app.rs:885-890`).
- **Today's navigation preload finds orphans.** Every returning GET
  navigation preloads the row unless a valid pull-complete marker lets it
  skip (`publisher.rs:3110-3117`, `:4819-4831`).
- **Withdrawal tombstones existing rows only,** with a CAS write of up to five
  attempts and then a completion-marker write, per withdrawn ID
  (`ec/kv.rs:1349-1430`, `:1115-1124`; `finalize.rs:490`, `:509-525`).
- **Cookies make responses private.** Any response with `Set-Cookie` gets
  `Cache-Control: private, max-age=0` and loses its edge-cache headers
  (`response_privacy.rs:122-145`). Returning-visitor navigations set no EC
  cookie today (`finalize.rs:124`), though they can set or expire the
  pull-complete marker (`pull_sync_marker.rs:132-155`).
- **The rate limiter counts in 60-second windows** and increments on every
  check that is under the limit. Any hourly limit below 60 becomes one call
  per minute (`ec/rate_limiter.rs:19-27`,
  `adapter-fastly/src/rate_limiter.rs:41-74`). Pull sync keys it on `ec_hash`
  (`pull_sync.rs:373`), the HMAC of the client IP (`generation.rs:83-93`), so
  everyone behind one address shares one budget.
- **The shim's `ts-eids` is small.** It is trimmed to 3,072 encoded bytes,
  lives one day and has no EID-level `ext` (EID fidelity spec §4.3). The
  server accepts up to 8 KB (`ec/prebid_eids.rs:24`).
- **Prebid Server receives the browser's cookies.** The Prebid transport
  copies the inbound `Cookie` header into every Prebid Server call and strips
  only consent cookies, only in `OpenrtbOnly` mode (`integrations/prebid.rs:2091-2108`,
  `consent_config.rs:137-150`). The publisher proxy forwards the `Cookie`
  header to the origin unchanged (`publisher.rs:3124-3147`).

## 4. Architecture

### 4.1 Overview

```mermaid
flowchart TD
    subgraph Writer["Writer request: a top-level navigation"]
        A[Read ts-ec, consent, ts-eids, ts-eids-s] --> B{Cookies answer<br/>everything?}
        B -- no --> C[KV snapshot read]
        B -- yes --> D
        C --> D[Pick eligible providers]
        D --> E[Hold-mode providers, if the<br/>navigation runs an auction]
        E --> F[Finalize: queue changes,<br/>set cookies only if needed]
        F --> G[Send response]
        G --> H[Returning EC: re-read live row]
        H --> I[After-send providers]
        I --> J[Apply IdentityMutation:<br/>one conditional write]
    end
    subgraph Stagers["Stagers: /auction, /_ts/page-bids, integration proxy"]
        K[Read cookies, and KV where<br/>the route reads it today] --> L[Hold-mode providers,<br/>passive capture]
        L --> M[Save results and the KV gap<br/>into ts-eids-s; never write KV]
    end
```

The design has four parts:

1. **`IdentityProvider` trait** (sections 4.2 to 4.6). One per vendor.
2. **`IdentityCoordinator`** (section 4.7). Shared. Chooses providers,
   dispatches them, turns results into changes.
3. **`IdentityMutation`** (section 5.4). A request-scoped accumulator. Every
   KV change in the writer request is added to it, and it is applied with one
   write.
4. **`ts-eids-s` cookie** (section 6.2). The cookie-first copy of
   provider-collected IDs and the place stagers save changes.

New code lives in `crates/trusted-server-core/src/ec/identity/`:

| File              | Contents                                                               |
| ----------------- | ---------------------------------------------------------------------- |
| `mod.rs`          | Module docs, re-exports                                                |
| `provider.rs`     | The `IdentityProvider` and `IdentityProviderConfig` traits             |
| `descriptor.rs`   | `ProviderDescriptor`, `Resolves`, `Captures`, `Input`, `ConsentPolicy` |
| `input.rs`        | `ResolveInput`, `CaptureInput`, `StoredId`, `HashedEmail`, `UnixTime`  |
| `exchange.rs`     | `ProviderRequest`, `ProviderResponse`                                  |
| `resolution.rs`   | `Resolution`, `ResolvedId`, `IdentityProviderError`                    |
| `coordinator.rs`  | Eligibility, dispatch, circuit breaker, result-to-change map           |
| `mutation.rs`     | `IdentityMutation` and its merge into a live `KvEntry`                 |
| `cookie.rs`       | `ts-eids-s` and `ts-eids-r`: seal, open, encode, decode, size caps     |
| `generic_pull.rs` | `GenericPullProvider`, today's pull sync on the trait                  |
| `config.rs`       | `[ec.identity]` settings and ownership validation                      |
| `test_support.rs` | `ProviderHarness`, a fake HTTP client and a write-counting KV store    |

### 4.2 The trait

The trait follows five rules:

- **One required method.** A provider implements `descriptor`. It adds a
  capability by declaring it in the descriptor and overriding the matching
  methods. Every other method has a default body.
- **Declarative.** What a provider is, may write, needs and must respect
  lives in one `ProviderDescriptor`. It is built from config with no
  secrets, so `ts config validate` and the running coordinator read the same
  data.
- **No I/O.** The coordinator sends requests, buffers, decodes and caps
  responses, and does all KV and cookie work. Providers only build requests
  and read buffered bytes, so the trait is synchronous and needs no
  `async_trait`. `AuctionProvider` parses asynchronously so that it can drain
  streaming bodies. The coordinator buffers instead, because it has to cap
  response bytes anyway.
- **Pure.** `request`, `response` and `capture` have no side effects and no
  hidden inputs. Randomness comes only from `ResolveInput::random_bytes`,
  and anything `response` needs from `request` travels in the call context
  (section 4.5). The coordinator may call `request` twice in one request,
  first as a dry run (section 5.2). `input.is_dry_run()` lets a provider skip
  expensive work in the dry run. A provider with private state gets no dry
  run.
- **Safe defaults.** The default consent policy is the strictest one, a
  provider can read only what it declared, and a capability declared but not
  implemented fails loudly in tests (section 13).

```rust
/// A vendor that turns server-side request signals into EIDs.
///
/// Only [`descriptor`](Self::descriptor) is required. A provider adds a
/// capability by declaring it in its descriptor and overriding the matching
/// methods:
///
/// - active resolution: [`Resolves`], plus [`request`](Self::request) and
///   [`response`](Self::response)
/// - passive capture: [`Captures`], plus [`capture`](Self::capture)
///
/// Methods added to this trait later get default bodies, so existing
/// providers keep compiling. No method takes generic parameters, so the
/// trait stays object-safe. Every default body returns the
/// [`IdentityProviderError::not_implemented`] marker, so a capability that is
/// declared but not implemented is caught by the harness.
pub trait IdentityProvider: Send + Sync {
    /// What this provider is, what it may write, what it needs and which
    /// consent it requires.
    fn descriptor(&self) -> &ProviderDescriptor;

    /// Builds the call for this request, or `None` to skip it this time.
    ///
    /// Skipping is not a failure: the coordinator waits `refresh_after_sec`
    /// before asking again (section 8).
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::UndeclaredInput`] when it reads
    /// something its descriptor did not declare, or another
    /// [`IdentityProviderError`] when the request cannot be built.
    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let _ = input;
        Err(IdentityProviderError::not_implemented("request"))
    }

    /// Turns the buffered response to [`request`](Self::request) into a
    /// resolution.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Rejected`] for a refusal and
    /// [`IdentityProviderError::Malformed`] for a body it cannot parse.
    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let _ = (response, input);
        Err(IdentityProviderError::not_implemented("response"))
    }

    /// Reads IDs from a response TS proxied for this provider, with no
    /// outbound call. [`Resolution::no_match`] means the response held
    /// nothing to capture, and costs nothing: capture never backs off.
    ///
    /// # Errors
    ///
    /// Returns an [`IdentityProviderError`] only for a provider bug. A body
    /// it does not recognise is `Ok(Resolution::no_match())`, not an error.
    fn capture(
        &self,
        input: &CaptureInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let _ = input;
        Err(IdentityProviderError::not_implemented("capture"))
    }
}

/// Implemented by each identity integration's config type.
///
/// Builds the provider's descriptor from config alone, with no secrets, so
/// `ts config validate` checks ownership against the same descriptor the
/// running provider uses.
pub trait IdentityProviderConfig: IntegrationConfig {
    /// Describes the provider this config enables.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the config cannot
    /// describe a valid provider.
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>>;
}
```

A provider's constructor calls its config's `build_descriptor()` and stores
the result, so the validated descriptor and the running one are the same
value. The two methods have different names so that a type implementing both
traits never hits an ambiguous call. `IntegrationConfig` is the existing
config trait (`settings.rs:235`), so identity configs load like every other
integration config.

**How each change stays additive.** Vendor providers live in
`trusted-server-core` today, and `#[non_exhaustive]` has no effect inside its
defining crate. So additivity rests on construction rules, with
`#[non_exhaustive]` as extra protection for providers that move into their
own crates under #777:

| Change                  | Why existing providers keep compiling                                                                      |
| ----------------------- | ---------------------------------------------------------------------------------------------------------- |
| A new capability        | A new trait method with a default body, and a new descriptor builder method                                |
| A new input             | A new `Input` variant and a new accessor. Providers list inputs; they never match on `Input`               |
| A new field on any type | Every public struct has private fields and is built through constructors or builders                       |
| A new outcome           | A new `Resolution` constructor. Providers build resolutions, never match on them                           |
| A new consent signal    | Providers keep compiling. By design they fail closed: `strict()` blocks the signal until a vendor opts out |
| A new resolve kind      | Kinds are opt-in: a provider is dispatched only for the kinds it lists (default `Initial`, `Refresh`)      |
| A new error             | Errors are built through constructors such as `IdentityProviderError::rejected(status)`                    |

Providers compare kinds with predicates (`input.is_refresh(&source)`) rather
than `match`. Operator settings (`[ec.identity.providers.<id>]`: mode,
timeouts, source narrowing, caps) are applied by the coordinator on top of
the descriptor. Providers never read them.

**Conventions for the listings below.** Every builder method and getter is
`#[must_use]`. `Resolves`, `Captures` and `EidProvenance` implement
`Default`. `Input`, `Access`, `ResolveKind` and `UsOptOut` derive
`Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, derive_more::Display`.
`When` derives `Debug, Clone, Copy, PartialEq, Eq` only, because `Duration`
has no `Display`. `ProviderRequest`, `StoredId` and `HashedEmail` derive
`Debug, Clone`; `HashedEmail`'s `Debug` prints a redacted marker.
`StateChange` derives `Debug, PartialEq, Eq`. `ProviderDescriptor`,
`Resolves`, `Captures`, `ConsentPolicy` and `EidProvenance` derive
`Debug, Clone, PartialEq, Eq`, so tests can compare descriptors. `UnixTime` derives
`Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord`. `IdentityProviderId`
and `SourceDomain` derive `Debug, Clone, PartialEq, Eq, Hash, PartialOrd,
Ord, derive_more::Display` and implement `FromStr` (with
`type Err = Report<IdentityProviderError>`), `Serialize` and `Deserialize`.
Every fallible function documents `# Errors`, and every public item has a
doc comment. The listings abbreviate these.

### 4.3 The descriptor

```rust
let descriptor = ProviderDescriptor::builder("exampleid".parse()?)
    .with_source("exampleid.example.com".parse()?)
    .with_resolves(
        Resolves::new()
            .with_endpoint("https://api.exampleid.example.com/v1/fetch")
            .with_needs([Input::ClientIp, Input::UserAgent])
            .with_wants([Input::PageUrl]),
    )
    .with_consent_policy(ConsentPolicy::strict().with_tcf_vendor(9999))
    .build()?;
```

```rust
impl ProviderDescriptor {
    pub fn builder(id: IdentityProviderId) -> ProviderDescriptorBuilder;
    pub fn id(&self) -> &IdentityProviderId;
    pub fn sources(&self) -> &[SourceDomain];
    /// The first source added. Every built descriptor has one.
    pub fn primary_source(&self) -> &SourceDomain;
    /// Finds a declared source by a vendor's spelling of it, normalized.
    pub fn source_named(&self, name: &str) -> Option<&SourceDomain>;
    pub fn resolves(&self) -> Option<&Resolves>;
    /// The endpoint declared with `Resolves::with_endpoint`, if any.
    pub fn endpoint(&self) -> Option<&Url>;
    pub fn captures(&self) -> Option<&Captures>;
    pub fn consent_policy(&self) -> &ConsentPolicy;
    pub fn provenance(&self, source: &SourceDomain) -> Option<&EidProvenance>;
    /// Whether IDs of `source` are declared as expiring.
    pub fn is_expiring(&self, source: &SourceDomain) -> bool;
}

impl ProviderDescriptorBuilder {
    /// Adds a source this provider may write. Repeatable.
    pub fn with_source(self, source: SourceDomain) -> Self;
    /// Declares active resolution.
    pub fn with_resolves(self, resolves: Resolves) -> Self;
    /// Declares passive capture.
    pub fn with_captures(self, captures: Captures) -> Self;
    /// Replaces the default policy, [`ConsentPolicy::strict`].
    pub fn with_consent_policy(self, policy: ConsentPolicy) -> Self;
    /// Marks the IDs of every declared source as expiring (section 6.1).
    pub fn with_expiring_ids(self) -> Self;
    /// Marks the IDs of one declared source as expiring, for a provider
    /// whose sources differ.
    pub fn with_expiring_source(self, source: &SourceDomain) -> Self;
    /// EID-level fields for a declared source. The coordinator adds them to
    /// every server-side value of that source this provider wrote. No KV
    /// field stores them.
    pub fn with_provenance(self, source: &SourceDomain, provenance: EidProvenance) -> Self;
    /// # Errors
    ///
    /// Returns `Config` when the descriptor has no source, no capability, a
    /// resolver with no endpoint or host, an endpoint that is not an `https`
    /// URL, a host that is not a bare hostname, or provenance or an
    /// expiring mark for a source it does not declare.
    pub fn build(self) -> Result<ProviderDescriptor, Report<IdentityProviderError>>;
}

impl Resolves {
    pub fn new() -> Self;
    /// The endpoint the provider calls, as config spells it. `build()`
    /// parses it and requires `https`. Its host becomes an allowed host.
    pub fn with_endpoint(self, endpoint: &str) -> Self;
    /// Adds another host the provider may call. Repeatable.
    pub fn with_host(self, host: &str) -> Self;
    /// Inputs required for every kind. Without them the provider is skipped.
    pub fn with_needs(self, inputs: impl IntoIterator<Item = Input>) -> Self;
    /// Inputs required for one kind only.
    pub fn with_needs_for(self, kind: ResolveKind, inputs: impl IntoIterator<Item = Input>) -> Self;
    /// Inputs passed when available, never required.
    pub fn with_wants(self, inputs: impl IntoIterator<Item = Input>) -> Self;
    /// The kinds this provider handles. Default: `Initial` and `Refresh`.
    pub fn with_kinds(self, kinds: impl IntoIterator<Item = ResolveKind>) -> Self;
    /// This provider keeps private state, so it runs only on the writer
    /// request, where state can be stored (section 5.3).
    pub fn with_private_state(self) -> Self;
}

impl Captures {
    pub fn new() -> Self;
    /// Offers proxied responses to exactly this method and path. Repeatable.
    pub fn with_path(self, method: Method, path: &str) -> Self;
    /// Offers proxied responses whose path starts with `prefix`. Repeatable.
    pub fn with_path_prefix(self, prefix: &str) -> Self;
    /// JSON pointers to consent strings in the proxied request's JSON body
    /// (TCF, GPP or US Privacy, detected by format). The coordinator
    /// extracts and decodes only these fields, and gates capture by the
    /// stricter of the cookie consent and the body consent. The provider
    /// never sees the body.
    pub fn with_request_consent(self, pointers: impl IntoIterator<Item = &'static str>) -> Self;
}

/// EID-level fields (OpenRTB 2.6 §3.2.27) that bidders read, such as who
/// inserted an ID and how it was matched.
impl EidProvenance {
    pub fn new() -> Self;
    pub fn with_inserter(self, inserter: &str) -> Self;
    pub fn with_matcher(self, matcher: &str) -> Self;
    pub fn with_mm(self, mm: i32) -> Self;

    pub fn inserter(&self) -> Option<&str>;
    pub fn matcher(&self) -> Option<&str>;
    pub fn mm(&self) -> Option<i32>;
}

/// An input a provider can ask for. Consent strings, the provider's stored
/// IDs, the publisher domain, the time and the declared endpoint are always
/// available. Private state is available to providers declared
/// `with_private_state()`.
#[non_exhaustive]
pub enum Input {
    ClientIp,
    UserAgent,
    /// The top-level page URL without query string or fragment.
    PageUrl,
    /// Country and subdivision only.
    Region,
    HashedEmail,
}

/// Something a provider read without declaring it.
#[non_exhaustive]
pub enum Access {
    Input(Input),
    Endpoint,
    PrivateState,
}

impl ConsentPolicy {
    /// The default. Blocked when any consent signal present cannot be
    /// decoded. Where TCF applies, requires Purposes 1 and 4. Blocked by every
    /// US opt-out TS decodes (sale, sharing, targeted advertising, and the
    /// GPC and known-child fields of GPP sections), by `Sec-GPC: 1` and by
    /// `DNT: 1`.
    pub fn strict() -> Self;
    /// Also requires these TCF purposes, where TCF applies.
    pub fn with_tcf_purposes(self, purposes: impl IntoIterator<Item = u8>) -> Self;
    /// Requires vendor consent for this IAB Global Vendor List ID.
    pub fn with_tcf_vendor(self, vendor_id: u16) -> Self;
    /// Relaxations. Each needs a documented legal basis in the vendor spec.
    pub fn allowing_dnt(self) -> Self;
    pub fn allowing_gpc(self) -> Self;
    pub fn allowing_us_opt_out(self, opt_out: UsOptOut) -> Self;
}

#[non_exhaustive]
pub enum UsOptOut {
    Sale,
    Sharing,
    TargetedAdvertising,
}

impl SourceDomain {
    pub fn as_str(&self) -> &str;
    /// Whether a vendor's spelling of a source names this one, after
    /// normalization.
    pub fn matches(&self, name: &str) -> bool;
}
```

The full EC ID is not an `Input`. A public variant would let any provider
declare it, and only generic pull may receive it. `Resolves` has a
`pub(in crate::ec::identity)` method, `with_ec_id()`, and `ResolveInput` an
accessor with the same visibility. Only `generic_pull.rs` uses them.

Every capture path and prefix must lie under the registering integration's
own proxy routes (`IntegrationProxy::routes`, `integrations/registry.rs:316`).
`IntegrationRegistration::build()` cannot fail (`integrations/registry.rs:720-722`),
so the check runs in `IntegrationRegistry::with_plan`
(`integrations/registry.rs:842-865`), which already returns an error and
merges every registration. The error names the integration.

`IdentityProviderId` is separate from `crate::auction::ProviderId`
(`auction/plan.rs:24-77`) but follows the same pattern. `FromStr` accepts
`^[a-z][a-z0-9_-]{0,62}$`, so an ID can be a bare TOML key and can equal an
integration ID that uses `_`. `SourceDomain::from_str` goes through
`normalize_partner_source_domain` (`ec/partner.rs:20`), mapping its `String`
error to `Config`, so `"…".parse()?` composes inside `build_descriptor`.

```rust
impl ProviderConsent {
    pub fn gdpr_applies(&self) -> bool;
    pub fn raw_tc_string(&self) -> Option<&str>;
    pub fn raw_gpp_string(&self) -> Option<&str>;
    pub fn gpp_section_ids(&self) -> Option<&[u16]>;
    pub fn raw_us_privacy(&self) -> Option<&str>;
}
```

### 4.4 Inputs

The coordinator gives a provider only what its descriptor declared. When the
sources in one request have different kinds, the inputs of every kind
present are available, and the provider uses `is_initial` or `is_refresh` to
decide per source. A required input is read with `?` and a wanted one with
`.ok()`:

```rust
let ip = input.client_ip()?;
let page = input.page_url().ok();
```

Accessors fail in two ways:

- **`UndeclaredInput { access }`**: the descriptor never declared the input,
  the endpoint or private state. That is a provider bug. `.ok()` would hide
  it, so every accessor also records an undeclared read on the
  `ResolveInput`. After `request` or `response` returns, the coordinator logs any recorded
  read as `Config`, and the harness fails the test (section 13), whatever
  the provider did with the error.
- **`InputUnavailable { input }`**: the input was declared but is missing
  this time, for example a wanted input or one declared only for another
  kind. That is a normal skip, handled like `request` returning `None`.

```rust
impl ResolveInput<'_> {
    /// The sources to resolve this time, each with its own kind.
    pub fn sources(&self) -> impl Iterator<Item = (&SourceDomain, ResolveKind)> + '_;
    pub fn is_initial(&self, source: &SourceDomain) -> bool;
    pub fn is_refresh(&self, source: &SourceDomain) -> bool;
    /// The descriptor's primary source, for single-source providers.
    pub fn primary_source(&self) -> &SourceDomain;
    /// The stored record for one of this provider's sources, valid or
    /// expired.
    pub fn stored(&self, source: &SourceDomain) -> Option<&StoredId>;
    /// This provider's private state. The raw JSON is loaded once per
    /// request and decoded on each call. `Ok(None)` when there is none.
    ///
    /// # Errors
    ///
    /// `InvalidState` when the stored state does not decode as `T`, for
    /// example after a schema change. The failed decode is recorded on the
    /// `ResolveInput`, like an undeclared read, and the coordinator clears
    /// the stored state whatever the provider returns, so the next call
    /// starts fresh (section 8). Without `with_private_state()` it is
    /// `UndeclaredInput`.
    pub fn state<T: DeserializeOwned>(&self) -> Result<Option<T>, Report<IdentityProviderError>>;
    /// True during the dry run of section 5.2. The request is discarded, so
    /// a provider may skip expensive work and return `Some` early. Always
    /// false for a provider declared `with_private_state()`, which gets no
    /// dry run.
    pub fn is_dry_run(&self) -> bool;
    /// The endpoint declared with `Resolves::with_endpoint`.
    pub fn endpoint(&self) -> Result<&Url, Report<IdentityProviderError>>;
    pub fn consent(&self) -> &ProviderConsent;
    pub fn publisher_domain(&self) -> &str;
    pub fn now(&self) -> UnixTime;
    /// Random bytes from the platform's generator, the one EC generation
    /// already uses (`ec/generation.rs:52-56`). Fixed by the harness, so
    /// tests are deterministic.
    pub fn random_bytes<const N: usize>(&self) -> [u8; N];

    pub fn client_ip(&self) -> Result<IpAddr, Report<IdentityProviderError>>;
    pub fn user_agent(&self) -> Result<&str, Report<IdentityProviderError>>;
    pub fn page_url(&self) -> Result<&Url, Report<IdentityProviderError>>;
    pub fn region(&self) -> Result<&Region, Report<IdentityProviderError>>;
    pub fn hashed_email(&self) -> Result<&HashedEmail, Report<IdentityProviderError>>;
}

impl CaptureInput<'_> {
    /// The proxied request path, without query string.
    pub fn path(&self) -> &str;
    pub fn response(&self) -> &ProviderResponse;
    pub fn primary_source(&self) -> &SourceDomain;
    pub fn source_named(&self, name: &str) -> Option<&SourceDomain>;
    pub fn stored(&self, source: &SourceDomain) -> Option<&StoredId>;
    /// The stricter of the cookie consent and the request-body consent.
    pub fn consent(&self) -> &ProviderConsent;
    pub fn publisher_domain(&self) -> &str;
    pub fn now(&self) -> UnixTime;
}

#[non_exhaustive]
pub enum ResolveKind {
    /// No stored record for the source.
    Initial,
    /// A stored record exists, valid or expired, and is due (section 4.7,
    /// step 4). The provider decides whether it can refresh an expired one;
    /// if not, it returns `None` for that source.
    Refresh,
}

impl StoredId {
    pub fn uid(&self) -> &str;
    pub fn atype(&self) -> Option<i32>;
    /// UID-level ext, as bidders read it in `uids[].ext`.
    pub fn ext(&self) -> Option<&serde_json::Value>;
    /// The provider that wrote it. `None` for browser ingest and legacy rows.
    pub fn writer(&self) -> Option<&IdentityProviderId>;
    pub fn acquired_at(&self) -> Option<UnixTime>;
    pub fn expires_at(&self) -> Option<UnixTime>;
    pub fn refresh_at(&self) -> Option<UnixTime>;
    pub fn is_expired(&self, now: UnixTime) -> bool;
}

impl Region {
    /// ISO 3166-1 alpha-2, upper case, for example `"US"`.
    pub fn country(&self) -> &str;
    /// The ISO 3166-2 subdivision without the country prefix, upper case,
    /// for example `"CA"`. `None` when the platform does not know it.
    pub fn subdivision(&self) -> Option<&str>;
    /// Whether this is `country` and `subdivision`.
    pub fn is(&self, country: &str, subdivision: &str) -> bool;
}

impl HashedEmail {
    pub fn as_bytes(&self) -> &[u8; 32];
    /// Lowercase hex, as LiveRamp takes it.
    pub fn to_hex(&self) -> String;
    /// Standard base64, as UID2 takes it.
    pub fn to_base64(&self) -> String;
}

impl UnixTime {
    pub fn from_secs(secs: u64) -> Self;
    /// For vendors such as Lockr and UID2 that send milliseconds.
    pub fn from_millis(millis: u64) -> Self;
    pub fn as_secs(self) -> u64;
}

impl Add<Duration> for UnixTime {
    type Output = UnixTime;
}
```

`ProviderConsent` is a view of the request's `ConsentContext`
(`consent/types.rs:109-148`) with the raw strings providers forward and
`gdpr_applies`. It leaves out the decoded detail and `jurisdiction`.
Providers never decide consent: the coordinator has already applied the
descriptor's policy. The raw GPP string and its section IDs still show which
US state section applies, so withholding `jurisdiction` hides only TS's
decoded reading of it.

Where each input comes from. Phase 1 captures the user agent, DNT and the
page URL into `EcContext`, because after-send code no longer has the request
(section 3.4):

- **`ClientIp`**: `services.client_info().client_ip`, the platform's address
  (as `publisher.rs:5574-5578` does), never a request header and never the
  normalized EC IP.
- **`PageUrl`**: the navigation URL; on `/auction`, the Referer as the
  auction sanitizer reduces it (`auction/formats.rs:106-155`); on
  `/_ts/page-bids`, the request origin plus the normalized `path` parameter
  (`publisher.rs:6978-6989`). Always without query string or fragment. A
  path can still carry identifiers on some sites, so `PageUrl` is a wanted
  input for most providers, never needed.
- **`Region`**: from the request's `GeoInfo` (`adapter-fastly/src/app.rs:423-432`).
  `GeoInfo.region` is a free-form `Option<String>` (`platform/types.rs:32`),
  so the coordinator normalizes it to the ISO 3166-2 subdivision and upper
  case. City, coordinates, metro and ASN are never passed.

`HashedEmail` holds the 32 raw bytes of a SHA-256 digest. Its `Debug` output
is redacted. No code in v1 constructs one.

The coordinator supplies `now` from `checked_current_timestamp()`
(`ec/mod.rs:736-748`) and skips identity work for the request when the clock
fails, because a zero time would make every expiry check pass.

### 4.5 Requests, responses and results

```rust
impl ProviderRequest {
    pub fn get(url: &Url) -> Self;
    pub fn post_json(url: &Url, body: &impl Serialize) -> Result<Self, Report<IdentityProviderError>>;
    /// A POST with a `text/plain` body, for APIs that send base64 text.
    pub fn post_text(url: &Url, body: &str) -> Self;
    /// Any method, for APIs the helpers above do not fit. `get`, `post_json`
    /// and `post_text` are shorthands for this.
    pub fn new(method: Method, url: &Url) -> Self;
    /// Sets the body and its `Content-Type`.
    ///
    /// # Errors
    ///
    /// Returns `Config` when `content_type` is not a valid header value.
    pub fn with_body(self, content_type: &str, body: Vec<u8>) -> Result<Self, Report<IdentityProviderError>>;
    pub fn with_query(self, name: &str, value: &str) -> Self;
    /// Both header helpers accept any header. A `Cookie` header, from
    /// either, makes the coordinator refuse to send the request, and the
    /// harness's conformance check fails it (section 13).
    pub fn with_header(self, name: HeaderName, value: HeaderValue) -> Self;
    /// For dynamic values. Rejects an invalid name or value.
    pub fn try_with_header(self, name: &str, value: &str) -> Result<Self, Report<IdentityProviderError>>;
    /// Adds `Authorization: Bearer <token>`.
    pub fn with_bearer(self, token: &Redacted<String>) -> Self;
    /// Data `response` needs from this call, such as a nonce or which
    /// endpoint was used. Kept in memory with the call, never sent.
    ///
    /// # Errors
    ///
    /// Returns `Config` when it does not serialize, or is over 1,024 bytes
    /// of JSON.
    pub fn try_with_call_context(self, context: &impl Serialize) -> Result<Self, Report<IdentityProviderError>>;
    pub fn method(&self) -> &Method;
    pub fn url(&self) -> &Url;
    pub fn headers(&self) -> &HeaderMap;
    pub fn body(&self) -> &[u8];
}

impl ProviderResponse {
    pub fn status(&self) -> StatusCode;
    pub fn header(&self, name: &str) -> Option<&str>;
    /// The decoded body. The coordinator undoes `Content-Encoding` first.
    pub fn body(&self) -> &[u8];
    /// Parses the body as JSON, whatever the status. A parse failure is
    /// `Malformed`.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, Report<IdentityProviderError>>;
    /// Requires a 2xx status, else `Rejected`, then parses the body as JSON.
    pub fn success_json<T: DeserializeOwned>(&self) -> Result<T, Report<IdentityProviderError>>;
    /// Requires a 2xx status, else `Rejected`, and a UTF-8 body, else
    /// `Malformed`.
    pub fn success_text(&self) -> Result<&str, Report<IdentityProviderError>>;
    /// The context the request carried, decoded. `Ok(None)` when there was
    /// none; a context that does not decode as `T` is `Config`.
    pub fn call_context<T: DeserializeOwned>(&self) -> Result<Option<T>, Report<IdentityProviderError>>;
}

impl Resolution {
    /// One or more IDs, at most one per source: KV holds one UID per
    /// source. The coordinator keeps the first ID for a source, discards the
    /// rest with a log, and the harness fails the test. An empty set becomes
    /// `no_match`, and `is_no_match()` then returns true.
    pub fn found(ids: impl IntoIterator<Item = ResolvedId>) -> Self;
    /// The provider recognized no one, or a capture held nothing.
    pub fn no_match() -> Self;
    /// The stored ID for `source` is no longer valid.
    pub fn invalidate(source: &SourceDomain) -> Self;
    /// The provider reports the user opted out.
    pub fn opted_out() -> Self;
    /// Also invalidates `source`. Chainable after `found`, so one response
    /// can return some sources and drop others.
    pub fn also_invalidate(self, source: &SourceDomain) -> Self;
    /// The earliest the coordinator may call again. Default: 24 hours after
    /// `no_match`, none otherwise.
    pub fn with_retry_in(self, delay: Duration) -> Self;
    /// Replaces the provider's private state.
    ///
    /// The state is stored as embedded JSON of at most 1,024 bytes. A larger
    /// value is not stored: the old state stays, the IDs are kept and a
    /// state-size warning is logged. From a provider not declared
    /// `with_private_state()`, the whole resolution is a `Config` error.
    /// `capture` never changes state: a state change in a capture result is
    /// ignored with a warning, and the harness fails the test.
    pub fn replacing_state(self, state: &impl Serialize) -> Self;
    pub fn clearing_state(self) -> Self;

    pub fn ids(&self) -> &[ResolvedId];
    pub fn invalidated(&self) -> &[SourceDomain];
    pub fn is_no_match(&self) -> bool;
    pub fn is_opted_out(&self) -> bool;
    pub fn retry_in(&self) -> Option<Duration>;
    pub fn state_change(&self) -> StateChange<'_>;
}

pub enum StateChange<'a> {
    Keep,
    /// The new state as JSON.
    Replace(&'a str),
    Clear,
}

impl ResolvedId {
    pub fn new(source: &SourceDomain, uid: impl Into<String>) -> Self;
    pub fn with_atype(self, atype: i32) -> Self;
    /// UID-level ext. Over 256 serialized bytes it is dropped and the ID
    /// kept.
    pub fn with_ext(self, ext: serde_json::Value) -> Self;
    pub fn with_expires_in(self, delay: Duration) -> Self;
    pub fn with_expires_at(self, at: UnixTime) -> Self;
    pub fn with_refresh_in(self, delay: Duration) -> Self;
    pub fn with_refresh_at(self, at: UnixTime) -> Self;

    pub fn source(&self) -> &SourceDomain;
    pub fn uid(&self) -> &str;
    pub fn atype(&self) -> Option<i32>;
    pub fn ext(&self) -> Option<&serde_json::Value>;
    pub fn expiry(&self) -> Option<When>;
    pub fn refresh(&self) -> Option<When>;
}

/// A time as a provider gave it: relative to `now`, or absolute.
pub enum When {
    In(Duration),
    At(UnixTime),
}

/// A provider failure. Built through the constructors below.
#[derive(Debug, derive_more::Display)]
#[non_exhaustive]
pub enum IdentityProviderError {
    /// Configuration or provider code makes the call impossible.
    #[display("identity provider configuration error: {message}")]
    Config { message: String },
    /// The provider read something its descriptor did not declare.
    #[display("`{access}` was read but not declared")]
    UndeclaredInput { access: Access },
    /// A declared input is not available for this request.
    #[display("input `{input}` is unavailable for this request")]
    InputUnavailable { input: Input },
    /// The provider refused the request.
    #[display("provider rejected the request with status {status}")]
    Rejected { status: StatusCode },
    /// The provider's response could not be parsed.
    #[display("provider response could not be parsed")]
    Malformed,
    /// The stored private state does not decode as the provider's type.
    #[display("stored provider state could not be decoded")]
    InvalidState,
    /// Set by the coordinator only. Providers never see transport failures.
    #[display("provider request failed in transport")]
    Transport,
}

impl IdentityProviderError {
    pub fn config(message: impl Into<String>) -> Report<Self>;
    pub fn rejected(status: StatusCode) -> Report<Self>;
    pub fn malformed() -> Report<Self>;
    /// The default body of a capability method: a `Config` error with a
    /// fixed marker message, so the harness can tell a missing override from
    /// a real failure.
    pub fn not_implemented(method: &'static str) -> Report<Self>;
}

impl core::error::Error for IdentityProviderError {}
```

Relative times (`with_expires_in`, `with_refresh_in`, `with_retry_in`) are
resolved against the request's `now` by the coordinator, so providers rarely
need the clock.

The coordinator discards a `ResolvedId` whose source the provider does not
claim (or that the operator narrowed away), or whose `uid` is empty or over
`MAX_UID_LENGTH` (`ec/kv_types.rs:35`). It logs each discard with the
provider ID and source only. An ID of an expiring source that carries no
expiry keeps the stored `exp` when its UID is unchanged, and is discarded
otherwise, because an owned expiring ID without `exp` is never served
(section 6.1). That fallback is for a vendor that omits an expiry at
runtime; the harness fails a fixture that does.

`replacing_state` with a value that does not serialize is handled like an
oversized one: the old state stays, the IDs are kept and a warning is
logged.

### 4.6 A complete provider

A fictional provider, `exampleid`, shows the whole surface: a descriptor
built from config, a required and a wanted input, consent strings, a
provider state round trip, a UID-level `ext` and a refresh hint.

```rust
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct ExampleIdConfig {
    pub enabled: bool,
    #[validate(url)]
    pub endpoint: String,
    pub partner_id: u32,
    pub gvl_id: u16,
}

impl IntegrationConfig for ExampleIdConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

impl IdentityProviderConfig for ExampleIdConfig {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        ProviderDescriptor::builder(EXAMPLEID_INTEGRATION_ID.parse()?)
            .with_source("exampleid.example.com".parse()?)
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.endpoint)
                    .with_needs([Input::ClientIp, Input::UserAgent])
                    .with_wants([Input::PageUrl])
                    .with_private_state(),
            )
            .with_consent_policy(ConsentPolicy::strict().with_tcf_vendor(self.gvl_id))
            .build()
    }
}

pub struct ExampleIdProvider {
    descriptor: ProviderDescriptor,
    partner_id: u32,
}

#[derive(Serialize, Deserialize)]
struct ExampleIdState {
    signature: String,
}

#[derive(Serialize)]
struct FetchBody<'a> {
    partner: u32,
    ip: String,
    ua: &'a str,
    page: Option<&'a str>,
    gdpr: bool,
    consent: Option<&'a str>,
    signature: Option<String>,
}

#[derive(Deserialize)]
struct FetchReply {
    uid: Option<String>,
    signature: Option<String>,
    link_type: Option<u8>,
    max_age_sec: Option<u64>,
}

impl ExampleIdProvider {
    /// Builds the provider from its validated config.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityProviderError::Config`] when the config cannot
    /// describe a valid provider.
    pub fn new(config: &ExampleIdConfig) -> Result<Self, Report<IdentityProviderError>> {
        Ok(Self {
            descriptor: config.build_descriptor()?,
            partner_id: config.partner_id,
        })
    }
}

impl IdentityProvider for ExampleIdProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let consent = input.consent();
        let body = FetchBody {
            partner: self.partner_id,
            ip: input.client_ip()?.to_string(),
            ua: input.user_agent()?,
            page: input.page_url().ok().map(Url::as_str),
            gdpr: consent.gdpr_applies(),
            consent: consent.raw_tc_string(),
            signature: input.state::<ExampleIdState>()?.map(|state| state.signature),
        };
        ProviderRequest::post_json(input.endpoint()?, &body).map(Some)
    }

    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let reply: FetchReply = response.success_json()?;
        let Some(uid) = reply.uid else {
            return Ok(Resolution::no_match());
        };

        let mut id = ResolvedId::new(input.primary_source(), uid).with_atype(1);
        if let Some(link_type) = reply.link_type {
            id = id.with_ext(json!({ "linkType": link_type }));
        }
        if let Some(max_age) = reply.max_age_sec {
            id = id.with_refresh_in(Duration::from_secs(max_age));
        }

        let resolution = Resolution::found([id]);
        Ok(match reply.signature {
            Some(signature) => resolution.replacing_state(&ExampleIdState { signature }),
            None => resolution,
        })
    }
}
```

Registration uses the existing builder (`integrations/registry.rs:636`) from
an integration's `register` function, which returns
`Report<TrustedServerError>` (`integrations/lockr.rs:321-323`). error-stack
converts only `C` into `Report<C>` (`error-stack-0.6.0/src/context.rs:101`),
so the builder does that conversion once for every vendor:

```rust
impl IntegrationRegistrationBuilder {
    /// Registers a provider built by a fallible constructor. A failure
    /// becomes `TrustedServerError::Integration`, naming the integration the
    /// builder already holds.
    pub fn try_with_identity_provider<P: IdentityProvider + 'static>(
        self,
        provider: Result<P, Report<IdentityProviderError>>,
    ) -> Result<Self, Report<TrustedServerError>>;
}

Ok(Some(
    IntegrationRegistration::builder(EXAMPLEID_INTEGRATION_ID)
        .try_with_identity_provider(ExampleIdProvider::new(&config))?
        .build(),
))
```

A capture-only provider overrides one method. This fictional one reads
tokens from the API it proxies, and returns `no_match` for any response it
does not recognise:

```rust
impl IdentityProvider for ExampleCaptureProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn capture(
        &self,
        input: &CaptureInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let Ok(reply) = input.response().json::<TokenReply>() else {
            return Ok(Resolution::no_match());
        };
        Ok(Resolution::found(reply.tokens.into_iter().filter_map(|token| {
            let source = input.source_named(&token.source)?;
            Some(ResolvedId::new(source, token.value).with_expires_at(UnixTime::from_secs(token.expires)))
        })))
    }
}
```

Its descriptor declares
`.with_captures(Captures::new().with_path(Method::POST, "/integrations/examplecapture/api/token"))`
and no `resolves`, so it is never dispatched.

### 4.7 The coordinator

`IdentityCoordinator` is built per request from the registered providers and
`[ec.identity]` settings. Its dependencies (settings, registry, KV graph,
rate limiter, runtime services) travel in one `IdentityDeps` struct, because
`handle_auction` and `handle_publisher_request` already take the maximum of
seven arguments (`auction/endpoints.rs:114-122`, `publisher.rs:4433-4441`).

**The eligibility view.** Eligibility reads one merged view of stored state,
on every route:

- KV is the base when the request read it, else the cookie's last
  confirmed copy.
- Cookie entries with `k: 0` override it when their `pv` matches the base
  revision. Removals and opt-outs in the removals cookie `ts-eids-r` always
  apply (section 6.2).
- `ps.n` is the later of the cookie's and KV's. `ps.iv` is the union.
- `ps.o`: a staged opt-out (in `ts-eids-r`, not yet in KV) always counts,
  unless `ts-eids-s` stages a clear `ps.oc` later than it (section 8). A
  mirrored opt-out (a copy in `ts-eids-s`) yields to KV when this request
  read KV, so a clear in KV (section 8) is not undone by the browser's copy.
- Pending markers come from the cookie only.

**Eligibility.** A provider that declares `resolves` is eligible for this
request when all of these hold:

1. The request is a real browser (`device_signals.looks_like_browser()`,
   `adapter-fastly/src/app.rs:410`), the same gate pull sync uses.
2. Global EC consent allows EC and EID persistence (`allows_eid_persistence`,
   as `pull_sync.rs:72-77` does).
3. The descriptor's consent policy is met (section 9.1).
4. At least one claimed source has a kind the provider lists, computed per
   source:
   - **Owned by this provider.** `Initial` when there is no stored record.
     `Refresh` when there is one, valid or expired, and it is due. A stored
     record is due when it is expired, when this provider did not write it,
     when its `refresh_at` has passed, or, if it has none, when
     `acquired_at + refresh_after_sec` has passed or `acquired_at` is
     missing. A browser-seeded value whose UID matches `ps.iv` is never due
     (section 7.2). An expired record is never served (section 7.1), so the
     provider either refreshes it or returns `None`.
   - **Unowned.** `Initial` only, and only when KV has no unexpired ID for the
     source. An expired record counts as absent. Unowned sources are never
     refreshed (section 7.2).
   - Sources whose kind's needs are missing are left out. If none remain,
     the provider is skipped and the skip is logged at debug.
5. `ps.n` (next eligible time) has passed, and no sticky `ps.o` opt-out is
   set (section 8).
6. No unanswered pending marker `p` is set for the provider (section 6.2).
7. The provider's circuit breaker is closed (below).
8. The request mode allows it, subject to the restrictions on stateful
   providers and first navigations below:
   - Hold-mode providers run on any request that runs an auction: a
     navigation with matched slots, `/auction` or `/_ts/page-bids`.
   - After-send providers run only on the writer request for a returning
     EC. A new visitor's navigation runs none, so its create stays the page
     load's one write (section 5.2). Its after-send providers start at the
     next navigation.
   - A provider declared `with_private_state()` runs only on the writer
     request. In hold mode it runs on writer navigations that run an
     auction, and after send on writer navigations that do not, so a site
     whose auctions all go through `/auction` still refreshes it.
   - No provider runs on a new visitor's first navigation, in either mode.
     Its row is created before routing, carrying only browser ingest.
9. The per-visitor burst cap is not exceeded. This check comes last, just
   before dispatch, because the Fastly limiter increments on every check under
   the limit (`adapter-fastly/src/rate_limiter.rs:54-66`). The cap is
   `max_calls_per_minute` on the key `identity:<provider>:<visitor>`, where
   `visitor` is an HMAC of the full EC ID with a key derived from
   `ec.passphrase`. It is not `ec_hash`, which everyone behind one IP shares
   (section 3.4).

Pacing per visitor comes from `ps.n` and the refresh rules in step 4, not
from the limiter. The limiter only caps bursts, because its windows are 60
seconds long.

**Pre-disclosure revalidation.** Before any byte goes to a provider for a
returning EC, the request must have read the live KV row and found it live
and consenting. Eligibility steps 4 to 6 are then recomputed from that live
row, so an opt-out or removal that the cookie has not caught up with still
stops the call. That read is the writer's phase 1 read for hold mode, its
phase 4 read for after-send mode (section 5.2), or the read `/auction` and
`/_ts/page-bids` already do. A new visitor's first navigation runs no
provider (step 8), so every provider call follows a confirmed row.

**Dispatch.** For each eligible provider, the coordinator builds a
`ResolveInput` holding only the declared inputs and calls `request`. It then
checks the request before sending it:

- The scheme is `https`.
- The host is one of the descriptor's `resolves` hosts.
- No `Cookie` header is present, and every header name and value is valid.

A request that fails a check is dropped and logged as `Config`. The
coordinator sends it through `RuntimeServices::http_client().send_async`, up
to `max_concurrency` at once, with a backend first-byte timeout of the
provider's timeout (for hold mode, `min(timeout_ms, remaining budget)`,
rounded to buckets as `auction/orchestrator.rs:453-473` does). It does not
follow redirects. It reads at most `max_response_bytes`, checked against
`Content-Length` first (`pull_sync.rs:412`) and against the buffered body
(`pull_sync.rs:472-474`). It undoes `Content-Encoding` with the same cap
applied to the decoded size, then passes the `ProviderResponse` to
`response`.

**Hold-mode timing.** Hold-mode providers share one wall-clock budget,
`hold_budget_ms` (default 100 ms), measured from the first dispatch. Each
waits for `min(timeout_ms, remaining budget)`. The auction starts when all
have answered or the budget is spent.

Why 100 ms:

- It is a fifth of the navigation auction's recommended 500 ms timeout
  (`auction_timeout_ms`, `trusted-server.example.toml:336-340`). A 300 ms
  budget would take 60% of it.
- Browser-side guidance runs higher, from 100 to 500 ms. Prebid's
  `auctionDelay` defaults to 500 ms (EID fidelity spec D4), and data vendors
  recommend at least 100 or 250 ms (`adlooxRtdProvider.md:86`,
  `51DegreesRtdProvider.md:101`). Those figures include the visitor's device
  connecting to the vendor. An edge call skips that: it goes from the POP,
  often over a connection Fastly already holds open.
- Missing the budget costs that one auction the ID. A call cut off by the
  budget is not a failure: it does not count toward `ps.f` or the breaker.
  It sets `ps.n = now + hold_retry_sec` (default 300 seconds), so the next
  request does not wait on the same slow provider straight away.

The coordinator logs each provider's response time. An operator raises the
budget only when a provider's measured p95 latency at the edge exceeds it.

On the navigation, holding has a latency cost. The navigation auction is
dispatched before the origin fetch (section 3.4), so a due hold-mode
provider delays bid dispatch. In modes where the origin fetch waits for
dispatch, it also delays the page's first byte by up to `hold_budget_ms`. A
provider is due rarely (step 4), so few navigations pay this.

**Results and headers.** A hold-mode call either answers within its budget
or is cut off; no call outlives the router (section 3.4). An answer is saved
in that response's `ts-eids-s` with `k: 0`, on the writer request as on
stagers, and a writer also adds it to its mutation. So a later request reads
it from the cookie first, and a failed flush loses nothing. After-send
results exist only in phase 4, after headers are sent. The writer marks each
after-send provider with a pending marker before headers go out (section
5.2).

**Circuit breaker.** The rate-limiter trait (`ec/rate_limiter.rs:16-47`)
gains `record(key)` and `count(key)`, backed by Fastly's counter `increment`
and `lookup_count`. Only transport failures, timeouts, 5xx and 429 responses
are recorded, under `identity-fail:<provider>`. While `count` is at or above
`breaker_failures_per_minute`, the provider is skipped for every visitor.
Input-caused 4xx responses and `Malformed` back off only that visitor, so no
visitor can open the breaker for everyone by seeding bad values. Fastly
counters are kept per POP, so the breaker is too.

**Passive capture.** `IntegrationRegistry::handle_proxy`
(`integrations/registry.rs:1031-1060`) holds the `EcContext`.
`IntegrationProxy::handle` itself has no EC context
(`integrations/registry.rs:319-324`). For each provider with a capture
rule (`with_path` or `with_path_prefix`) matching the request:

- **Before `handle`.** When the descriptor's `Captures` declares `with_request_consent` and the
  request has a body, `handle_proxy` buffers it with the existing
  `collect_body_bounded` (`integrations/mod.rs:182`, 256 KiB cap), extracts
  only the named JSON pointers, and re-wraps the bytes with
  `EdgeBody::from` before calling `handle`, so the integration forwards them
  unchanged. Today the request moves into `handle` unread
  (`integrations/registry.rs:1061`), and Lockr buffers its own copy inside
  `handle_api_proxy` (`integrations/lockr.rs:208-210`), out of the
  coordinator's reach. A body over the cap or not valid JSON allows no
  positive IDs; removals still apply.
- **The consent to apply.** `consent::stricter_of(cookie, body)` is a new
  function. It combines two consent contexts field by field: any opt-out,
  missing purpose or missing vendor consent in either one wins.
- **After `handle`,** on 2xx responses only, the coordinator decodes
  `Content-Encoding` under the byte caps, builds a `CaptureInput` and calls
  `capture`.
- **It applies the result.** Removals and opt-outs always apply, whatever the
  consent, because a revocation arrives after consent is denied. Positive
  IDs apply only when eligibility steps 1 to 3 pass under the stricter
  consent, and the provider has no sticky `ps.o`, unless this result clears
  it: a capture-only provider without a GVL ID that returns IDs under
  consenting cookie and body consent stages a clear, `ps.oc = now`, beside
  its IDs in `ts-eids-s` (section 8).

The result is staged on `EcContext`. `ec_finalize_response`, which already
runs for proxy responses (`adapter-fastly/src/app.rs:909`,
`main.rs:606-614`), writes positive IDs into `ts-eids-s` and removals and
opt-outs into `ts-eids-r`, including on the unusable-consent path (section
6.2). On a writer request the result also joins the mutation.

Capture never touches `ps.n`, `ps.f` or the breaker: a proxied call that
held nothing is not a provider failure. Only an opt-out and its IDs
persist. Capture never changes provider state: a state change in its result
is ignored with a warning, on writers and stagers alike, and the harness's
conformance check fails a `capture` that returns one (section 13). This
holds whether or not the provider declares `with_private_state()`.

Lockr's API responses are already buffered on Fastly: a non-streaming send
reads the body whole, up to 10 MiB (`adapter-fastly/src/platform.rs:413-429`).
The Lockr proxy forwards the browser's `Accept-Encoding`
(`integrations/lockr.rs:259-265`), so its responses can be compressed. The
coordinator decodes them, as above.

## 5. One KV write per page load

### 5.1 Writer requests and stagers

A request is the **writer request** when all of these hold. The decision is
made once pre-route filters have allowed dispatch, because a challenged or
blocked request must not persist anything (`ec/mod.rs:91-96`). It sits where
routing already decides EC generation (`adapter-fastly/src/app.rs:846-851`,
`integrations/registry.rs:1048`) and is stored on `EcContext`:

- It is an organic route: the publisher fallback, or an integration proxy
  route.
- `Sec-Fetch-Dest` is `document`. The `Accept: text/html` fallback of
  `is_navigation_request` does not count, because `fetch()` can send it
  (`http_util.rs:105-117`).
- It is not a prefetch or prerender. `is_prefetch_request` moves to
  `http_util` as a public function so the adapter can call it.
- The request is a real browser (`adapter-fastly/src/app.rs:410`).
  Integration-proxy EC generation gains the same explicit check: today it
  is gated only by a missing KV graph (`integrations/registry.rs:1048-1049`,
  `adapter-fastly/src/app.rs:458`).

An integration-proxy writer never rotates an EC, as today
(`adapter-fastly/src/app.rs:885-890`).

The adapter captures the response status before `send_edgezero_response`
consumes the response, and passes it to phase 4 (`main.rs:512-517`, `:618`).
A returning EC's writer flushes only when that status is 2xx or 304. For any
other status it drops its mutation and dispatches no after-send provider. It
saves the answers of hold-mode calls it already made in the response's
cookie, because those calls happen before the origin status is known
(`publisher.rs:4887-4894`, `:5151-5175`). A redirect is followed by another
navigation, which recomputes everything from the same cookies.

A new visitor's create (section 5.2, phase 1, before routing) is written whatever the
status, as today, because the response sets the EC cookie.

Every other request is a **stager**: `/auction`, `/_ts/page-bids`,
integration proxy calls that are not navigations (including Lockr's API
proxy), prefetches, iframes, clients that send no `Sec-Fetch-Dest`, and every
other route. Stagers read cookies and KV but never write KV. Section 5.3
covers what they save instead.

`/_ts/api/v1/identify` stops writing too. The once-per-EC `cluster_size`
evaluation (`ec/identify.rs:107`, `ec/kv.rs:1484`) moves into the writer's
mutation. `/identify` uses a stored `cluster_size` when present and computes
one without writing when not.

**SPA views.** A single-page app changes views with JavaScript, and TS's
script reports each change to `/_ts/page-bids` (`publisher.rs:6890-6894`).
An SPA view is part of the page load that loaded the document, so
`/_ts/page-bids` is a stager. Today it ingests `ts-eids` into KV
(`publisher.rs:6925`). Under this design it stops. A visitor who stays in an
SPA has their staged changes written at their next full navigation.
Auctions keep using the staged entries from the cookie in the meantime.

**Exception: consent withdrawal.** A withdrawal writes its tombstone on
whichever request carries it, before the response is sent, exactly as today.
That includes stagers and clients the bot gate does not treat as real
browsers (`adapter-fastly/src/app.rs:446-452`). Revocation cannot wait for a
navigation that may never come. The exception covers the whole of today's
withdrawal path: a CAS tombstone with up to five attempts, then its
completion-marker write, for each withdrawn EC ID (one or two;
`ec/kv.rs:1349-1430`, `:1115-1124`; `finalize.rs:509-525`). It is the only
page-load exception to G4. The tombstone is not part of any `IdentityMutation`. A
writer whose phase 4 re-read finds the tombstone drops its mutation (section
5.4, step 1).

### 5.2 Phases of the writer request

**Phase 1, at the routing decision. No KV write.**

- Read `ts-ec`, consent, `ts-eids`, sharedId and `ts-eids-s`. Verify
  `ts-eids-s` (section 6.2).
- Decide whether this is the writer request (section 5.1).
- Capture the user agent, DNT and page URL into `EcContext` (section 4.4).
- Read the KV snapshot when section 6.3 says to. A missing row for a
  returning EC starts today's orphan recovery: confirm the absence, then
  rotate before send (`ec/finalize.rs:371-400`). The rotated EC's create
  carries this request's browser ingest, as today (`ec/finalize.rs:323`),
  and its removals and sticky `ps.o`, `ps.oc` and `ps.iv`, but never
  positive staged IDs. It is the page load's one write (section 5.6): a
  request that rotates drops the rest of its mutation and its pending
  markers, and skips phase 4. When the create is `Written`, its response
  deletes the `ts-eids-r` cookies, whose contents the create carried, and
  sets a new `ts-eids-s` from the created row. Otherwise both are kept. If the confirm
  read finds the row after all, no rotation happens and that read is the
  phase 1 snapshot (today's code writes at once there, `ec/finalize.rs:383-386`;
  here the change joins the mutation).
- For a new visitor, `generate_if_needed` creates the row before routing, as
  today (`ec/mod.rs:407-455`), so no unconfirmed ID is ever used. Today that
  create is an empty row and browser ingest is a second write in finalize
  (`ec/mod.rs:412`, `ec/finalize.rs:144`). Here the create also carries
  browser ingest from `ts-eids` and sharedId, which are read before routing,
  so it is the page load's one write.

**Phase 2, routing.**

- The coordinator computes eligible providers from the merged view.
- If the navigation runs an auction, due hold-mode providers are dispatched
  before it, after eligibility is recomputed from the phase 1 read, and
  awaited within the hold budget. Their answers feed the auction.
- For a returning EC, the coordinator calls `request` for each eligible
  after-send provider as a dry run, with `input.is_dry_run()` true. `request`
  is pure and does no I/O. A provider that returns `Some` gets a pending
  marker in phase 3. One that returns `None` is skipped (section 8). The real
  request is rebuilt in phase 4 from the live row.
- A provider declared `with_private_state()` gets no dry run, because its
  state is only in KV. When it is due on the cookie view it gets a marker,
  and phase 4 rebuilds its request with the state from the live row. A
  rebuilt `None` sets `ps.n` (section 8). This saves a phase 1 read on each
  of its refreshes.
- Any other dry run sees what KV holds only when phase 1 read it. Section
  6.3 makes phase 1 read KV whenever an after-send provider is eligible on
  the cookie view and has a candidate source that is unowned or only a
  stub.

**Phase 3, finalize, before send.**

- **New visitor.** Nothing to write: the row was created before routing
  (phase 1). `ts-ec` is set because the create was `Written`, as today
  (`ec/finalize.rs:145-149`). Phase 4 does nothing for a new visitor, and no
  provider runs on its first navigation (section 4.7, step 8).
- **Returning EC.**
  - Browser ingest from `ts-eids` and sharedId adds `FillMissing` changes to
    the mutation, instead of calling `sync_eid_cookie_updates`
    (`ec/finalize.rs:183`). It covers owned sources too, which seeds them
    for their owner (section 7.2).
  - Staged entries from `ts-eids-s` pass the flush checks (section 5.3) and
    are added to the mutation.
  - Hold-mode answers and this request's captures join the mutation.
- **Consent withdrawal** runs today's synchronous path (section 5.1).
- **Cookies.** Removals and opt-outs from this request go into a new
  `__Host-ts-eids-r-<id>` cookie (section 6.2). `ts-eids-s` is set on the HTML response only
  when needed:
  - for a new EC, from its created row
  - with the results of hold-mode providers that answered, saved with
    `k: 0`, except a provider with private state: its answer stays in the
    mutation only, because its ID and its state must land together
  - with a pending marker for each after-send provider whose dry run
    returned `Some`, only when the response status is 2xx or 304 (finalize
    already holds the response)
  - when the phase 1 read changed the cookie under the section 6.3 merge
  - when the cookie was missing, failed to open or was past its
    revalidation age

**Phase 4, after send. Returning ECs only. The only write.**

1. If the mutation is empty and no after-send provider was marked, stop. No
   read, no write.
2. If phase 1 read the row successfully and no after-send provider is
   marked, do not read again (a failed phase 1 read counts as no read, and
   phase 4 re-reads): apply the mutation to the phase 1 snapshot and, if anything
   changes, write with that generation. A concurrent change since then
   fails the CAS, as any conflict does. Otherwise re-read the live row.
   Either way, the snapshot or the live row decides:
   - A tombstone, or `consent.ok = false`: drop the mutation and stop.
   - Missing: drop the mutation and stop. The next request that reads KV
     finds the orphan (section 6.3). This covers an integration-proxy
     writer, which never rotates.
   - Failed: drop the mutation and stop. Staged entries stay in the cookie.
3. Recompute eligibility steps 4 to 6 from the live row, rebuild each marked
   provider's request, dispatch the after-send providers and turn their
   results into changes. This happens after the re-read, so no EC ID is
   disclosed before consent is confirmed (`ec/pull_sync.rs:100-110` does the
   same today).
4. Apply the mutation to the live entry (section 5.4). If anything changed,
   write it once with `IfGenerationMatch`.
5. If the write fails, on a precondition failure, Fastly's per-key rate
   limit or any other error, log and stop. It is not retried. Fastly limits
   writes to one key to about one per second, and the limit can be stricter,
   so an immediate retry after a conflict would usually fail too. Nothing is
   lost for good:
   - browser ingest is re-derived from `ts-eids` by the next navigation
   - staged entries and stateless hold-mode answers are still `k: 0` in the
     cookie
   - removals and opt-outs stay in `ts-eids-r` until a KV read shows them
     applied
   - an after-send provider's result is lost, an `invalidate` or `opted_out`
     included. The ID it rejected stays served, as it was before the call,
     until the provider is called again once its pending marker passes its
     deadline and repeats the removal. Nothing after send can save it,
     because the KV write is what failed
   - a stateful hold-mode provider's answer, state change and `ps.n`/`ps.f`
     are lost with the mutation. Its answer never went into the cookie
     (phase 3), so no ID can flush later without its state, and the next
     writer calls it again with the state KV still holds. If the vendor has
     rotated that state (a used UID2 refresh token, for example), the call
     fails, the stored ID ages out, and the browser or a later initial call
     re-seeds it

**Cost per page load.** Writes are at most one, and never retried. Reads,
compared with today:

| Navigation                                                      | Today                  | This design                                                            |
| --------------------------------------------------------------- | ---------------------- | ---------------------------------------------------------------------- |
| Runs an auction                                                 | 1 (preload)            | 1 (phase 1). Phase 4 reuses it unless an after-send provider is marked |
| No auction, registered source in `ts-eids`                      | 1 (ingest compare)     | 1 (phase 4)                                                            |
| No auction, nothing due, staged or pending, valid recent cookie | 0 or 1                 | 0                                                                      |
| An after-send provider dispatches                               | 2 (preload, pull sync) | 1 or 2: the phase 4 re-read, plus phase 1 when section 6.3 requires it |

Today's preload never skips a navigation whose auction needs the row
(`publisher.rs:3110-3116`, `:4825-4828`), and the cookie does not cache
unowned values, so such a navigation reads KV once, as today. The 6.3
trigger list states the rule.
Each `/auction` and `/_ts/page-bids` keeps its one existing read. Orphan
recovery adds the reads it adds today, in the rare case it runs.

A digest of the last-compared browser set in `ts-eids-s` could remove the
read on navigations with no auction. It is left out until measurements show
the read matters.

### 5.3 Stagers

A stager never writes KV. It saves positive changes into `ts-eids-s` and
removals and opt-outs into `ts-eids-r`, on its own response (section 6.2).
`ts-eids-s` is set in the returning-visitor branch of `ec_finalize_response`
(`ec/finalize.rs:81-127`), replacing today's ingest write at
`finalize.rs:110-112`. That branch already has the request's client EIDs
(`ec/mod.rs:568-578`) and its KV snapshot. `ts-eids-r` is also written from
`finalize_unusable_consent` (`ec/finalize.rs:457`), because a revocation can
arrive while consent is unusable but not explicitly withdrawn, and that path
returns before the returning-visitor branch (`finalize.rs:62-78`). A stager
saves:

- **The KV gap, on `/auction`.** `/auction` holds the full body set
  (`auction/endpoints.rs:298-303`) and the KV row it already reads
  (`endpoints.rs:307-311`). For each registered source in the consent-gated
  body set that the KV row lacks or holds only expired, it saves
  `{s, u, b: 1, k: 0}`. It picks the
  UID with today's ingest rules (`collect_eid_updates`,
  `ec/prebid_eids.rs:151`). It saves only absent or expired sources,
  because ingest is fill-missing (sections 3.2 and 5.4), and skips a UID that matches `ps.iv` (section
  7.2). It costs no extra KV read.
  - When the client set came from the `ts-eids` cookie rather than a body,
    nothing is saved: the next navigation ingests that cookie anyway.
  - When the KV read failed, found the row missing or found a tombstone,
    nothing is saved.
  - `/auction` returns early on consent denial, before it reads KV
    (`endpoints.rs:231-275`). Then nothing is saved.
- **Hold-mode results** on `/auction` and `/_ts/page-bids`, from providers
  without state: IDs as entries with `pv` (the revision they replace) in
  `ts-eids-s`, `ps` changes marked `sg: 1`, and invalidations and opt-outs
  in `ts-eids-r`. `/_ts/page-bids` reads KV only when its auction
  runs (`publisher.rs:7077-7090`), so it runs hold mode only then.
- **Passive capture results** on integration proxy responses (Lockr), the
  same way.
- **Cookie upkeep from the KV read** that `/auction` and `/_ts/page-bids`
  already do (section 6.3). These responses are JSON and already private, so
  cookie upkeep costs no HTML cacheability.

Providers with state run only on the writer request (section 4.7, step 8),
because state lives in KV only and a stager cannot write it. A stager that
sees one of them due leaves it to the next navigation.

**No lost EIDs (G10).** If the KV gap does not fit in the cookie's size cap,
the next `/auction` finds the same gap, since KV still lacks it, and saves it
again. An ID is delayed by a page view, never dropped, as long as the browser
keeps sending it. Today's `/auction` ingest writes the same fill-missing set
(`ec/kv.rs:146-154`), so everything it writes still reaches KV.

**Concurrent stagers.** Parallel `/auction` or `/_ts/page-bids` calls each
rewrite the whole cookie from their own copy, and the last `Set-Cookie`
wins. A saved entry can be lost this way:

- A lost KV-gap entry is saved again by the next `/auction`.
- A lost hold-mode result costs one later provider call. The per-visitor
  burst cap, one call per minute by default, makes near-simultaneous
  duplicate calls rare. The limiter is best-effort under concurrency
  (`ec/rate_limiter.rs:25-27`), so they are not impossible.
- Removals and opt-outs are never lost this way. Each batch lives in its own
  `ts-eids-r` cookie, which no other response rewrites and only a writer
  whose KV read shows it applied deletes (section 6.2), so a concurrent
  rewrite of `ts-eids-s` or a second removal cannot drop them. A capture cannot be re-derived, so this matters most for
  Lockr's `revoke-consent`.

The loss of positive entries is accepted.

**Flush checks for staged entries.** In phase 3, the writer adds a staged
entry to its mutation only when it passes all of these:

These checks apply to positive entries. Removals and staged opt-outs keep
only the binding and revision checks (section 5.6).

- The cookie opens with this EC's ID as associated data (section 6.2).
- The entry is fresh: its staged-at time `sa` is no more than
  `staged_max_age_sec` ago. Older entries are dropped from the cookie on its
  next rewrite.
- Its writer `w` is an enabled provider that currently claims the source and,
  for an owned source, owns it.
- It passes `gate_eid_updates_by_consent` (`ec/finalize.rs:268`) and the
  writing provider's consent policy, both against this request's consent,
  which may have changed since staging.
- A provider entry is dropped when `ps.o` marks that provider opted out,
  unless the cookie also stages a clear `ps.oc` later than that opt-out's
  `at`. A replayed old cookie therefore cannot re-insert an opted-out ID: its
  clear predates the opt-out.
- A provider entry becomes an `OwnerWrite` with its `pv` as the expected
  revision when its writer currently owns the source, and a `FillMissing`
  otherwise, so an unowned source keeps the fill-missing contract. The cookie
  may hold `k: 0` entries for unowned sources; it caches only owned sources
  once they are in KV. A removal from `ts-eids-r` becomes an `OwnerRemove`,
  or a `WriterRemove` when it carries no revision. A browser entry becomes a
  `FillMissing`. A staged `ps` change merges as described in
  section 6.2.

### 5.4 `IdentityMutation`

```rust
pub(crate) struct IdentityMutation {
    partner_ids: Vec<PartnerIdChange>,
    provider_state: BTreeMap<IdentityProviderId, ProviderStateChange>,
    cluster_size: Option<u32>,
}

pub(crate) enum PartnerIdChange {
    /// From browser ingest, a KV-gap entry or an unowned provider.
    /// Applies only if the source is absent or its record is expired.
    FillMissing { source: SourceDomain, record: StoredRecord },
    /// From the owning provider. Applies if the live record's revision equals
    /// `expected`, or if `expected` is `None` and the source has no valid ID
    /// that this provider wrote. A browser-seeded or legacy value never
    /// blocks the owner.
    OwnerWrite { source: SourceDomain, record: StoredRecord, expected: Option<Revision> },
    /// From `Resolution::invalidate` or `also_invalidate`. Applies only if
    /// the revision matches.
    OwnerRemove { source: SourceDomain, expected: Revision },
    /// From `Resolution::opted_out`, or a staged removal without a revision.
    /// Removes the source's record if this provider wrote it, at any
    /// revision.
    WriterRemove { source: SourceDomain, writer: IdentityProviderId },
}

/// A keyed hash of the `(uid, acquired_at)` pair a request read. Comparing
/// the UID alone is not enough: a refresh can extend expiry without changing
/// the UID. The cookie's `pv` stores the same hash, so staged entries compare
/// against the live record's hash.
pub(crate) struct Revision(RevisionHash);
```

A new visitor's create is not a mutation: `generate_if_needed` builds the
seed row before routing (section 5.2).

Applying the mutation to a live entry, in order:

1. If the live entry is a tombstone, drop everything.
2. Apply removals first: every `OwnerRemove` and `WriterRemove`. Applying a
   new `ps.o` for a provider also removes every live record that provider
   wrote, owned or unowned, at any revision. Removals are never skipped.
   An opt-out applies only when its `at` is after both the live `ps.o.at`
   and the live `ps.oc`. Then apply each staged clear `ps.oc` that is later
   than the provider's opt-out `at`, live or staged, and store it as the
   live `ps.oc`, so the next step sees the provider as not opted out and a
   later replay of the old opt-out is stale. A clear that is not later is
   dropped.
3. Apply the positive changes by their own rules. Every change written by
   a provider the live `ps.o` marks opted out is skipped, `OwnerWrite` and
   `FillMissing` alike, and so is a staged entry whose `sa` is before that
   provider's latest opt-out `at`, even after a later clear. A `FillMissing` whose
   UID matches the live `ps.iv` is skipped. Changes that no longer apply are
   dropped silently: a newer write won.
4. Apply `provider_state` changes (`n`, `f`, `o`, `iv`, `st`) and
   `cluster_size`. Drop expired records and the `ps` entries of providers no
   longer configured.
5. Validate the result with `KvEntry::validate`. If it fails, keep only the
   removals and opt-outs and validate again, so a full row can always
   shrink. If that also fails, drop the mutation and log. An invalid row is
   never written.
6. If nothing changed, skip the write.

Because the rules are checked against the live entry, a later writer can
re-apply the same changes, re-derived or still staged, without losing a
concurrent tab's newer data.

### 5.5 Orphans, collisions and deletion

Creating a new visitor's row before its cookie goes out keeps today's three
safety properties, which a deferred create would lose:

- **Withdrawals stick.** A withdrawal can only arrive with a cookie, and the
  cookie is set only after the row exists, so the tombstone always has a row
  to land on.
- **Collisions are caught.** An `AlreadyExists` on the first create re-mints
  the ID before any cookie is set, so no browser adopts another visitor's
  row.
- **Orphans are found.** A row that goes missing later is found by a KV read
  and recovered by today's rotation, before send (section 5.2, phase 1).
  Section 6.3 guarantees such a read at least every `kv_revalidate_sec`, and
  any stager that sees the row missing replaces `ts-eids-s` with a minimal
  `m: 1` cookie to force one at the next navigation.

**Deletion.** The IAB data-deletion framework, deferred today, would
hard-delete rows (`ec/kv.rs:1551-1563`). When that framework lands, it writes
a tombstone with `TOMBSTONE_TTL` (24 hours, `ec/kv.rs:38-39`) instead, using
its own unconditional writer and today's completion marker. That replaces
one delete with one write, the TTL removes the tombstone with no further
write, and every TS request that reads the row in those 24 hours treats the
identity as withdrawn. A browser that visits within those 24 hours
revalidates (section 6.3) and expires its cookie. One that returns later
finds the row missing, and section 5.6 makes a missing row expire the cookie
and rotate without carrying any positive IDs, so a deletion is never undone
through the cookie.

### 5.6 Rules that keep removals winning and markers answered

Round 4 found paths where a cookie could undo a removal or a marker could
loop. These rules close them, and take precedence over any earlier sentence
that seems to allow otherwise:

- **Removals survive every filter.** Removals and staged `ps.o` and `ps.iv`
  live in `ts-eids-r` (section 6.2). They are exempt from the consent
  re-gate, the writer-still-owns check and `staged_max_age_sec` in the flush
  checks (section 5.3), and keep only the EC binding and, when they carry
  one, the revision check. Withdrawing consent, disabling a provider or a
  week in an SPA can delay a positive ID, never a removal.
- **A config change keeps removals.** When the `cf` of `ts-eids-s` does not
  match, its positive entries are discarded and it is rebuilt from KV.
  Pending markers are kept. `ts-eids-r` carries no `cf`, so its removals
  apply against the current config unchanged.
- **A missing row empties the cookie.** Any KV read that finds the row
  missing (outside a recovery in the same request) treats it like a
  tombstone for the server set: no positive entries or markers survive, and
  `ts-eids-s` is replaced by a minimal cookie holding only `m: 1`, so the
  next navigation reads KV and recovers. `ts-eids-r` is kept. `/auction`
  saves no KV gap when the row is missing. Orphan rotation carries its
  removals and sticky `ps.o`, `ps.oc` and `ps.iv` into the new row, never
  positive staged IDs.
- **Markers are answered without busy writes.** A KV read answers a marker
  when the live row makes that provider ineligible (eligibility steps 4 to
  6), as well as by the rules of section 6.2. Phase 4 adds
  `ps.n = max(ps.n, now + 60 s)` only for a marked provider it did not dispatch for a
  reason the live row does not show, such as the burst cap or the breaker. A
  rebuilt `request` that returns `None` already sets `ps.n` (section 8). If
  the flush fails, the marker expires at its deadline as before.
- **Unowned dry runs need KV.** A dry run for a provider whose sources are
  unowned returns `Some` only when the phase 1 read showed the source missing
  and the pull-complete marker is absent. Otherwise the cookie cannot know,
  and no marker is set.
- **Staged entries trigger a reconciling read.** A cookie holding `k: 0`
  entries makes the writer read KV in phase 1. That read flips flushed
  entries to `k: 1` on this response (phase 3's "the phase 1 read changed the
  cookie" rule). Phase 4 reuses that generation only when no after-send
  provider is marked. Otherwise it re-reads the live row before any
  dispatch, so a withdrawal that lands after send is always seen.
- **Withdrawn consent never refills the cookie.** A stager whose decoded
  consent denies EID persistence never writes positive entries into
  `ts-eids-s`, so a concurrent `/auction` that started before a withdrawal
  cannot bring IDs back. It may still set removals in a new `ts-eids-r`
  cookie.
- **The cookie is rewritten only when it changes.** A request that read KV
  re-emits `ts-eids-s` only when its entries, markers or `ps` changed, or
  when the inbound `kc` is older than half of `kv_revalidate_sec`. Phase 3
  stamps `kc` only when this request read KV. This keeps `Set-Cookie` off
  most responses and narrows the window for concurrent overwrites.
- **Cluster size rides on a write.** `/_ts/api/v1/identify` is a
  cross-site, bearer-token endpoint (`ec/identify.rs:23-60`), so it cannot
  rely on TS cookies. When it reads a row without `cluster_size`, it
  computes the value for its response and does not store it. A writer whose
  phase 1 or phase 4 read finds a live row without `cluster_size` adds the
  evaluation to its mutation, run in phase 4 after send, so its KV prefix
  list (`ec/kv.rs:1460-1469`) never delays a page. That is one list and at
  most one write per EC, once, as today (`ec/kv.rs:1497-1529`); the write
  is the page load's one write.
- **The size cap always holds.** If dropping every droppable entry still
  leaves `ts-eids-s` over its cap (section 6.2), it is replaced by a minimal
  cookie with `kc = 0` and the markers only, which forces a read at the next
  navigation. A source that is stubbed on every rewrite is left as a stub,
  and the auction reads it from KV only when the navigation's auction needs
  that source.
- **EC minting on non-writer navigations.** Today a real browser gets an EC
  on any navigation `is_navigation_request` accepts, including prefetch and
  the `Accept: text/html` fallback (`adapter-fastly/src/app.rs:846-857`).
  That stays: such a request creates the row before routing, as a new
  visitor's writer does, and that create is its page load's one write. It
  runs no providers and writes nothing else.
- **Proxy capture plumbing.** `ProxyDispatchInput` gains `is_real_browser`,
  the writer decision and `IdentityDeps`. To capture, `handle_proxy` buffers
  the response body up to `max_response_bytes` (a larger body is passed
  through uncaptured), decodes a copy for `capture`, and re-wraps the
  original bytes with `EdgeBody::from(bytes)`, so the browser receives the
  response unchanged.

## 6. Storage

### 6.1 KV record

`KvPartnerId` (`ec/kv_types.rs:102`) gains optional fields. `KvEntry` gains
`ps`. All new fields use `#[serde(default, skip_serializing_if = ...)]`, so
`SCHEMA_VERSION` stays 1 and old rows read unchanged. KV item metadata, which
Fastly caps at 2,048 bytes, holds no IDs (`ec/kv_types.rs:199-219`) and is
unaffected.

`KvPartnerId` and `KvEntry` are public structs without `Default`, and about
30 `KvPartnerId { .. }` literals in 8 files would stop compiling (for example
`ec/kv.rs:160`, `:208`), as would the constructors at `ec/kv_types.rs:282`,
`:312` and `:344`. So both derive `Default` and gain constructors, and the
literals move to them. `PartnerIdUpdate` (`ec/kv.rs:75-90`) gains
`acquired_at`, so browser ingest records `at`.

```json
{
  "ids": {
    "id5-sync.com": {
      "uid": "ID5*example",
      "w": "id5",
      "at": 1760000000,
      "exp": 1767776000,
      "ra": 1760086400,
      "atype": 1,
      "ext": { "linkType": 2 }
    },
    "sharedid.org": { "uid": "example-shared-id", "at": 1759990000 }
  },
  "ps": {
    "id5": { "n": 1760003600, "f": 1, "st": "example-opaque-state" }
  }
}
```

| Field   | Meaning                                                                                                             |
| ------- | ------------------------------------------------------------------------------------------------------------------- |
| `w`     | Writer: a provider ID. Absent for browser ingest and legacy rows                                                    |
| `at`    | Unix seconds when the ID was acquired or last refreshed                                                             |
| `exp`   | Unix seconds after which the ID must not be sent                                                                    |
| `ra`    | Unix seconds after which the ID is due for refresh                                                                  |
| `atype` | Provider-supplied atype. Absent: use `[[ec.partners]].openrtb_atype`                                                |
| `ext`   | UID-level ext, at most 256 serialized bytes                                                                         |
| `ps.n`  | Unix seconds before which the provider must not be called                                                           |
| `ps.f`  | Consecutive failures, for backoff                                                                                   |
| `ps.o`  | Sticky opt-out: `{at, fp}`, when it was reported and the consent fingerprint                                        |
| `ps.oc` | When the provider's opt-out was last cleared. An opt-out reported at or before it is stale and never re-applied     |
| `ps.iv` | Keyed hashes of UIDs the owner invalidated, at most 4, so they are not re-seeded                                    |
| `ps.st` | The provider's private state, as embedded JSON (a raw value, not an escaped string) of at most 1,024 bytes. KV only |

`KvEntry` validation (`ec/kv_types.rs:375`) adds: `ext` at most 256 bytes,
`st` at most 1,024 bytes of embedded JSON, `ps` at most 32 providers.
Mutations prune the `ps` entries of providers no longer configured, and more
than 32 enabled providers is a config error (section 10.2), so the cap is
never reached in practice. With every cap reached,
eight owned sources come to about 7 KB, plus 1 KB for each provider with
state. A row normally carries one or two stateful providers. That is above the ~5 KB estimate in
`ec/kv_types.rs:40`, which is a documented estimate rather than an enforced
limit. Fastly's value limit is far larger.

The admin EC lookup returns the stored JSON verbatim (`ec/admin.rs:394-397`).
It redacts `ps.st`.

**Rollback rule.** The current reader drops unknown fields when it writes the
row again. If TS is rolled back after providers have written metadata, the
next write strips `w`, `exp` and the rest. So, for an owned source whose
provider marks the source as expiring, only records with `w` set to the owner and an
unexpired `exp` are sent to bidders from the server. Any other record for
that source (browser-seeded, or stripped by an older writer) is used only as
the input to the owner's refresh. This fails closed: an ID is re-fetched,
never sent past its real expiry.

### 6.2 The `ts-eids-s` and `ts-eids-r` cookies

Two server-owned cookies:

- **`ts-eids-s`** (`__Host-ts-eids-s`) caches provider-collected IDs for owned
  sources, holds positive entries stagers saved, and carries pending markers
  and mirrored `ps`.
- **`ts-eids-r`** is a family of up to four cookies,
  `__Host-ts-eids-r-<id>`, that hold removals and opt-outs not yet in KV.
  Each response that produces a removal or opt-out sets a new one, with a
  fresh random 8-hex-digit `<id>`, holding only its own batch. No response
  rewrites another's cookie, so neither a concurrent `/auction` rewriting
  `ts-eids-s` nor another removal in flight can lose a batch. Stagers never
  delete one. They are deleted, by name, in three cases only: by the writer
  navigation, after its phase 1 read shows a cookie's entries all applied
  or the cookie fails to open; by any request whose KV read confirms a
  tombstone; and by an orphan rotation whose create was `Written`. That keeps a capture's revocation, which
  nothing can re-derive, safe (section 5.3).

**Attributes.** Both use `Path=/; Secure; SameSite=Lax; HttpOnly`, with
`Max-Age=31536000`. The `ts-eids-r` cookies get the same lifetime so that a
batch never expires before a writer applies it; they are deleted once
applied, and their count and size caps bound them. The
`__Host-` prefix makes them host-only, with no `Domain`. They are caches of
KV, so per-host copies are fine. The repo's other server-set cookie, the
pull-complete marker, is host-only for the same reason: a `Domain` attribute
would let sibling hosts that share the passphrase replay it
(`pull_sync_marker.rs:198-201`). Host-only also stops a sibling subdomain from
reading the IDs or planting a competing cookie. A request carrying two
cookies of the same name is treated as carrying none.

**Sealed, not just signed.** These cookies are stripped before the origin
and Prebid Server (section 6.2, outbound requests), but they still travel in
every request from the browser, through any intermediary that logs headers.
A signed cookie would show server-only IDs, such as UID2 tokens and LiveRamp
envelopes, in plain text there, and to the origin if the strip ever
regressed. So each value is sealed with AES-256-GCM:
`v1.<nonce>.<ciphertext and tag>`, base64url without padding. The key is
derived from `ec.passphrase` with a namespaced label per cookie
(`trusted-server/ts-eids-s/key/v1`, `trusted-server/ts-eids-r/key/v1`),
following the pull-complete marker's pattern. The nonce is 12 random bytes
from the platform generator. The full EC ID is the associated data, so it is
authenticated but not stored:

- A cookie copied from another visitor, or left over after an EC rotation,
  fails to open and is ignored.
- The payload of `ts-eids-s` carries `kc` and `cf`. A `cf` that does not match
  the current identity config makes its positive entries be ignored and
  rebuilt from KV (section 5.6). `ts-eids-r` carries no `cf`.
- An old `ts-eids-s` replayed by its own visitor carries an old `kc`, which
  forces a KV read (section 6.3), and the read overrides it. Replay can delay
  a removal by at most `kv_revalidate_sec`.

`ec_hash` is not used for binding, because it is the HMAC of the client IP
and is shared by every browser behind one address (`ec/generation.rs:83-93`).
A cookie that fails to open or decode is ignored and rebuilt by the next
request that reads KV. The failure is logged without the value. Sealing
needs an AES-GCM implementation, a new dependency (section 14), which the
UID2 provider needs anyway (section 12.3).

**Why it is sealed.** Staged entries become KV writes, and sealing proves TS
wrote them. Without it, a visitor could plant an arbitrary ID in KV, where it
would also be sent from the server on routes without the cookie. Encryption
also keeps the IDs out of origin logs and browser extensions.

**`ts-eids-s` payload.** Compact JSON. A source is written as its index into
the sorted list of configured sources, which `cf` fingerprints, so a config
change that reorders sources also invalidates the indexes.

```json
{
  "v": 1,
  "kc": 1760000000,
  "cf": "1a2b3c4d",
  "ids": [
    {
      "s": 2,
      "u": "ID5*example",
      "w": "id5",
      "t": 1760000000,
      "e": 1767776000,
      "r": 1760086400,
      "a": 1,
      "x": { "linkType": 2 },
      "pv": "3f2a9c1d04b7e6a8",
      "sa": 1760000000,
      "k": 0
    },
    {
      "s": 4,
      "u": "example-envelope",
      "t": 1760000000,
      "sa": 1760000000,
      "b": 1,
      "k": 0
    },
    { "s": 0, "d": 1 }
  ],
  "ps": {
    "id5": { "n": 1760003600, "f": 1, "sg": 1 },
    "pull-ssp-example-com": { "p": 1760000000 }
  }
}
```

| Field         | Meaning                                                                                    |
| ------------- | ------------------------------------------------------------------------------------------ |
| `kc`          | When a request last confirmed this cookie against the KV row                               |
| `cf`          | Fingerprint of the identity config: providers, sources and owners                          |
| `m`           | A request saw this EC's row missing. The next writer reads KV in phase 1                   |
| `s`           | The source, as an index into the configured source list                                    |
| `u`…`x`       | Mirror the KV fields `uid`, `w`, `at`, `exp`, `ra`, `atype`, `ext`                         |
| `pv`          | Keyed hash of the revision this entry replaces. Absent: none                               |
| `sa`          | When the entry was staged                                                                  |
| `b`           | `1` for a browser KV-gap entry (`FillMissing`)                                             |
| `k`           | `1` when the entry is known to be in KV, `0` when staged and not yet written               |
| `d`           | A stub: the entry was dropped for size and is still in KV                                  |
| `ps.n/f/o/iv` | Mirror KV `ps`. `sg: 1` marks a staged change not yet written                              |
| `ps.oc`       | A staged opt-out clear from a capture (section 8), with its time                           |
| `ps.p`        | Pending marker: when an after-send dispatch happened whose result is not in the cookie yet |

**`ts-eids-r` payload.** Removals as `{s, w, pv?, sa}`, where `s` is the
source domain itself, never an index: `ts-eids-r` carries no `cf`, so an
index could point at another source after a config change. An entry
without `pv` removes the source's record if `w` wrote it and acquired it
before `sa`. Opt-outs as `o: {<provider>: <at>}`, and invalidated UIDs as
`iv: {<provider>: [<hash>]}`.

An entry is **applied**, and is skipped if the mutation meets it again,
when the live row shows:

- a removal with `pv`: the record is gone or at another revision
- a removal without `pv`: the record is gone, written by another writer, or
  acquired at or after `sa`
- an opt-out: KV's `ps.o.at` is at or after `at`, or KV's `ps.oc` is after
  `at`, so a cleared opt-out is never re-applied

`iv` entries never keep a cookie alive: they are applied with the cookie's
other entries and then dropped with it. The writer's phase 1 read deletes
each cookie whose entries are all applied, and any `__Host-ts-eids-r-`
cookie that fails to open, for example one sealed under an old key. A
cookie that is only partly applied is kept whole, and its applied entries
are skipped when re-applied.

Each cookie's sealed value is at most 512 bytes. A batch over that
coalesces a provider's removals into one provider-wide removal for each
source it claims, then drops its `iv` hashes (losing them allows one
re-seed, section 7.2), and then splits into a second cookie. When four
openable `ts-eids-r` cookies already exist, a response merges its batch
into the openable one with the lowest `<id>` instead of adding a fifth. If
the merge does not fit, the response rewrites all four as one
provider-wide removal and opt-out per provider, which at 32 providers fits
the 2 KB of four cookies. Those rewrites are the only remaining race: two
responses rewriting the same cookies at once, while four batches wait for
a writer.

**Staged `ps` changes** merge at flush: `n` takes the later value, `f` the
cookie's, `iv` is the union, and an opt-out from `ts-eids-r` is kept unless
it is applied or stale under the rules above. A staged clear `ps.oc` is
dropped from `ts-eids-s` once KV holds an equal or later `ps.oc`.
A mirrored `o` in `ts-eids-s` yields to KV (section 4.7).

**Pending markers.** While a marker is unanswered, the provider is not
dispatched again, and the next writer request reads KV in phase 1 (section
6.3). Until that read, the provider's cookie entries are not used in
auctions, because its after-send result may have removed them. A KV read
answers a marker when any of these hold, and the reader then clears it and
copies the result into the cookie, removals included:

- `ps.n` for the provider is at or after `p`.
- `ps.o` for the provider is set.
- A source the provider claims has `at` at or after `p`, or lost the value
  the cookie holds.
- The live row makes the provider ineligible (section 5.6).

A marker that a read does not answer by `p + timeout_ms + pending_grace_sec`
is dropped: the dispatch was cancelled or its flush failed, and the provider
is eligible again. A marker cannot block a provider for good.

**Size cap.** At most 2,048 bytes for the sealed value of `ts-eids-s`. When it
is over, entries are dropped in this order:

1. Entries with `k: 1` become stubs `{s, d: 1}`. They are still in KV. A
   stub makes the next request that needs the source read KV.
2. `ps` entries with no marker and no staged change. They are still in KV.
3. Provider entries with `k: 0`, in ascending `cookie_priority` (section
   10.1). Each one lost costs a later provider call.
4. Browser KV-gap entries (`b: 1`), last, because nothing re-derives one
   unless the browser sends it on another `/auction` (section 5.3).

Pending markers are never dropped. Each drop is logged with the provider and
source.

**Who writes them.**

- **`ts-eids-s`:** the writer request's HTML response only when needed
  (section 5.2, phase 3), because any HTML response with `Set-Cookie` becomes
  `private, max-age=0` (section 3.4); and stagers, whenever they save an
  entry or their KV read changes the cookie (section 5.6). Their JSON
  responses are already private.
- **`ts-eids-r`:** any response that produces a removal or opt-out, on any
  route, including the unusable-consent path (section 5.3), adds a new
  cookie for its batch; and the writer navigation's HTML response deletes
  the cookies its phase 1 read showed applied.
- **A consent withdrawal, or any KV read that finds a tombstone,** sets
  `Max-Age=0` on `ts-eids-s`. Once the tombstone is confirmed (written by
  this request, or read), the `ts-eids-r` cookies are pointless and are
  deleted too, because the tombstone removes everything. If the
  withdrawal's tombstone write failed, they are kept.

The browser shim never reads or writes either cookie. Both join `ts-ec`,
`ts-eids` and sharedId in the list of identity cookies the template cache
refuses as cache keys (`cookies/template_cache_policy.rs:35`), so no operator
can split the shared template cache per visitor with them.

**Outbound requests.** TS's identity cookies never reach third parties. One
list, `TS_IDENTITY_COOKIE_NAMES` (`ts-ec`, `ts-eids`, `__Host-ts-eids-s`,
every `__Host-ts-eids-r-` cookie by prefix, `ts-ec-pull-complete`), is stripped from Prebid Server
requests and never added to proxy or provider requests. The publisher origin
is not a third party, but it gets only the small ones:

- **Publisher origin. ID cookies stripped.** The publisher proxy removes
  `ts-eids`, `__Host-ts-eids-s` and every `__Host-ts-eids-r-` cookie
  (`TS_ORIGIN_STRIPPED_COOKIE_NAMES`) from the `Cookie` header it forwards
  (`publisher.rs:3124-3147`), and drops the header when nothing is left.
  `ts-ec` and `ts-ec-pull-complete` still reach the origin, as today,
  because publishers' servers may read `ts-ec`. Without the strip the ID cookies
  add up to about 5.5 KB to every origin request, and up to 2 KB more while
  removal batches wait, and common origin
  servers reject a header line over about 8 KB with 400 or 431, which would
  fail the page itself. Stripping `ts-eids` from the origin is a behaviour
  change: an origin that read it must read `x-ts-eids` or ask TS instead. A
  metric still reports the inbound `Cookie` header size (section 14).
- **Prebid Server.** Stripped in the Prebid transport before the
  consent-forwarding mode is checked (`integrations/prebid.rs:2091-2108`), so
  it applies in every mode, including the early return that forwards the raw
  header. A `Cookie` header that is not valid UTF-8 is dropped, not
  forwarded. The call site passes every `Cookie` value, not one.
- **Integration proxies.** They already use header allowlists without
  `Cookie` (`integrations/lockr.rs:245`, `didomi.rs:258`, `permutive.rs:259`).
- **Identity providers.** The coordinator rejects any request with a `Cookie`
  header (section 4.7).

The strip iterates every `Cookie` field, as `gpt_diagnostics.rs:394-416` does,
and drops the header when nothing is left. Only the routes of section 6.3
open the cookies, lazily, at most once per request, keeping the result on
`EcContext`. Assets and other routes never decrypt them: the origin and
Prebid Server strips match cookie names only. The derived keys, `cf` and
the source index are computed once per settings load, not per request.

### 6.3 Read order

For a request with a valid EC that uses identity (the writer request,
`/auction`, `/_ts/page-bids`, a proxy capture, or any request dispatching a
hold-mode provider):

1. If `ts-eids-s` opens, use its IDs for owned sources (skipping expired
   entries, stubs and the entries of providers with an unanswered marker),
   its staged entries, and its `ps` for eligibility (section 4.7). Whether
   or not it opens, apply `ts-eids-r` on top: its removals hide the records
   they name, from the cookie view and from KV, and its opt-outs block their
   providers.
2. **The writer's phase 1** reads the KV snapshot when any of these hold:
   - `ts-eids-s` is missing, fails to open, or has a `cf` that does not
     match
   - `now - kc` exceeds `kv_revalidate_sec`
   - `m` is set
   - any `ts-eids-r` cookie is present, so the writer can delete the
     applied ones
   - any pending marker is unanswered
   - the navigation's auction needs a source that is only a stub
   - a hold-mode provider is due, so the read is its pre-disclosure
     revalidation
   - an after-send provider without private state is eligible on the
     cookie view and has a candidate source that is unowned or only a stub,
     so its dry run sees what KV holds (section 5.2). A valid pull-complete
     marker removes a generic-pull provider's sources from the cookie-view
     candidates, as today, and step 3's cookie-only `ps.n` keeps this from
     firing on every navigation.
   - the navigation runs an auction that needs the row, as today's preload
     decides (`auction_needs_row`, `publisher.rs:3110-3117`). Today that is
     every auction navigation with unowned bidstream sources, because the
     cookie caches no unowned values
   - consent is withdrawn and the tombstone path needs the row, as today

   `/auction` and `/_ts/page-bids` keep their existing reads and gain no new
   triggers.

3. **Any request that read KV** merges the two as below and sets `kc = now`.
   It re-emits the cookie only when the merge changed it or the inbound `kc`
   is older than half of `kv_revalidate_sec` (section 5.6):
   - A tombstone, or `consent.ok = false`, empties the server set and expires
     the cookie.
   - A missing row, unless this request is about to recover it, replaces
     `ts-eids-s` with a minimal cookie holding only `m: 1`: no positive
     entries and no markers (section 5.6).
   - For each owned source, the KV record wins, with one exception: a staged
     provider entry that `OwnerWrite` would still apply (its `pv` matches, or
     it has no `pv` and KV holds only a value the provider did not write)
     stays `k: 0`.
   - A `k: 1` entry whose source KV no longer holds, or holds at a different
     revision, is dropped or replaced.
   - A staged entry KV already holds becomes `k: 1`. A `b: 1` entry is dropped
     once KV holds any value for its source.
   - A stub is replaced by the KV record.
   - Answered pending markers are cleared, and `m` is cleared when the row is
     present.
   - A provider the row shows ineligible because all its unowned candidate
     sources are present gets `ps.n = now + refresh_after_sec` in the
     cookie only, stored with the KV `n` it saw. A later read that shows a
     different KV `n` (an `invalidate` sets `n = now`, for example) drops
     the cookie-only value. Owned sources need no such value: the cookie
     caches them with their `ra` and `exp`, so the cookie view already knows
     when they are due.
   - On the writer only, each `ts-eids-r` cookie whose entries the row shows
     all applied is deleted. Stagers leave the `ts-eids-r` cookies alone.

   The writer's HTML response carries the result only under phase 3's rules,
   which include "the phase 1 read changed the cookie".

Missing cookie, KV present: the writer request reads KV and rebuilds the
cookie. The visitor loses nothing.

## 7. Auction merge and ownership

### 7.1 Merge

`merge_auction_eids` (`auction/endpoints.rs:505`), and the navigation and
page-bids merge (`publisher.rs:5547-5568`), take a third input: the server
set. It holds IDs from `ts-eids-s` (including staged entries) and KV, with
expired entries, stubs and removed entries left out. The server set requires
what today's KV path requires: `ec_allowed()`, and a live consenting row
whenever the request read KV (`endpoints.rs:446-460`). Each server-side ID
must also pass its owning provider's consent policy for this request, so the
sharing and targeted-advertising opt-outs and GPC stop it even when the
generic gates pass.

For each source, the value sent to bidders is:

| Source kind | Browser sent the source                     | Browser did not send it                                                       |
| ----------- | ------------------------------------------- | ----------------------------------------------------------------------------- |
| Owned       | the owner's valid value, else the browser's | the owner's valid value, else a browser-seeded value if the IDs do not expire |
| Unowned     | the browser's value (today's rule)          | the server value, if any (today's rule)                                       |

"The owner's valid value" means a record with `w` set to the owner that is
not past its `exp`. A browser-seeded record never beats the browser's
current value. It may be stale, which is the problem the EID fidelity work
fixed (D9). For a provider whose IDs expire, a browser-seeded record is never
sent from the server at all (section 6.1).

A server value uses its stored `atype`, else `[[ec.partners]].openrtb_atype`,
and its stored UID-level `ext`. A value the provider wrote also gets the
EID-level `inserter`, `matcher` and `mm` the provider declared for that
source (`with_provenance`, section 4.3). A browser value keeps the browser's
own EID-level fields, as the EID fidelity work requires. Sources with `bidstream_enabled = false` are
left out, as `resolve_partner_ids` does today (`ec/eids.rs:36`).

The merged set is consent-gated (`gate_eids_by_consent`) and reported in
`x-ts-eids` exactly as today, with the same 4 KB cap. `x-ts-eids` therefore
shows server-only IDs. The calling script can read it, and so can any page
script that wraps `fetch`, so `HttpOnly` on `ts-eids-s` does not hide server
IDs from the page. The header stays for validation and troubleshooting, and
this exposure is accepted.

One shared servability check covers expiry, the rollback rule, the owner's
consent policy, and opt-outs: a record whose writer has `ps.o` set, in KV or
in `ts-eids-r`, is never served, even before the removal reaches KV. `resolve_partner_ids` (`ec/eids.rs:36`), the admin view
(`ec/admin.rs:485`) and `/_ts/api/v1/identify` all use it. `/identify` reads
`entry.ids` directly today (`ec/identify.rs:99-103`), so it switches to the
check. The stale module comment at `ec/eids.rs:1-5` is corrected. So expired
IDs are never returned by any of them (G7).

### 7.2 Ownership semantics

- **Owned source.** The owner is authoritative. It refreshes when due
  (section 4.7, step 4), and its valid value wins in the auction. Browser
  ingest and KV-gap entries may only fill the source when it is absent or
  its record is expired.
  That seeds it: the owner treats a value it did not write as due, and
  refreshes it at once. No other provider may claim the source (section
  10.2).
- **Invalidated values are not re-seeded.** When the owner invalidates a
  value, its keyed hash goes into `ps.iv`. Browser ingest and KV-gap entries
  skip a UID in `ps.iv`, so the browser cannot re-seed the value the owner
  just rejected, and no invalidate-reseed-refresh loop starts.
- **Unowned source.** Any provider claiming it, and the browser, write with
  `FillMissing` only. Unowned sources are never refreshed. This is exactly
  today's pull-sync contract. The auction is browser-first.
- **Batch sync.** A batch-sync update to an owned source applies only when
  the source is absent (`ec/batch_sync.rs:247`). It never replaces the
  owner's value. Unowned sources keep today's batch-sync behaviour.

## 8. Errors and outcomes

Every effect below goes into the mutation (writer request), or a stager's
cookies: positive changes into `ts-eids-s`, removals and opt-outs into
`ts-eids-r`.

| Result or error                                                                | Stored IDs                                                            | `ps`                                                                                  | Log   |
| ------------------------------------------------------------------------------ | --------------------------------------------------------------------- | ------------------------------------------------------------------------------------- | ----- |
| `Resolution::found`                                                            | `OwnerWrite` or `FillMissing` per source                              | `f = 0`; `st` replaced or cleared when the resolution says so                         | debug |
| `Resolution::no_match`                                                         | unchanged                                                             | `n = now + retry_in` (default 24 h, max 7 d)                                          | debug |
| `Resolution::invalidate`, `also_invalidate`                                    | `OwnerRemove` with the revision the request read                      | UID hash added to `iv`; `n = now`: eligible for `Initial`                             | info  |
| `Resolution::opted_out`                                                        | `WriterRemove` for every record this provider wrote, owned or unowned | `o = {at: now, fp}`, `st` cleared                                                     | info  |
| `with_retry_in` on any resolution                                              | as above                                                              | `n` is at least `now + retry_in`                                                      | debug |
| `request` returns `None`, or `InputUnavailable`                                | unchanged                                                             | `n = now + refresh_after_sec`                                                         | debug |
| A recorded `InvalidState`, whatever the provider returned                      | as the result says                                                    | `st` cleared, `f` unchanged, `n = now + refresh_after_sec` unless the result sets `n` | warn  |
| Hold-mode budget spent before the answer                                       | unchanged                                                             | `n = now + hold_retry_sec`                                                            | debug |
| `Rejected` with a 4xx other than 429, `Config`, `UndeclaredInput`, `Malformed` | unchanged                                                             | `f += 1`, `n = now + min(1 h × 2^(f-1), 24 h)`                                        | warn  |
| `Rejected` with 429 or 5xx, `Transport`, timeout                               | unchanged                                                             | `f += 1`, `n = now + min(5 min × 2^(f-1), 1 h)`; breaker `record`                     | info  |
| Breaker open                                                                   | not dispatched                                                        | unchanged                                                                             | debug |
| Capture: any result or error                                                   | IDs and removals as above                                             | only `o`; never `n`, `f` or the breaker                                               | debug |

**Sticky opt-outs.** `ps.o = {at, fp}` records a provider-reported opt-out
and the consent fingerprint at that time. Server-side calls do not carry the
vendor's own opt-out cookie, so `o` is TS's only record of it. Applying it
removes every record the provider wrote (section 5.4), and while it is set
no record that provider wrote is served (section 7.1). It is cleared only by
an affirmative re-grant for this provider:

- **A provider with a GVL ID:** a TC string whose `LastUpdated` is after `at`
  and that grants the provider's vendor and purposes.
- **Any provider, under GPP:** the applicable US section showing no sale,
  sharing or targeted-advertising opt-out for the signals the provider's
  policy reads, where the fingerprint at `at` showed one. GPP carries no
  creation time, so the clear is by section values.
- **A capture-only provider without a GVL ID, such as Lockr:** a later 2xx
  capture that returns IDs, under consenting cookie and body consent. That
  is the vendor issuing again after the visitor re-consented through it.
  The capture's proxy request is a stager, so it stages `ps.oc` beside the
  IDs in `ts-eids-s`, and the next writer applies the clear before the IDs
  (section 5.4). If a concurrent `/auction` overwrites that cookie, both
  are lost together, and the next capture stages them again.
- **An operator,** through the admin endpoints.

A change of jurisdiction, a newly present GPP string, or a privacy signal the
provider relaxes never clears it. A clear is written to KV, and the
browser's mirrored copy yields to it (section 4.7). A consent withdrawal's
tombstone removes everything, provider opt-outs included: after it, the
visitor starts again with a new EC.

The **consent fingerprint** is the first 16 hex digits of an HMAC, keyed from
`ec.passphrase`, over the consent signals the provider's policy reads:

- its TCF purpose bits and vendor bit
- the US sale, sharing and targeted-advertising opt-outs it does not relax
- GPC and DNT, unless relaxed

It is not computed over the raw consent string, which changes whenever a CMP
re-saves it.

Rules:

- No error ever deletes a still-valid ID. Only `Resolution::invalidate` and
  `also_invalidate` (by revision) and `opted_out` (everything the provider
  wrote) delete.
- An expired ID is not permission to call. Eligibility still applies.
- Errors are logged with provider ID, outcome and status only (section 9.3).
- A provider whose IDs expire and whose refresh keeps failing loses its ID
  at `exp`, not before.

## 9. Security and privacy

### 9.1 Consent

- **Before dispatch:** global EC consent, EID-persistence consent, and the
  descriptor's `ConsentPolicy` (section 4.3). "Where TCF applies" means
  `gdpr_applies` or a jurisdiction of `Gdpr` or `Unknown`, matching
  `consent_allows_server_side_auction` (`consent/mod.rs:298-308`). Each check
  maps to a signal TS decodes:
  - TCF purposes and vendor consents (`consent/tcf.rs:145`). A small
    `has_vendor_consent` helper is new.
  - The US sale opt-out, from GPP US sections (`consent/gpp.rs:74`, `:125`)
    or the US Privacy string (`consent/us_privacy.rs:50-63`).
  - The US sharing and targeted-advertising opt-outs. This work extends the
    GPP decoder to extract them next to `decode_us_sale_opt_out`
    (`consent/gpp.rs:125`). The `iab_gpp` crate already exposes
    `sharing_opt_out` in the US National and California sections, and
    `targeted_advertising_opt_out` in the US National section and the state
    sections that define it.
  - GPP section 6, the US Privacy string inside GPP, which the decoder skips
    today (`consent/gpp.rs:112`), and the US National section's GPC and
    known-child fields. This work decodes them too. A known child without
    consent blocks every provider.
  - `Sec-GPC` (`consent/extraction.rs:25-50`).
  - DNT, captured in phase 1 (section 4.4). TS reads it today only for the
    auction (`auction/routing.rs:336`).
- **Fail closed.** `strict()` blocks when a raw TC, GPP or US Privacy string
  is present but has no decoded form, for example in proxy mode or after a
  decode failure (`consent/mod.rs:104-127`, `:197-210`).
- **Before disclosure:** the live-row read and eligibility recomputation
  described in section 4.7.
- **At write:** the CAS on that read's generation.
- **At flush and on every cookie write:** staged positive entries, and IDs
  written into `ts-eids-s`, pass the generic gate and the writing provider's
  policy against the current consent (section 5.3). Removals and opt-outs
  never need consent to apply (section 5.6).
- **In the bidstream and `/identify`:** server-side IDs pass the owner's
  policy (section 7.1).
- **On withdrawal:** the tombstone is written synchronously on whichever
  request carries it, and the response expires `ts-eids-s` (section 5.1).

### 9.2 Outbound requests

- HTTPS only. Redirects are not followed.
- The host must be one of the descriptor's `resolves` hosts.
- Providers build headers from scratch. The coordinator rejects any request
  with a `Cookie` header, and TS's identity cookies are stripped from Prebid
  Server requests. The publisher origin receives `ts-ec` but not the ID
  cookies (section 6.2).
- Providers get only the inputs their descriptor declares (section 4.4).
  Only generic pull receives the EC ID. Location is coarse (country and
  subdivision), page URLs carry no query string, and the consent view
  carries no decoded jurisdiction, though the raw GPP string still shows the
  applicable state section (section 4.4). A URL path can still carry
  identifiers, so most providers should want `PageUrl` rather than need it.
- The client IP comes only from the platform's client-IP API. A provider that
  needs it in `X-Forwarded-For` sets it from `ResolveInput::client_ip`.
  Client-supplied forwarding headers are never copied.
- Response bytes are capped before and after decompression.

### 9.3 Logging and data handling

- Never log UIDs, envelopes, HEMs, provider state, consent strings, cookie
  values, or provider URLs with a query string. EC IDs go through `log_id`
  (`ec/mod.rs:63`).
- Provider errors are logged with `current_context()` only, never with
  `{:?}` of the report, whose attachments could carry a URL or body.
- `ProviderRequest`, `ProviderResponse`, `ResolveInput`, `CaptureInput`,
  `StoredId`, `ResolvedId`, `ProviderConsent` and `HashedEmail` have
  redacting `Debug` implementations. `ConsentContext` gets one too, because
  it derives `Debug` over the raw consent strings today
  (`consent/types.rs:109`).
- The Lockr proxy logs only the path of the upstream URL, not its query
  (`integrations/lockr.rs:202-204`).
- Hashes of identifiers (the rate-limit visitor key, `pv`, `ps.iv`, the
  consent fingerprint) are HMACs with keys derived from `ec.passphrase`, so
  they cannot be linked to an EC ID without the key.
- `CaptureInput` never exposes request bodies or request headers. Proxied
  requests can contain an email or HEM. TS forwards them, and reads from a
  body only the consent pointers a descriptor declares (section 4.7). It
  never reads anything else for identity.
- Provider state stays in KV. It is never sent to bidders, put in a cookie or
  returned by the admin lookup.

### 9.4 Caching

Responses that set `ts-eids-s` follow the same cookie-privacy rule as every
cookie-bearing response: `private, max-age=0`, with edge-cache headers
removed (`response_privacy.rs:122-145`). The Fastly terminal hook re-applies
it after late mutations (`adapter-fastly/src/main.rs:745`). Section 5.2 keeps
`ts-eids-s` off returning-visitor HTML except when something is due, changed
or stale.

## 10. Configuration

### 10.1 Layers

```toml
# Coordinator policy: vendor-neutral, one place for every timing choice.
[ec.identity]
# Replaces ec.pull_sync_concurrency. The old key is accepted and deprecated.
max_concurrency = 4
# Shared wall-clock budget for all hold-mode providers, in milliseconds.
# Default 100 (section 4.7 explains why).
hold_budget_ms = 100
# After a call is cut off by the hold budget, wait this long before the
# next. Default 300.
hold_retry_sec = 300
# Per provider response, before and after decoding.
max_response_bytes = 65536
# Seconds a pending marker outlives its provider's timeout. Default 300.
pending_grace_sec = 300
# Staged cookie entries older than this are not written. Default 604800.
staged_max_age_sec = 604800
# A cookie not confirmed against KV for this long forces a KV read.
# Must be below the 24-hour tombstone TTL. Default 3600.
kv_revalidate_sec = 3600

[ec.identity.providers.id5]
# "hold" or "after_send". Default "after_send". ID5 keeps private state, so
# it runs only on the writer navigation: before the auction when that
# navigation runs one, after send when it does not (section 4.7, step 8).
mode = "hold"
# Hold mode: min(timeout_ms, remaining hold budget). Default: hold_budget_ms
# in hold mode, 1000 after send.
timeout_ms = 80
# Narrows the sources this provider may write. Default: every source its
# descriptor declares. Narrowed here to the one source registered below.
sources = ["id5-sync.com"]
# Refresh interval when the provider gives no refresh hint, and the wait
# after a skipped call. Default 86400.
refresh_after_sec = 86400
# Burst cap per visitor, enforced in 60-second windows. Default 1.
max_calls_per_minute = 1
# Skip the provider for every visitor once this many transport failures,
# timeouts, 429s or 5xx responses happen within one 60-second window.
# Default 50.
breaker_failures_per_minute = 50
# Lower is dropped first when ts-eids-s exceeds its cap. Default 0.
cookie_priority = 10

# Source policy: the existing table, with one new field.
[[ec.partners]]
name = "ID5"
source_domain = "id5-sync.com"
openrtb_atype = 1
bidstream_enabled = true
# New. A provider ID that owns this source. Absent: unowned.
owner = "id5"

# Vendor protocol: the integration's own table, defined by its own spec.
[integrations.id5]
enabled = true
partner_id = 1234
```

`[ec.identity]` and each `[ec.identity.providers.<id>]` table are optional.
A provider with no table uses the defaults.

### 10.2 Validation

Deploy-time validation (`validate_settings_for_deploy`, `config.rs:249-262`)
parses integration configs but never builds providers. Each identity
integration gets a line in a new `validate_identity_integration::<T>` list
next to today's per-type list (`config.rs:303-332`). That function parses the
config as `T: IdentityProviderConfig` and calls `build_descriptor`, which
needs no secrets and returns the same descriptor the running provider
stores. A separate pass builds the descriptors of `[[ec.partners]]` pull
providers. The identity check runs next to
`PartnerRegistry::validate_config_for_deploy` (`config.rs:260`) in
`ts config validate` and `ts config push`, and in
`validate_settings_for_runtime` at startup (`config.rs:270-280`). It applies
each provider's `sources` narrowing before checking ownership.

Each of these is an error:

- `owner` names a provider that is not enabled.
- `owner` names a provider whose descriptor does not include that source.
- A descriptor fails to build (section 4.3).
- An enabled provider claims a source with no `[[ec.partners]]` entry.
- An enabled provider claims a source owned by a different provider. Use
  `sources` to narrow one of them. For example, when native ID5 owns
  `id5-sync.com`, Lockr is narrowed to the sources no native provider owns.
- `sources` names a source the provider's descriptor does not declare, or
  narrows the provider to no source at all.
- Two providers have the same ID (section 10.4).
- A capture rule lies outside the registering integration's proxy routes.
  `IntegrationRegistry::with_plan` reports it at startup (section 4.3).
- `[ec.identity.providers.<id>]` names an unknown provider.
- `mode` is not `hold` or `after_send`. A provider with state runs only on
  the writer request in either mode: in hold mode on writer navigations that
  run an auction, and after send on those that do not (section 4.7, step 8).
- More than 32 providers are enabled, generic pull partners included. A KV
  row holds `ps` for at most 32 (section 6.1).
- `timeout_ms` is greater than `hold_budget_ms` for a hold-mode provider.
- `hold_budget_ms` is over 1,000, `max_concurrency` is 0,
  `refresh_after_sec` is 0, or `kv_revalidate_sec` is not below 86,400.

Validation never calls a provider. Live approvals, such as a LiveRamp
placement or an approved origin, stay on the rollout checklist of each vendor
spec.

**Secrets.** A provider's secret fields, such as UID2's API key and client
secret, are `Redacted<String>` config fields. At rest the config holds their
secret-store key names, and they are resolved in memory before runtime
settings are built (`secret_resolution.rs`). Each identity integration adds
its secret paths to `TrustedServerAppConfig::secret_fields()`
(`config.rs:136-175`). `build_descriptor` must never read a secret field,
because deploy validation runs before resolution and would see key names.
The harness's `check_descriptor_ignores_secrets` builds the descriptor from
a config whose secrets are placeholder key names and from one with resolved
secrets, so a descriptor that depends on a secret fails the test (section
13).

### 10.3 Registration

`IntegrationRegistration::builder(ID)` gets
`.with_identity_provider(Arc<dyn IdentityProvider>)`, next to `.with_proxy()`
(`integrations/registry.rs:663`), and
`.try_with_identity_provider(Result<P, Report<IdentityProviderError>>)`,
which wraps a constructor's result for the common case (section 4.6). An
integration can register more than one provider. The registry exposes `identity_providers()` to build the
coordinator. `handle_proxy` offers a proxied response to every provider whose
descriptor has a capture rule matching it (section 4.7). A compile-time guard,
`const _: Option<&dyn IdentityProvider> = None;`, keeps the trait
object-safe.

### 10.4 Pull sync on the trait

Each `[[ec.partners]]` entry with `pull_sync_enabled = true` registers a
built-in `GenericPullProvider`. Its ID is `pull-` plus the source domain with
dots replaced by `-` (for example `pull-ssp-example-com`). An ID that would
exceed 63 characters is cut and given an 8-character hash suffix of the full
domain, so IDs stay unique and valid.

Unchanged:

- The protocol: `GET <pull_sync_url>?ec_id=<id>`, bearer `ts_pull_token`,
  response `{"uid": "..."}`, the 64 KiB cap and the
  `pull_sync_allowed_domains` allowlist.
- Fill-missing only, unless the operator sets `owner`.
- The hourly rotation of partner order, which keeps dispatch fair under
  `max_concurrency` (`pull_sync.rs:143-156`).
- The pull-complete marker, which stays the signal that every pull partner
  already has an ID (section 6.3).

Its descriptor, built from the `[[ec.partners]]` entry:

- One source, the entry's `source_domain`. Its IDs do not expire.
- `resolves` with the `pull_sync_allowed_domains` as hosts and the
  crate-private `with_ec_id()`, because the protocol is keyed by the EC ID.
- A consent policy that blocks exactly what pull sync blocks today. The
  migration parity tests (section 13) pin it.
- Default `after_send`, and unowned.
- Optional `owner = "pull-<domain>"` makes it authoritative, and the legacy
  `pull_sync_ttl_sec` becomes its `refresh_after_sec`.
- `pull_sync_rate_limit` (per hour) maps to
  `max_calls_per_minute = max(1, ceil(pull_sync_rate_limit / 60))`, which is
  what the limiter enforces today. A `[ec.identity.providers.<id>]` table
  overrides it.

What changes:

- It is dispatched only on the writer request for a returning EC. Today it
  runs after any response whose request loaded a consenting row
  (`pull_sync.rs:86`, `:166-174`), including a new visitor's first
  navigation. New visitors now reach pull partners from their second page
  load.
- Its results go into the single flush. Today it makes its own write.
- Errors back off through `ps` (section 8). Today a failing partner is
  retried on every eligible request, within the rate limit.
- The rate-limit key moves from `pull:<domain>:<ec_hash>`
  (`pull_sync.rs:373`) to the per-visitor key of section 4.7. Counters reset
  at deploy.

`dispatch_pull_sync` and `build_pull_sync_context` are removed.
`run_edgezero_pull_sync_after_send` (`adapter-fastly/src/main.rs:618`)
becomes the coordinator's phase 4 entry point.

### 10.5 Overlap with #777

The module stack in #777 replaces `[integrations.<id>]` with
`[<type>.<id>]` (for example `[identity.lockr]`) and adds `ts config migrate`.
Under that stack only the vendor protocol layer moves.
`[ec.identity.providers.<id>]` is keyed by the same provider ID and is
unaffected. Whichever lands second adapts its config paths. This spec is
written against `main`.

## 11. Adapters

KV identity runs only on Fastly. Axum, Cloudflare and Spin build an
`EcContext` for consent-gated routes, but have no `KvIdentityGraph`, no EC
finalize and no EC generation (`publisher.rs:6896-6899`).

This design changes nothing on them except the Prebid Server and origin cookie strips, which
apply on every adapter. Providers are never dispatched there, and their
`/auction` and `/_ts/page-bids` keep resolving client EIDs without KV. When
EC identity is ported to them (#777 rows 52 to 54), phase 4 runs after send
where the platform allows it (Cloudflare's `waitUntil`), and before the
response otherwise.

## 12. Vendor examples

These examples were written against the API of sections 4.2 to 4.6 to test
it. They are illustrations of the API, not the vendors' real code, and the
framework does not depend on any detail in them. Endpoints, payloads,
consent parameters, size limits and semantics such as what Lockr's
revocation covers are decided in each vendor's implementation plan, which
replaces any shape marked "assumed" below. `IntegrationConfig` impls are
omitted; they match section 4.6. Endpoints come from operator config, shown with
`example.com` hosts. Section 12.5 lists what writing them changed in the
API.

### 12.1 LiveRamp ATS (active resolution, expiring IDs)

LiveRamp exercises refresh of an existing ID, expiring IDs, region-dependent
lifetimes, initial resolution that needs a HEM, and headers built from
inputs. The request parameters come from #1245 §9; the response shape and the
consent parameter names are assumed.

```rust
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct LiveRampConfig {
    pub enabled: bool,
    /// For example `https://ats.example.com/api/identity/v2/envelope`.
    #[validate(url)]
    pub endpoint: String,
    /// LiveRamp placement ID. An operator identifier, not a secret.
    pub placement_id: String,
    /// Countries the placement is approved for, as ISO 3166-1 alpha-2.
    pub approved_countries: Vec<String>,
}

impl IdentityProviderConfig for LiveRampConfig {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        ProviderDescriptor::builder(LIVERAMP_INTEGRATION_ID.parse()?)
            .with_source("liveramp.com".parse()?)
            .with_expiring_ids()
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.endpoint)
                    .with_needs([Input::ClientIp, Input::Region])
                    .with_needs_for(ResolveKind::Initial, [Input::HashedEmail]),
            )
            .with_consent_policy(ConsentPolicy::strict().with_tcf_vendor(LIVERAMP_GVL_ID))
            .build()
    }
}

pub struct LiveRampProvider {
    descriptor: ProviderDescriptor,
    placement_id: String,
    approved_countries: Vec<String>,
}

/// Whether the call was an initial lookup or a refresh. Carried from
/// `request` to `response`.
#[derive(Serialize, Deserialize)]
enum LiveRampCall {
    Initial,
    Refresh,
}

/// Assumed shape: V2 returns several envelope products.
#[derive(Deserialize)]
struct EnvelopeReply {
    envelopes: Vec<EnvelopeProduct>,
}

#[derive(Deserialize)]
struct EnvelopeProduct {
    #[serde(rename = "type")]
    product_type: u32,
    source: String,
    value: String,
}

/// The regular ATS envelope (#1245 §9).
const ATS_ENVELOPE_TYPE: u32 = 19;
const ATS_ENVELOPE_SOURCE: &str = "envelopeLiveramp";
/// Identifier types (#1245 §2): 4 is a hashed email, 19 an envelope refresh.
const IT_HASHED_EMAIL: &str = "4";
const IT_ENVELOPE: &str = "19";

impl IdentityProvider for LiveRampProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let region = input.region()?;
        if !self.approved_countries.iter().any(|country| country == region.country()) {
            return Ok(None);
        }

        let source = input.primary_source();
        let (call, identifier_type, identifier_value) = match input.stored(source) {
            Some(stored) if input.is_refresh(source) => {
                (LiveRampCall::Refresh, IT_ENVELOPE, stored.uid().to_owned())
            }
            _ => (LiveRampCall::Initial, IT_HASHED_EMAIL, input.hashed_email()?.to_hex()),
        };

        let consent = input.consent();
        let mut request = ProviderRequest::get(input.endpoint()?)
            .with_query("pid", &self.placement_id)
            .with_query("it", identifier_type)
            .with_query("iv", &identifier_value)
            .with_query("atype", "3")
            .try_with_call_context(&call)?
            .try_with_header("origin", &format!("https://{}", input.publisher_domain()))?
            .try_with_header("x-forwarded-for", &input.client_ip()?.to_string())?;
        if let Some(tc_string) = consent.raw_tc_string() {
            request = request.with_query("gdpr_consent", tc_string);
        }
        if let Some(gpp) = consent.raw_gpp_string() {
            request = request.with_query("gpp", gpp);
        }
        Ok(Some(request))
    }

    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let source = input.primary_source();
        let call: LiveRampCall = response
            .call_context()?
            .ok_or_else(|| IdentityProviderError::config("missing call context"))?;

        if response.status() == StatusCode::NO_CONTENT {
            return Ok(match call {
                LiveRampCall::Refresh => Resolution::invalidate(source),
                LiveRampCall::Initial => Resolution::no_match(),
            });
        }

        let reply: EnvelopeReply = response.success_json()?;
        let Some(envelope) = reply.envelopes.into_iter().find(|product| {
            product.product_type == ATS_ENVELOPE_TYPE && product.source == ATS_ENVELOPE_SOURCE
        }) else {
            return Ok(Resolution::no_match());
        };

        let lifetime_days = if input.region()?.is("US", "CA") { 15 } else { 30 };
        let id = ResolvedId::new(source, envelope.value)
            .with_atype(3)
            .with_expires_in(Duration::from_secs(lifetime_days * 86_400));
        Ok(Resolution::found([id]))
    }
}
```

What this shows:

- `request` and `response` both need to know whether the call was an
  initial lookup or a refresh, because a 204 means different things. The
  call context carries it (section 12.5).
- A `Refresh` is attempted for an expired envelope too (section 4.4,
  `ResolveKind`). LiveRamp answers 204 for one it no longer accepts, and the
  record is invalidated.
- `Input::HashedEmail` is declared only for `Initial`, so in v1 an owned
  `liveramp.com` source is resolved only by `Refresh`, of an envelope that
  browser ingest seeded (section 7.2). For an `Initial`, eligibility step 4
  leaves the source out because its needs are missing, so LiveRamp is not
  called at all, and `input.hashed_email()?` is never reached.
- A 451 or 4xx goes through `success_json` as `Rejected` and backs off that
  visitor only (section 8).
- The example sets no refresh hint, so the operator's `refresh_after_sec`
  (default one day) paces refreshes. Each refresh costs a provider call and
  a KV write, so a vendor hint belongs here only when LiveRamp documents
  one.

### 12.2 Lockr (passive capture)

Lockr exercises capture from several exact paths, a revocation path that
must work after consent is denied, consent signals carried in the request
body, vendor key names mapped to sources, expiry in milliseconds, and
EID-level provenance. The paths and payloads come from the earlier Lockr
capture design (`spec/lockr-identity-capture`, its section 2), measured on a
live page.

```rust
const LOCKR_API: &str = "/integrations/lockr/api/publisher/app/v2/identityLockr";

/// Lockr's token key names and the sources they carry. Only confirmed
/// mappings are listed.
const KEY_SOURCES: &[(&str, &str)] = &[
    ("id5id", "id5-sync.com"),
    ("_lr_env", "liveramp.com"),
    ("__uid2_advertising_token", "uidapi.com"),
];

impl IdentityProviderConfig for LockrConfig {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        let mut captures = Captures::new()
            .with_request_consent(["/consentString", "/gppString", "/ccpaString"]);
        for endpoint in ["page-view", "generate-tokens", "refresh-tokens", "sync-no-hem-ids", "revoke-consent"] {
            captures = captures.with_path(Method::POST, &format!("{LOCKR_API}/{endpoint}"));
        }

        let mut builder = ProviderDescriptor::builder(LOCKR_INTEGRATION_ID.parse()?)
            .with_captures(captures);
        for (_, domain) in KEY_SOURCES {
            builder = builder.with_source(domain.parse()?);
        }
        builder
            .with_provenance(
                &"id5-sync.com".parse()?,
                EidProvenance::new().with_inserter("Lockr-For-Publishers").with_matcher("").with_mm(2),
            )
            .build()
    }
}

#[derive(Deserialize)]
struct TokenReply {
    #[serde(rename = "aimTokens", default)]
    aim_tokens: Vec<AimToken>,
    #[serde(default)]
    ids: BTreeMap<String, LockrId>,
}

#[derive(Deserialize)]
struct AimToken {
    key_name: String,
    /// Milliseconds since the epoch.
    identity_expires: Option<u64>,
}

#[derive(Deserialize)]
struct LockrId {
    eid: LockrEid,
}

/// Lockr's own EID shape. Not `openrtb::Eid`, which denies unknown fields
/// (`openrtb.rs:72`, `:124`), so one new Lockr field would fail the whole
/// reply.
#[derive(Deserialize)]
struct LockrEid {
    source: String,
    #[serde(default)]
    uids: Vec<LockrUid>,
}

#[derive(Deserialize)]
struct LockrUid {
    id: String,
    atype: Option<i32>,
    ext: Option<serde_json::Value>,
}

impl IdentityProvider for LockrProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn capture(
        &self,
        input: &CaptureInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        if input.path().ends_with("/revoke-consent") {
            return Ok(Resolution::opted_out());
        }

        let Ok(reply) = input.response().json::<TokenReply>() else {
            return Ok(Resolution::no_match());
        };
        Ok(Resolution::found(reply.ids.iter().filter_map(|(key, entry)| {
            let (_, domain) = KEY_SOURCES.iter().find(|(name, _)| name == key)?;
            let source = input.source_named(domain)?;
            if !source.matches(&entry.eid.source) {
                return None;
            }
            let expires = reply
                .aim_tokens
                .iter()
                .find(|token| token.key_name == *key)
                .and_then(|token| token.identity_expires)
                .map(UnixTime::from_millis);
            if expires.is_some_and(|at| at <= input.now()) {
                return None;
            }

            let uid = entry.eid.uids.first()?;
            let mut id = ResolvedId::new(source, uid.id.clone());
            if let Some(atype) = uid.atype {
                id = id.with_atype(atype);
            }
            if let Some(ext) = &uid.ext {
                id = id.with_ext(ext.clone());
            }
            if let Some(at) = expires {
                id = id.with_expires_at(at);
            }
            Some(id)
        })))
    }
}
```

What this shows:

- **Exact paths.** A prefix would also capture `settings` and any future
  endpoint. `Captures::with_path` matches one method and path.
- **Revocation outlives consent.** `revoke-consent` arrives after the visitor
  has denied consent, so the request's consent gate would block it. A
  capture's removals must apply whatever the consent (section 12.5). It is
  provider-scoped here: it removes Lockr-written records only, never the EC.
  Lockr's plan settles the real scope.
- **Consent in the request body.** Lockr's SDK sends its own consent strings
  in the body. The descriptor names them, the coordinator extracts and
  decodes only those fields, and capture is gated by the stricter of the
  cookie consent and the body consent. The provider never sees the body.
- **Provenance.** Lockr's ID5 EID carries `inserter`, `matcher` and `mm`,
  which bidders read. The descriptor declares them once per source, and the
  coordinator adds them to every server-side value Lockr wrote.
- **Milliseconds.** `UnixTime::from_millis` converts Lockr's expiry, and a
  token already expired at capture time is skipped.
- **Optional expiry.** Lockr gives an expiry only for tokens listed in
  `aimTokens`, so it marks no source as expiring. An expiry it does give is
  still enforced: expired IDs are never sent (section 7.1). A provider whose
  sources differ marks only the ones that always expire, with
  `with_expiring_source`.
- **Vendor types.** Lockr's payload is parsed into local, permissive types.
  The repo's `openrtb::Eid` denies unknown fields, so it suits TS's own
  output, not a vendor's input.
- **The opt-out lasts until Lockr issues again.** Lockr has no GVL ID for a
  TC string to re-grant, so its `ps.o` is cleared by a later successful
  token capture under consenting cookie and body consent (section 8).
- **Ownership.** Lockr claims `liveramp.com` and `uidapi.com` too. Where native
  providers own those, the operator narrows Lockr with `sources` (section
  10.2).

### 12.3 UID2 (secrets, crypto, private state)

UID2 exercises resolved secrets, encrypted request envelopes that need
random nonces, a nonce that `response` must check against `request`, a large
private state, and a provider-reported opt-out. The API names follow the
UID2 server-side token endpoints. The envelope crypto is elided.

```rust
#[derive(Debug, Clone, Deserialize, Validate)]
pub struct Uid2Config {
    pub enabled: bool,
    /// For example `https://uid2.example.com`.
    #[validate(url)]
    pub base_url: String,
    /// Secret-store key names at rest, resolved values at runtime. Listed in
    /// `secret_fields()` (section 10.2), so `build_descriptor` never reads them.
    pub api_key: Redacted<String>,
    pub client_secret: Redacted<String>,
}

#[derive(Serialize, Deserialize)]
struct Uid2State {
    refresh_token: String,
    refresh_response_key: String,
    /// Milliseconds since the epoch.
    refresh_expires: u64,
}

#[derive(Serialize, Deserialize)]
struct GenerateCall {
    request_nonce: [u8; 8],
}

#[derive(Deserialize)]
struct Uid2Reply {
    status: String,
    body: Option<Uid2Identity>,
}

#[derive(Deserialize)]
struct Uid2Identity {
    advertising_token: String,
    refresh_token: String,
    refresh_response_key: String,
    /// Milliseconds since the epoch, like the two below.
    identity_expires: u64,
    refresh_from: u64,
    refresh_expires: u64,
}

fn join(base: &Url, path: &str) -> Result<Url, Report<IdentityProviderError>> {
    base.join(path)
        .map_err(|_| IdentityProviderError::config("`base_url` cannot take a path"))
}

// Envelope crypto, elided: AES-256-GCM as UID2's API documentation describes.
// fn encrypt_envelope(secret: &Redacted<String>, iv: &[u8; 12], now: UnixTime,
//     nonce: &[u8; 8], payload: &serde_json::Value) -> Result<String, Report<IdentityProviderError>>;
// fn decrypt_generate(secret: &Redacted<String>, body: &str, nonce: &[u8; 8])
//     -> Result<Uid2Reply, Report<IdentityProviderError>>;
// fn decrypt_refresh(key: &str, body: &str) -> Result<Uid2Reply, Report<IdentityProviderError>>;

impl IdentityProviderConfig for Uid2Config {
    fn build_descriptor(&self) -> Result<ProviderDescriptor, Report<IdentityProviderError>> {
        ProviderDescriptor::builder(UID2_INTEGRATION_ID.parse()?)
            .with_source("uidapi.com".parse()?)
            .with_expiring_ids()
            .with_resolves(
                Resolves::new()
                    .with_endpoint(&self.base_url)
                    .with_needs_for(ResolveKind::Initial, [Input::HashedEmail])
                    .with_private_state(),
            )
            .with_consent_policy(ConsentPolicy::strict().with_tcf_vendor(UID2_GVL_ID))
            .build()
    }
}

impl IdentityProvider for Uid2Provider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn request(
        &self,
        input: &ResolveInput<'_>,
    ) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>> {
        let base = input.endpoint()?;
        if input.is_refresh(input.primary_source()) {
            let Some(state) = input.state::<Uid2State>()? else {
                return Ok(None);
            };
            if UnixTime::from_millis(state.refresh_expires) <= input.now() {
                return Ok(None);
            }
            let url = join(base, "v2/token/refresh")?;
            return Ok(Some(
                ProviderRequest::post_text(&url, &state.refresh_token).with_bearer(&self.api_key),
            ));
        }

        let url = join(base, "v2/token/generate")?;
        let request_nonce = input.random_bytes::<8>();
        let envelope = encrypt_envelope(
            &self.client_secret,
            &input.random_bytes::<12>(),
            input.now(),
            &request_nonce,
            &json!({ "email_hash": input.hashed_email()?.to_base64() }),
        )?;
        Ok(Some(
            ProviderRequest::post_text(&url, &envelope)
                .with_bearer(&self.api_key)
                .try_with_call_context(&GenerateCall { request_nonce })?,
        ))
    }

    fn response(
        &self,
        response: &ProviderResponse,
        input: &ResolveInput<'_>,
    ) -> Result<Resolution, Report<IdentityProviderError>> {
        let body = response.success_text()?;
        let reply: Uid2Reply = match response.call_context::<GenerateCall>()? {
            Some(call) => decrypt_generate(&self.client_secret, body, &call.request_nonce)?,
            None => {
                let state = input
                    .state::<Uid2State>()?
                    .ok_or_else(|| IdentityProviderError::config("refresh without state"))?;
                decrypt_refresh(&state.refresh_response_key, body)?
            }
        };

        match reply.status.as_str() {
            "optout" => Ok(Resolution::opted_out()),
            "success" => {
                let identity = reply.body.ok_or_else(IdentityProviderError::malformed)?;
                let id = ResolvedId::new(input.primary_source(), identity.advertising_token)
                    .with_atype(3)
                    .with_expires_at(UnixTime::from_millis(identity.identity_expires))
                    .with_refresh_at(UnixTime::from_millis(identity.refresh_from));
                Ok(Resolution::found([id]).replacing_state(&Uid2State {
                    refresh_token: identity.refresh_token,
                    refresh_response_key: identity.refresh_response_key,
                    refresh_expires: identity.refresh_expires,
                }))
            }
            _ => Err(IdentityProviderError::malformed()),
        }
    }
}
```

What this shows:

- **Secrets.** `api_key` and `client_secret` are `Redacted<String>` fields
  that hold secret-store key names at rest and resolved values at runtime.
  Their paths must be added to `TrustedServerAppConfig::secret_fields()`
  (`config.rs:136-175`), and `build_descriptor` must not read them, because
  deploy validation runs before resolution (section 10.2).
- **Expired tokens refresh.** A `Refresh` runs for an expired advertising
  token too, so a visitor who misses the refresh window still recovers while
  the refresh token lives. The provider checks `refresh_expires` itself.
- **No dry run.** UID2 keeps private state, so it gets no dry run (section
  5.2), and its `request` always builds the real, encrypted call.
- **Randomness.** The envelope needs a random IV and nonce. A provider has no
  safe source of its own in a pure method, so `ResolveInput::random_bytes`
  supplies them, and the harness makes them deterministic.
- **Call context.** `response` must check the nonce `request` generated. The
  call context carries it, as it carries LiveRamp's call kind.
- **Text bodies.** Both endpoints send and return base64 text, not JSON, so
  `ProviderRequest::post_text` and `ProviderResponse::success_text` are
  needed.
- **State size.** A UID2 refresh token is several hundred bytes, so the state
  cap rises from 512 to 1,024 bytes (sections 4.5, 6.1).
- **Opt-out.** `"optout"` maps to `Resolution::opted_out()`, which is sticky
  (section 8).
- The crypto needs an AES-GCM implementation, a new dependency that the UID2
  spec justifies.

### 12.4 ID5, generic pull, and a provider not in v1

- **ID5** has the shape of the section 4.6 example: `ClientIp` and
  `UserAgent` needed, `PageUrl` wanted, the `signature` as private state, and
  `ext.linkType` per UID. It declares three sources (`id5-sync.com`, the
  EUID source its responses name, and `true-link-id5-sync.com`,
  `id5IdSystem.js:40-41`, `:130-170`) and uses `also_invalidate` for a
  declared source a response no longer returns.
- **Generic pull** is built in (section 10.4).
- **A local provider**, one that needs no outbound call, does not fit v1. An
  example is a publisher login ID read from a first-party cookie and
  emitted as an EID. It would need a third capability, say `derive`, and an
  input that reads a named first-party cookie. Both are additive under
  section 4.2, which is the point of the check. This spec does not add them.

### 12.5 What the examples changed

Writing the examples found these gaps. Each is now in the API of sections 4
to 6:

| Gap                                                          | Found by          | Change                                                                           |
| ------------------------------------------------------------ | ----------------- | -------------------------------------------------------------------------------- |
| `response` cannot know what `request` sent                   | LiveRamp, UID2    | `ProviderRequest::try_with_call_context`, `ProviderResponse::call_context` (4.5) |
| No safe randomness in a pure method                          | UID2              | `ResolveInput::random_bytes`, deterministic in the harness (4.2, 4.4, 13)        |
| Secrets have no stated path                                  | UID2              | Secret fields in `secret_fields()`; `build_descriptor` never reads them (10.2)   |
| Text bodies                                                  | UID2              | `post_text`, `success_text` (4.5)                                                |
| State cap too small for refresh tokens                       | UID2              | 1,024 bytes (4.5, 6.1)                                                           |
| Expiry in milliseconds                                       | Lockr, UID2       | `UnixTime::from_millis` (4.4)                                                    |
| Prefix capture is too broad                                  | Lockr             | `Captures::with_path(method, path)` (4.3)                                        |
| Revocation arrives after consent is denied                   | Lockr             | Capture removals bypass the consent gate (4.7)                                   |
| Consent signals in the request body                          | Lockr             | `Captures::with_request_consent`; the stricter consent wins (4.3, 4.7)           |
| EID-level `inserter`, `matcher` and `mm` cannot be expressed | Lockr             | `ProviderDescriptorBuilder::with_provenance`, applied on output (4.3, 7.1)       |
| A source string from a response must match a declared source | Lockr             | `SourceDomain::matches` normalizes before comparing (4.3)                        |
| An oversized UID-level `ext` discarded the whole ID          | Lockr (ID5 `pba`) | The `ext` is dropped and the ID kept (4.5)                                       |
| Region tests need country and subdivision                    | LiveRamp          | `Region::country`, `Region::is` (4.4)                                            |
| An expired record stranded refreshable providers             | UID2, LiveRamp    | `Refresh` covers expired records; the provider decides (4.4, 4.7)                |
| The dry run repeated expensive work and could not see state  | UID2              | No dry run for providers with private state (5.2)                                |
| Every example parsed its endpoint twice                      | all               | `Resolves::with_endpoint(&str)`, checked by `build()` (4.3)                      |
| Capture had two ways to say "nothing"                        | Lockr             | `capture` returns `Resolution`; `no_match` means nothing (4.2)                   |

## 13. Testing

**Provider harness** (`ec/identity/test_support.rs`). `ProviderHarness`
drives a provider the way the coordinator does, so vendor tests stay short:

```rust
let provider = ExampleIdProvider::new(&example_config()).expect("should build the provider");
let mut harness = ProviderHarness::new(&provider)
    .with_client_ip("192.0.2.10")
    .with_user_agent("ExampleBrowser/1.0");

let request = harness
    .request()
    .expect("should build a request")
    .expect("should not skip the request");
assert_eq!(
    request.url().host_str(),
    Some("api.exampleid.example.com"),
    "should call the declared host"
);

let resolution = harness
    .respond(StatusCode::OK, &json!({ "uid": "example-uid", "signature": "example-signature" }))
    .expect("should parse the response");
assert_eq!(resolution.ids()[0].uid(), "example-uid", "should return the uid");
```

`harness.respond_raw(status, headers, bytes)` covers empty bodies such as a
204, and headers. `harness.request_json::<T>()` decodes the built body.

The harness runs the coordinator's steps around the provider, so a test
sees what production would:

- **Kind.** A source with a `with_stored` record is `Refresh` (valid or
  expired), any other `Initial`.
- **Eligibility.** It applies the consent policy (eligibility step 1) and
  step 4: sources whose kind's needs are missing are left out, and with none
  left `request()` returns `Ok(None)` without calling the provider.
- **Provenance.** `ProviderDescriptor::provenance` gives the EID-level
  fields; the harness adds nothing of its own.

The harness API, in the test-support module, gated by
`#[cfg(any(test, feature = "test-support"))]`. A dependent crate never sees
`cfg(test)`, so a provider in its own crate enables `test-support` as a
dev-dependency feature of `trusted-server-core`:

```rust
impl<'p> ProviderHarness<'p> {
    pub fn new(provider: &'p dyn IdentityProvider) -> Self;
    /// Replaces every input below at once, for conformance cases.
    pub fn with_setup(self, setup: HarnessSetup) -> Self;
    pub fn with_client_ip(self, ip: &str) -> Self;
    pub fn with_user_agent(self, user_agent: &str) -> Self;
    pub fn with_page_url(self, url: &str) -> Self;
    pub fn with_region(self, country: &str, subdivision: Option<&str>) -> Self;
    pub fn with_hashed_email(self, email: HashedEmail) -> Self;
    /// Raw consent strings. Default: GDPR does not apply and no opt-outs.
    pub fn with_consent(self, consent: TestConsent) -> Self;
    pub fn with_stored(self, stored: StoredId) -> Self;
    pub fn with_state(self, state: &impl Serialize) -> Self;
    /// The fixed clock. Default: a constant test time.
    pub fn with_now(self, now: UnixTime) -> Self;
    pub fn with_random_seed(self, seed: u64) -> Self;
    pub fn as_dry_run(self) -> Self;

    /// Calls `request` and keeps the built request and its call context.
    pub fn request(&mut self) -> Result<Option<ProviderRequest>, Report<IdentityProviderError>>;
    /// Decodes the last built request's JSON body.
    ///
    /// # Panics
    ///
    /// When no request was built or its body is not `T`.
    pub fn request_json<T: DeserializeOwned>(&self) -> T;
    /// Calls `response` with the kept call context.
    pub fn respond(
        &mut self,
        status: StatusCode,
        body: &serde_json::Value,
    ) -> Result<Resolution, Report<IdentityProviderError>>;
    pub fn respond_raw(
        &mut self,
        status: StatusCode,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Resolution, Report<IdentityProviderError>>;
    /// Runs a capture as the coordinator does, consent extraction included.
    pub fn capture(&self, fixture: &CaptureFixture) -> Result<Resolution, Report<IdentityProviderError>>;
    /// Resolves an expiry or refresh against the fixed clock.
    pub fn resolve(&self, when: When) -> UnixTime;
    /// The replacement state in a result, decoded, for assertions.
    pub fn state_of<T: DeserializeOwned>(&self, resolution: &Resolution) -> Option<T>;

    /// Runs only the cases passed in, never the harness's own inputs. A
    /// provider declared `with_private_state()` fails here with a pointer to
    /// `check_conformance_with_state`.
    pub fn check_conformance(&self, cases: &ConformanceCases);
    pub fn check_conformance_with_state<S: Serialize + DeserializeOwned>(&self, cases: &ConformanceCases);
}

/// Inputs for one harness run. The harness's own `with_*` methods delegate
/// to an inner `HarnessSetup`.
#[derive(Debug, Clone, Default)]
pub struct HarnessSetup { /* private */ }

impl HarnessSetup {
    pub fn with_client_ip(self, ip: &str) -> Self;
    pub fn with_user_agent(self, user_agent: &str) -> Self;
    pub fn with_page_url(self, url: &str) -> Self;
    pub fn with_region(self, country: &str, subdivision: Option<&str>) -> Self;
    pub fn with_hashed_email(self, email: HashedEmail) -> Self;
    pub fn with_consent(self, consent: TestConsent) -> Self;
    pub fn with_stored(self, stored: StoredId) -> Self;
    pub fn with_state(self, state: &impl Serialize) -> Self;
    pub fn with_now(self, now: UnixTime) -> Self;
    pub fn with_random_seed(self, seed: u64) -> Self;
}

/// Raw TCF, GPP and US Privacy strings, as a request would carry them.
#[derive(Debug, Clone, Default)]
pub struct TestConsent { /* private */ }

impl TestConsent {
    pub fn with_gdpr_applies(self, applies: bool) -> Self;
    pub fn with_tc_string(self, tc: &str) -> Self;
    pub fn with_gpp_string(self, gpp: &str, section_ids: &[u16]) -> Self;
    pub fn with_us_privacy(self, us_privacy: &str) -> Self;
    pub fn with_gpc(self) -> Self;
    pub fn with_dnt(self) -> Self;
}

/// One proxied call: the request a capture rule matches, and its response.
#[derive(Debug, Clone)]
pub struct CaptureFixture { /* private */ }

impl CaptureFixture {
    pub fn new(method: Method, path: &str) -> Self;
    pub fn with_request_body(self, body: &serde_json::Value) -> Self;
    pub fn with_response(self, status: StatusCode, body: &serde_json::Value) -> Self;
}

#[derive(Debug, Clone, Default)]
pub struct ConformanceCases { /* private */ }

impl ConformanceCases {
    /// A setup to run `request` under, and the response to answer it with.
    pub fn with_resolve_case(
        self,
        setup: HarnessSetup,
        status: StatusCode,
        response: &serde_json::Value,
    ) -> Self;
    /// For text, binary or empty replies, such as UID2's base64 body or a
    /// LiveRamp 204.
    pub fn with_resolve_case_raw(
        self,
        setup: HarnessSetup,
        status: StatusCode,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Self;
    pub fn with_capture_case(self, fixture: CaptureFixture) -> Self;
}

/// Test-only constructors, behind the same gate.
impl StoredId {
    pub fn for_test(source: &SourceDomain, uid: &str) -> Self;
    pub fn written_by(self, writer: &IdentityProviderId) -> Self;
    pub fn expiring_at(self, at: UnixTime) -> Self;
}

impl HashedEmail {
    /// From the SHA-256 of a normalized address, as 64 hex digits.
    pub fn for_test(sha256_hex: &str) -> Self;
}

pub fn check_descriptor_ignores_secrets<C: IdentityProviderConfig>(placeholder: &C, resolved: &C);
```

`harness.check_conformance(&cases)` runs the checks every provider must
pass. `ConformanceCases` lists the setups to try (inputs, stored IDs, state
and a test-only `HashedEmail`) and one vendor fixture per capture rule, so the
harness never guesses which setup makes a provider call out:

- The harness builds inputs that hold only what the descriptor declares, so
  reading an undeclared input fails the test with `UndeclaredInput`.
- A provider that declares `resolves` returns `Some` from `request` for at
  least one listed setup, and its `response` on a vendor fixture does not
  return the `not_implemented` marker.
- A provider that declares `captures` returns a `Resolution` other than
  `no_match` for at least one listed fixture, and never the
  `not_implemented` marker.
- No undeclared input or state read is recorded.
- A provider without `with_private_state()` never returns state.
- Requests go only to declared hosts, over HTTPS, with no `Cookie` header and
  with valid header names and values.
- Returned IDs are only for declared sources, at most one per source.
- `ResolvedId::expiry()` and `refresh()` report what the provider set, and a
  fixture ID of a source marked expiring carries an expiry.
- `capture` never returns a state change.
- For every case with state, the harness also injects a stored state that
  is a bare JSON string, which no struct decodes, and checks that the
  provider neither panics nor treats it as valid state. A
  `#[serde(default)]` state type that decodes anything fails here. That the
  coordinator then clears it is a coordinator test.
- During the dry run, `is_dry_run()` is true and the built request is
  discarded unsent. `as_dry_run()` fails the test for a provider declared
  `with_private_state()`, which never gets one.

`harness.check_conformance_with_state::<S>(&cases)` also checks that the
state type round-trips through `replacing_state` and `input.state()`.
`check_descriptor_ignores_secrets::<C>(&placeholder, &resolved)` builds the
descriptor from a config with placeholder secret names and from one with
resolved secrets, and fails if the two differ (section 10.2). Vendor tests add a
fixture response for each outcome the vendor can return, and assert it
through the `Resolution` getters.

The harness also:

- fixes `random_bytes` with `with_random_seed`, so encrypted requests are
  reproducible
- carries the built request's call context into `respond`, as the
  coordinator does
- takes capture fixtures with a request body, and checks that only the
  declared consent pointers are read from it and that a stricter body
  consent blocks positive IDs but not removals
- applies a provider's provenance, so a test can assert the EID-level fields
  bidders will see

The examples in section 12 are the first harness cases: LiveRamp's initial
and refresh 204s, Lockr's page-view and revoke-consent, and UID2's success,
refresh and opt-out.

**Coordinator.**

- An eligibility matrix: consent × ownership × per-source kind × due state ×
  inputs × mode × writer or stager × new or returning EC.
- The merged eligibility view: a cookie-only `no_match` or result stops a
  re-call on the next `/auction`; KV's `o` blocks dispatch even when the
  cookie lacks it.
- `sources` narrowing, owner takeover of a browser-seeded record, sticky
  `ps.o` surviving a jurisdiction change and cleared only by a newer grant,
  and `ps.iv` stopping a re-seed.
- Hold budget: a fake HTTP client with controlled latency shows the auction
  starts at the budget, an answer is saved with `k: 0`, and a cut-off call
  sets `hold_retry_sec` without counting a failure.
- A dry-run `None` sets no pending marker. A `None` or `InputUnavailable`
  waits `refresh_after_sec`.
- Pending markers block re-dispatch, hide that provider's cookie entries
  until a read, are answered by each of the three rules (including a
  removal), and are dropped past their deadline.
- A navigation answered with a status other than 2xx or 304 dispatches no
  after-send provider and keeps hold-mode answers in its cookie.
- An `UndeclaredInput` is logged as `Config` and backs off.
- Only transport failures, timeouts, 429 and 5xx feed the breaker. 4xx and
  `Malformed` from many visitors never open it.
- The burst cap is checked after every other eligibility step.
- Passive capture runs from `handle_proxy` only for providers with a capture
  rule matching the request, decodes gzip and brotli bodies under the caps,
  reads only the declared consent pointers from a request body, and never
  touches `n`, `f` or the breaker.
- A Lockr `revoke-consent` captured on the proxy removes every Lockr-written
  record, owned or unowned, including sources the cookie holds only as
  stubs, and survives a concurrent `/auction` rewriting `ts-eids-s`.
- Providers declared `with_private_state()` never run on stagers. A due
  stateful provider gets a marker without a dry run, phase 4 rebuilds its
  request with the live state, and a writer navigation without an auction
  runs it after send.
- A provider's `ResolveKind` is `Refresh` for a stored record whether it is
  still valid or expired, and an expired record counts as absent for
  `FillMissing`.

**Consent.**

- The GPP sharing and targeted-advertising opt-outs, GPP section 6, and the
  US National GPC and known-child fields decode from each section that
  defines them. Each one blocks a strict policy.
- A raw consent string that does not decode blocks a strict policy.
- A targeted-advertising opt-out without a sale opt-out produces no
  server-side IDs in `user.eids`, no flush of that provider's entries, and no
  IDs written into the cookie.

**One-write invariant.** A new test store counts writes by mode (Add,
`IfGenerationMatch`, Overwrite) and is shared through a test-support feature
so adapter tests can use it. `KvIdentityGraph::counting` counts reads only
(`ec/kv.rs:1596-1605`, `ec/kv_backend.rs:205-221`).

| Scenario                                                           | KV writes                                            |
| ------------------------------------------------------------------ | ---------------------------------------------------- |
| New visitor, navigation                                            | 1 (create, before send)                              |
| New visitor whose first ID collides                                | 1 (create with a re-minted ID)                       |
| Returning visitor, changes pending                                 | 1                                                    |
| Returning visitor, nothing changed                                 | 0                                                    |
| 304 revalidation with changes pending                              | 1                                                    |
| Returning EC, navigation answered with 3xx, 4xx or 5xx             | 0                                                    |
| New visitor, prefetch or prerender navigation                      | 1 (create, before send)                              |
| Returning visitor, prefetch or prerender navigation                | 0                                                    |
| New visitor, client without `Sec-Fetch-Dest`                       | 1 (create, before send)                              |
| Returning visitor, iframe HTML or no `Sec-Fetch-Dest`              | 0                                                    |
| Integration-proxy navigation that mints an EC                      | 1 (create)                                           |
| `/auction`, `/_ts/page-bids`, `/identify`, Lockr API proxy, assets | 0                                                    |
| Consent withdrawal, on any request                                 | CAS tombstone (up to 5 attempts) plus marker, per ID |
| CAS conflict or per-key limit on the flush                         | 1 attempt, not retried                               |

**Withdrawal and removal.**

- A withdrawal on `/auction` after the navigation's send leaves a tombstone,
  and the navigation's phase 4 drops its mutation and calls no provider.
- After `opted_out` or `invalidate`, the removed ID stops reaching bidders
  from the cookie by the next request that reads KV, and within
  `kv_revalidate_sec` at most.
- A KV read that finds a tombstone expires `ts-eids-s`.
- A deletion tombstone makes every browser drop its cookie copy within
  `kv_revalidate_sec`.

**Failed flush.** After a refused write, staged entries and hold-mode answers
are still `k: 0` in the cookie, and the next navigation writes them.

**Reads.** A returning visitor with a valid, recently confirmed `ts-eids-s`,
nothing due, staged or pending, and no registered source in `ts-eids` pays
zero KV reads on the navigation. `/auction` and `/_ts/page-bids` pay their
existing one read and no more.

**No lost EIDs (G10).**

- A registered source present only in the `/auction` body (beyond the
  cookie trim) is saved as a KV gap and written by the next navigation.
- A gap too large for the cookie is saved again by the next `/auction` and
  eventually written.
- Every source today's `/auction` ingest would write reaches KV.

**Orphans and collisions (section 5.5).**

- A row deleted out of band is found within `kv_revalidate_sec` and recovered
  by rotation before send.
- A stager that sees the row missing sets `m`, and the next writer recovers.
- A collision on the first create re-mints before any cookie is set.

**Storage.**

- `KvPartnerId` and `ps` round-trip, with and without the new fields.
- The rollback rule: a stripped or browser-seeded record of an expiring
  owned source is never sent from the server.
- Revision-checked `OwnerRemove` and `OwnerWrite` lose to a newer write.
  Re-applying changes on a later writer keeps the other tab's change.
- `ts-eids-s`:
  - sealing and opening with the EC ID as associated data; a cookie sealed
    for another EC, or tampered with, fails to open and is ignored without
    logging its value
  - rejection of a duplicate `__Host-ts-eids-s`
  - a `cf` that does not match the identity config discards the cookie's
    positive entries and keeps its removals
  - a stale `kc` forcing a read
  - the size cap's drop order, stubs forcing a read, and removals and
    markers never dropped
  - a staged refresh that `OwnerWrite` would apply survives the merge, and
    one it would not is dropped
  - the flush checks (freshness, writer still enabled and owning, consent
    re-gate, opt-out, revision)
  - a replayed old cookie cannot re-insert an opted-out ID, and is overridden
    at its next revalidation
- `ts-eids-r`:
  - an entry written by a removal-producing response survives a concurrent
    `/auction` that rewrites `ts-eids-s` from an older copy
  - two removal-producing responses in flight together both keep their
    batches, each in its own cookie
  - a fifth batch merges into the lowest openable `<id>`; a partly applied
    cookie is kept whole; an over-cap batch coalesces, drops `iv`, then
    splits; a merge that does not fit rewrites all four per provider
  - a Lockr revoke followed by a re-consent capture, before and across
    navigations, ends with Lockr's new IDs in KV and the old opt-out never
    re-applied (`ps.oc` in KV), including after a failed flush
  - a removal names its source by domain, so a config change that reorders
    sources removes the right one
  - a cookie that fails to open is deleted by the writer and never counted
  - a removal without `pv` skips a record its writer re-acquired after `sa`
  - it is honoured on the unusable-consent path, where `ts-eids-s` is
    expired, until the next writer applies it
  - the 512-byte sealed cap per cookie, the one-year `Max-Age`, and that
    stagers never delete one
- The admin lookup redacts `ps.st`.

**Outbound requests.** Requests to Prebid Server, in every
consent-forwarding mode, carry none of the `TS_IDENTITY_COOKIE_NAMES`
cookies, keep every other cookie, and drop the header when nothing is left.
Requests to the publisher origin carry none of `ts-eids`,
`__Host-ts-eids-s` or any `__Host-ts-eids-r-` cookie, keep `ts-ec`,
`ts-ec-pull-complete` and every other cookie, and drop the header when
nothing is left.

**Merge.**

- Owned versus unowned precedence.
- A browser-seeded record never beats the browser's current value.
- Expired IDs are never sent.
- `x-ts-eids` matches what was sent.
- `/_ts/api/v1/identify` omits expired IDs and IDs the owner's policy blocks.

**Batch sync.** An update to an owned source applies only when absent.

**Migration parity.** Today's `ec/pull_sync.rs` tests are ported against
`GenericPullProvider`. Each behaviour change in section 10.4 gets its own
test.

**Config.** Each validation error in section 10.2, `build_descriptor`
running in `ts config validate` without secrets, and acceptance of a config
with no `[ec.identity]` at all.

**Adapters.** Phase 4 runs after send on Fastly. Axum, Cloudflare and Spin
behave as before apart from the Prebid Server and origin cookie strips. The parity test crate
passes
(`cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`).

## 14. Rollout

Three steps, each shippable on its own:

0. **Strips first.** One release before any new cookie is set, ship only
   the cookie strips (origin and Prebid Server, by name and by the
   `__Host-ts-eids-r-` prefix) and the template cache's identity cookie
   list. Stripping `ts-eids` from origin requests is a behaviour change for
   any origin that read it. A rollback after step 1 then lands on code that
   still keeps the new one-year cookies away from the origin and its header
   limit. This release is the rollback floor.
1. **One write, no new vendors.** This step includes:
   - the writer-request rule and the `IdentityMutation` single flush, with
     no retry; the new visitor's create stays before routing, as today
   - `/identify` and `/_ts/page-bids` stopping their writes
   - `ts-eids-s`: the host-only cookie sealed with AES-256-GCM (a new
     `aes-gcm` dependency, which the UID2 provider later reuses), `kc`
     revalidation, KV-gap staging on `/auction`, pending and `m` markers,
     the size cap
   - the `ts-eids-r` removal cookies
   - a metric for the inbound `Cookie` header size (section 6.2)
   - metadata-preserving `KvPartnerId` and `ps`, with `Default` and
     constructors
   - `GenericPullProvider` replacing pull sync, which brings the trait, the
     descriptor, the harness and the write-counting test store
   - the shared servability check for `/identify` and the bidstream

   It changes behaviour on its own: fewer KV writes, `/auction`,
   `/_ts/page-bids` and `/identify` stop writing, and new visitors reach
   pull partners from their second page load. Write and read counters per
   page load show the effect.

2. **Providers.** `.with_identity_provider` and
   `.try_with_identity_provider` for vendors, ownership and its
   validation, hold mode, passive capture, provider state, the rate-limiter
   `record` and `count` methods and the circuit breaker, and the consent
   decoder extensions of section 9.1. Vendor modules follow in their own
   specs.

After providers write metadata, do not roll back past step 1. Step 1's reader
preserves the new fields. A rollback past it strips them:

- `w`, `exp`, `ra` and `at`. The rollback rule of section 6.1 turns these into
  re-fetches, not stale IDs, once rolled forward again.
- `ps.o`. Opted-out providers would be called again after rolling forward,
  except where the cookie still holds the sticky `o`.
- `ps.st`. Providers with state start over.
- `ps.iv`. Rejected values could be re-seeded once.

While rolled back, `exp` is not enforced at all. Rolling back past step 1
also restores today's `/auction` ingest. Step 0's code ignores `ts-eids-s`
and the `ts-eids-r` cookies but still strips them, and the browser keeps
sending what it staged.

## 15. Open questions

1. **How long single-page apps wait.** Some sites are single-page apps:
   after the first page loads, they change pages with JavaScript instead of
   loading a new document from TS. TS hears about those page changes only
   through `/_ts/page-bids`, which never writes KV (section 5.1). So on those
   sites, entries saved in `ts-eids-s` wait until the visitor's next full
   page load. After launch, measure how long saved entries wait before they
   are written, and revisit if the wait is long.
2. **Tombstone failures.** Today's withdrawal tombstone is a CAS write with
   up to five attempts (`ec/kv.rs:1349-1430`). It can fail on CAS exhaustion,
   on Fastly's per-key limit when another write to the same key landed
   within about a second, on a read failure, or on a KV error. Should a
   failed tombstone be retried after send, with a delay?

## 16. Decisions log

Decisions made while writing this spec, with the user:

| Decision                         | Choice                                                                                                                                                                  |
| -------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Scope                            | Framework and trait. Vendors get their own specs.                                                                                                                       |
| HEM input                        | Typed slot, always empty in v1. Intake is a later spec.                                                                                                                 |
| Browser vs server precedence     | Configured per source through `owner`. The owner's valid value wins.                                                                                                    |
| Cookie for server IDs            | Separate `ts-eids-s`, server-owned and HttpOnly, sealed with AES-256-GCM so the origin and its logs never see server-only IDs.                                          |
| Structure                        | Trait plus coordinator, with an optional passive-capture capability.                                                                                                    |
| Trait shape                      | One required method, `descriptor`. Capabilities are default methods. Synchronous. Additive by construction.                                                             |
| Inputs                           | Declared in the descriptor and enforced: a provider can read only what it declared.                                                                                     |
| Page load for the one-write rule | A navigation plus every request it causes. Only the navigation writes. Strict.                                                                                          |
| New-visitor write                | One create before routing, as today, carrying browser EIDs. No provider runs on the first navigation. Changed in rounds 3 and 4.                                        |
| New-visitor recovery             | Today's rotation. The cookie-proof recreation of revision 3 is removed with the deferred create.                                                                        |
| Failed write                     | Not retried. Changed from one merge-retry once Fastly's per-key limit was confirmed.                                                                                    |
| Browser EIDs beyond the cookie   | `/auction` saves the KV gap in `ts-eids-s`. The next navigation writes it.                                                                                              |
| Delay on body-less routes        | Accepted: a browser ID first seen in an `/auction` body reaches KV one page view later.                                                                                 |
| SPA views (`/_ts/page-bids`)     | Stagers. They never write KV.                                                                                                                                           |
| Hold budget                      | `hold_budget_ms` defaults to 100 ms, reduced from 300 ms (section 4.7 gives the reasons).                                                                               |
| Outbound cookies                 | TS's identity cookies are stripped from Prebid Server requests. The origin keeps `ts-ec` but loses `ts-eids`, `ts-eids-s` and `ts-eids-r`, to stay under header limits. |
| Deletion                         | A tombstone with the 24-hour `TOMBSTONE_TTL` replaces the hard delete. No extra writes.                                                                                 |
| GPP opt-outs                     | Sharing and targeted-advertising opt-outs are decoded, and the strict policy blocks them.                                                                               |
| `x-ts-eids`                      | Kept as a full troubleshooting header. Its exposure of server-only IDs to page scripts is accepted.                                                                     |
| Consent withdrawal               | Synchronous tombstone on any request, the only G4 exception. Proposed in review; not yet confirmed.                                                                     |

Round 7 proposed these, and they are applied in revision 8 pending the
user's review of the spec:

| Decision                | Choice                                                                                                      |
| ----------------------- | ----------------------------------------------------------------------------------------------------------- |
| Removals and opt-outs   | One `__Host-ts-eids-r-<id>` cookie per removal batch, up to four, so no concurrent response can lose one    |
| Provider opt-out        | Removes every record the provider wrote, with no revision check. Opted-out writers are never served         |
| Expired records         | `Refresh` covers a stored record, valid or expired. An expired record counts as absent for `FillMissing`    |
| Stateful hold providers | Run only on the writer: before the auction when it runs one, after send when it does not                    |
| `capture` result        | Returns `Resolution`; `no_match` means nothing was captured                                                 |
| Registration            | `try_with_identity_provider` converts a constructor's error once; `build()` stays infallible                |
| KV read cost            | An auction navigation reads once, as today. Phase 4 reuses that snapshot unless an after-send provider runs |
| Provider count          | At most 32 enabled providers, so a row's `ps` never fills up                                                |

## 17. Review log

**Round 1** (2026-10-21) found 5 blocking issues, 16 significant gaps and 17
accuracy problems. Revision 2 resolved them. Its findings covered the
deferred create's rotation risk, staged refreshes, pending-marker liveness,
provider state, the writer definition, hold-mode latency, concurrent cookie
writes, HTML cacheability, multi-source providers, inputs, `ext` level,
owner takeover, `/identify` and batch sync, the size cap, flush checks, rate
limits, the breaker, tombstones, `/auction` ingest and deploy-time
validation.

**Round 2** (2026-10-21) answered the open questions and reworked the trait
for ergonomics and additivity (revision 3).

**Round 3** (2026-10-21) ran four independent reviews: Rust API, write
protocol, code references, and privacy and consent. They found 7 blocking
issues and about 30 significant ones. Revision 4 resolves them as follows:

| Finding                                                                                          | Resolution                                                               |
| ------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------ |
| Deferred create loses withdrawals, collision checks and orphan detection (B1, B2, B3, C1, D1)    | Create before send, sections 5.2, 5.5                                    |
| Cookie IDs outlive KV removals (D2, B5)                                                          | `kc` revalidation, merge rules, G12, 6.2, 6.3                            |
| `ts-eids-s` reaches Prebid Server (D3, C11)                                                      | Strip from Prebid Server; origin unchanged (revision 5), 6.2             |
| Stagers cannot save removals, state or `ps` (B4)                                                 | Removal entries, staged `ps`, state writer-only, 5.3                     |
| Eligibility on stagers ignored the cookie (B6)                                                   | Merged eligibility view, 4.7                                             |
| Invalidate and re-seed loop (B7)                                                                 | `ps.iv`, 7.2                                                             |
| Writer could not clear markers or flip `k` (B8, C16)                                             | Phase 3 cookie rule, 5.2                                                 |
| Late hold results impossible; overruns tripped the breaker (B9, C2)                              | Cut-off calls, `hold_retry_sec`, 4.7, 8                                  |
| `request` returning `None` left providers due forever (A1)                                       | `None` waits `refresh_after_sec`, dry run, 4.7, 8                        |
| Capture backoff on unrecognised bodies (A2)                                                      | Capture never backs off, 4.7, 8                                          |
| `MissingInput` conflated a bug and a skip; state could drop IDs (A3)                             | `UndeclaredInput`, `InputUnavailable`, `with_state(&T)`, 4.4, 4.5        |
| EC ID and capture prefix unenforced (A4)                                                         | Crate-private EC ID, prefix check, 4.3                                   |
| Multi-source invalidation (A5)                                                                   | `also_invalidate`, 4.5                                                   |
| Registration snippet and config trait (A6)                                                       | `change_context`, `IdentityProviderConfig: IntegrationConfig`, 4.6, 10.2 |
| `#[non_exhaustive]` ineffective in-crate (A7)                                                    | Additivity by construction, opt-in kinds, 4.2                            |
| Getters, setters and naming (A8, A13)                                                            | `with_*` setters, getters, renames, 4.3 to 4.5                           |
| Second `ProviderId` (A9)                                                                         | `IdentityProviderId`, 4.3                                                |
| Error type and header validation (A10, A11)                                                      | 4.5                                                                      |
| Lint and doc conventions; silent mis-wiring (A12, A14)                                           | Conventions note, conformance checks, 4.2, 13                            |
| Phase 4 lacked UA, page URL, DNT and a usable IP (C3)                                            | Captured in phase 1, 4.4                                                 |
| `/identify` bypassed the expiry filter (C4, D6)                                                  | Shared servability check, 7.1                                            |
| Missing writes; withdrawal path misdescribed (C5, B10, D13)                                      | P3 table, 5.1, 13, 15                                                    |
| Writer decision placement and prefetch visibility (C6)                                           | 5.1                                                                      |
| Rate-limiter API and breaker (C7, B11, D12)                                                      | `record`/`count`, cap last, transport-only breaker, 4.7                  |
| Capture buffering, encoding and cookie site (C8, C18)                                            | 4.7, 5.3                                                                 |
| Unowned completeness signal (C9)                                                                 | Pull-complete marker kept, 6.3, 10.4                                     |
| Domain-wide cookie and MAC binding (C10, D5)                                                     | `__Host-`, delimited MAC, `cf`, 6.2                                      |
| Clock failure, schema literals, write-counting tests (C12, C13, C14)                             | 4.4, 6.1, 13                                                             |
| Policies gated only dispatch (D4)                                                                | Re-applied at flush, cookie, merge, `/identify`, 9.1                     |
| Opt-out fingerprint lapsed (D8)                                                                  | Sticky `o`, 8                                                            |
| Strict policy failed open (D9)                                                                   | Fail closed, more decoding, 9.1                                          |
| Minimization leaks: geo, jurisdiction, page URL, `x-ts-eids` (D10)                               | `Region`, `ProviderConsent`, sanitized URL, documented header, 4.4, 7.1  |
| Logging holes (D11)                                                                              | 9.3                                                                      |
| `Sec-Fetch-Dest` fallback, undefined transitions, rollback losses (B12, B13, B14, B15, C15, C17) | 3.4, 5.1, 5.2, 6.2, 6.3, 14                                              |
| Reference corrections (C19)                                                                      | Throughout                                                               |

**Round 4** (2026-10-21) re-reviewed revision 4 with two independent
reviewers. They found 6 blocking issues, all with contained fixes, and
confirmed most round-3 fixes. Revision 5 resolves them:

| Finding                                                                                                               | Resolution                                                                 |
| --------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------- |
| A minted ID was used in routing before its deferred create, on publisher and proxy routes                             | Create stays before routing; no provider on the first navigation, 4.7, 5.2 |
| A config change discarded staged removals and opt-outs                                                                | 5.6                                                                        |
| Flush checks could veto removals                                                                                      | Removals exempt, 5.6                                                       |
| Orphan rotation re-bound positive IDs, undoing deletion                                                               | Missing row expires the cookie, 5.5, 5.6                                   |
| Markers could not be answered when phase 4 wrote nothing                                                              | Phase 4 always answers; unowned dry runs need KV, 5.6                      |
| Markers set on non-2xx responses                                                                                      | 5.2                                                                        |
| Unowned hold answers became `OwnerWrite`                                                                              | 5.3                                                                        |
| Writer never flipped its flushed entries to `k: 1`                                                                    | Reconciling read, 5.6                                                      |
| EC minting on non-writer navigations undefined                                                                        | 5.6                                                                        |
| Withdrawal windows and `kc` stamping                                                                                  | 5.6                                                                        |
| G4 named withdrawal the only exception                                                                                | G4 scoped to page loads                                                    |
| `cluster_size` had no trigger; size cap could fail                                                                    | 5.6                                                                        |
| `pv` hash compared with a raw revision                                                                                | `RevisionHash`, 5.4                                                        |
| Capture plumbing, Prebid strip paths, default `capture`, `ProviderConsent`, `type Err`, `with_private_state()` naming | 4.2, 4.3, 5.6, 6.2                                                         |

**Round 5** (2026-10-21), a final review focused on trait ergonomics, found
3 blocking and 6 significant issues, all fixed in revision 6:

- Phase 4 re-reads before dispatching whenever after-send providers are
  marked, so a withdrawal after send is always seen (5.6).
- Conformance checks a provider against at least one working harness setup
  and a fixture, so UID2- and LiveRamp-style providers pass (13).
- The capture-prefix check moves to `IntegrationRegistration::build()` (4.3).
- Undeclared input and state reads are recorded and caught even through
  `.ok()` (4.4).
- Every capability default, including `request`, returns a
  `not_implemented` marker (4.2, 4.5).
- State is enforced: `with_state` and `state()` need `with_private_state()`
  (4.4, 4.5).
- Less boilerplate: `Resolves::with_endpoint`, `primary_endpoint`,
  `ResolveInput::primary_source`; the example parses its endpoint once (4.6).
- Consistent names: `with_private_state`, `with_expiring_ids`,
  `also_invalidate`; `found([])` is `no_match` (4.3, 4.5).
- Stale references, outbound-cookie wording, capture state, the additivity
  claim for consent, mixed-kind inputs and the step 8 wording are corrected.

**Round 6** (2026-10-21) was an example-driven review. LiveRamp ATS, Lockr
and UID2 were written as full providers against the API (section 12), and
everything they could not express was added. Section 12.5 lists the gaps:

- a call context from `request` to `response`
- deterministic randomness
- a stated path for secrets
- text bodies
- a larger state cap
- millisecond times
- exact capture paths
- capture removals that bypass the consent gate
- consent from the request body
- EID-level provenance
- source matching
- keeping an ID whose `ext` is oversized
- region tests

**Round 7** (2026-10-21) ran two final reviews in parallel. One covered the
API, the examples and feasibility against the code. The other covered the
protocol, privacy, consistency and efficiency. Together they found 2
blocking, 13 significant and 11 minor issues, three of them found by both.
All are fixed in revision 8:

- A provider opt-out removes every record that provider wrote, with no
  revision check, and servability excludes records whose writer is opted
  out. `OwnerRemove` is never skipped because of `ps.o` (5.4, 7.1).
- Capture removals and opt-outs go into a new append-only cookie,
  `ts-eids-r`, which a concurrent `/auction` cannot overwrite. It is also
  written on the unusable-consent path (5.3, 6.2).
- A due stateful provider, or one with an unowned or stubbed candidate
  source, triggers the phase 1 KV read, so its dry run sees KV. A dry run
  without loaded state never sets `ps.n` (5.2, 6.3).
- `Refresh` covers expired records, and an expired record counts as absent
  for `FillMissing`. An expiring ID without an expiry keeps the stored `exp`
  when its UID is unchanged, and is discarded otherwise (4.4, 4.5, 5.4).
- `ps` is pruned on every mutation, more than 32 providers is a config
  error, and a mutation that fails validation falls back to its removals
  (5.4, 6.1, 10.2).
- `ts-eids-s` is sealed with AES-256-GCM, compactly encoded and capped at
  2,048 bytes. Rollout adds a `Cookie` header size metric and an origin
  header-limit checklist item (6.2, 14).
- A stateful hold-mode provider runs after send on writer navigations
  without an auction (4.7, 10.1).
- Opt-out clearing: a mirrored `o` yields to KV, GPP clears by section
  values, a capture-only provider clears on a later positive capture, and a
  withdrawal resets every opt-out (8).
- Efficiency: the read-cost table is corrected, phase 4 reuses the phase 1
  snapshot, cluster evaluation runs only for `/identify` and after send, the
  cookie is rewritten only when it changed, and `is_dry_run`, cached
  `state()`, embedded-JSON `st` and one cookie decode per request remove
  repeated work (4.2, 4.4, 5.2, 5.6, 6.1).
- Request-body consent is buffered in `handle_proxy` with
  `collect_body_bounded` and merged by `consent::stricter_of` (4.7, 9.3).
- The capture-path check runs in `IntegrationRegistry::with_plan`, so
  `build()` stays infallible (4.3, 10.3).
- API: `Resolves::with_endpoint(&str)`, an `Access` enum in
  `UndeclaredInput`, a fallible `try_with_call_context` and `call_context`,
  `expiry()` and `refresh()` getters, `StateChange`, `capture` returning
  `Resolution`, `with_retry_in`, `replacing_state`, `CaptureInput`
  accessors, and full listings for `Region`, `HashedEmail`, `UnixTime`,
  `SourceDomain::matches` and `try_with_identity_provider` (4.2 to 4.6).
- Harness: `check_conformance(&ConformanceCases)` and
  `check_descriptor_ignores_secrets` (13).
- Lockr parses into permissive local types and skips expired tokens (12.2).
- Minimization claims and stale wording are corrected (9.2, and
  throughout).

After round 7, at the user's request, `ts-eids`, `ts-eids-s` and `ts-eids-r`
are also stripped from publisher origin requests, which keep `ts-ec`. This
replaces the origin header-limit checklist item (6.2, 14).

**Round 8** (2026-10-21) was a last overall review: two parallel reviews,
one on the protocol and design and one on the API, examples and
efficiency, plus an outside review. They found 1 blocking, 9 significant
and 22 minor issues, several found more than once. All are fixed in
revision 9:

- Removal batches no longer share a cookie. Each removal-producing
  response sets its own `__Host-ts-eids-r-<id>` (up to four), stagers never
  delete one, and only the writer deletes those its phase 1 read shows
  applied. The cookies last a year, so a removal never expires before a
  writer applies it (6.2, 6.3).
- Orphan rotation drops the rest of its mutation and skips phase 4, and
  phase 4 applies the tombstone, missing and failed rules to a reused
  snapshot too (5.2).
- A failed flush drops a stateful provider's staged IDs with its state,
  and the loss of an after-send removal is bounded by the marker deadline
  (5.2).
- Pull sync no longer reads KV on every navigation: the pull-complete
  marker and a cookie-only `ps.n` stop the trigger (6.3).
- `cluster_size` is no longer driven by a `/identify` cookie flag. A writer
  that is already writing adds it after send (5.6).
- A missing row has one outcome, a minimal `m: 1` cookie, and saves no KV
  gap. The KV gap also fills expired sources (5.3, 5.6, 6.3).
- Lockr's opt-out can be cleared: a positive capture stages `ps.oc`, which
  the writer applies before the IDs (5.4, 8).
- Expiry is declared per source (`with_expiring_source`, `is_expiring`).
  Lockr marks none, because its expiry is optional (4.3, 12.2).
- `state()` returns `Result<Option<T>>` with a new `InvalidState` error, and
  `capture` never changes state (4.4, 4.5, 4.7).
- The harness has a full listing, including `HarnessSetup`, `TestConsent`,
  `CaptureFixture`, `ConformanceCases` and test-only constructors (13).
- Stateful providers skip the dry run, saving a phase 1 read per refresh.
  Cookies open lazily, only on identity routes. Browser KV-gap entries are
  dropped last at the size cap (5.2, 6.2).
- Smaller fixes: provenance getters, descriptor derives, the LiveRamp
  refresh pacing, Lockr narrowing, the G10 and G12 bounds, and stale
  wording.

**Round 9** (2026-10-21) ran two parallel reviews, protocol and design, and
API, examples and harness, plus an outside review of developer experience.
They found 2 blocking, 11 significant and 16 minor issues. All are fixed in
revision 10:

- A re-consent clear is stored in KV as `ps.oc`, so an old opt-out batch is
  never re-applied, and "applied" is defined for every removal kind (5.4,
  6.1, 6.2).
- `ts-eids-r` names sources by domain, deletes cookies that fail to open,
  and has a bounded fallback when four cookies are full (6.2).
- A stateful hold provider's answer stays out of the cookie, so its ID
  never flushes without its state (5.2).
- The cookie-only `ps.n` applies to unowned sources and yields to a changed
  KV `n` (6.3). Undispatched markers back off by at least 60 seconds (5.6).
- `cluster_size` is evaluated once per EC from any writer read, as today
  (5.6).
- A strips-only release ships first and is the rollback floor (14).
- UID2's dead dry-run branch is gone; `InvalidState` always clears state and
  has a section 8 row (4.4, 8, 12.3).
- The harness gains a raw resolve case, `HarnessSetup` methods, more
  `TestConsent` setters, `state_of`, and stated kind and eligibility rules.
  It is gated by `cfg(any(test, feature = "test-support"))`, so provider
  crates can use it (13).
- `ProviderRequest::new` and `with_body` cover any method and body; both
  header helpers follow one `Cookie` rule; `found` takes at most one ID per
  source (4.5).
- Derives are corrected (`When` has no `Display`; `Hash` and `Ord` added),
  and stale wording is fixed.
