# Server-to-Server EID Collection — Design Spec

**Date:** 2026-10-30
**Status:** PR A (#1487) implements PR1 to PR4, PR B (#1489) PR5 and PR6;
reviewed (see the review log)
**Source of intent:** the S2S EID collection requirements document, kept
outside this repository (sections referenced as "R§n")

## 1. Purpose

Trusted Server (TS) collects external IDs (EIDs) only through the browser and
one fixed pull-sync protocol, and a single page load can write the EC KV row
several times. This spec defines:

- a Rust trait that any identity provider implements so TS can collect IDs
  server-to-server, with implementations isolated in a dedicated crate;
- one shared coordinator that owns eligibility, consent, transport, time
  budget, validation, merging, persistence and the mirror cookie;
- a strict rule of **at most one KV write per page load**, made on the top-level
  document navigation and never retried; an opt-in after-response phase
  (§5.2.1) may add one guarded follow-up update with provider results only;
- a full-fidelity, ownership- and expiry-aware KV representation for
  provider-written IDs, and how auctions and other consumers read it.

Client-side collection (`ts-eids`, the `/auction` body) keeps working alongside
server-side collection, and both feed the same consent-gated KV row.

## 2. Scope

### 2.1 In scope

- `IdentityProvider` trait, its input/output types and capability description.
- Dedicated provider crate boundary and registration.
- Identity coordinator: provider selection, consent gating, allowlisted
  outbound HTTP, shared time budget, validation, merge, single write.
- Page-load write permit: navigation-only, single-attempt KV writes.
- Additive KV schema for multiple EIDs/UIDs with EID-level and UID-level
  fields, owner and expiry.
- `__Host-ts-eids-mirror` first-party cookie.
- Consumer changes: `/auction`, `/_ts/page-bids`, `x-ts-eids`,
  `/_ts/api/v1/identify`, browser ingest, batch sync on provider-owned sources.
- Re-expressing the existing pull sync as the first provider on the trait.
- Operator configuration, secrets, logs and metrics.

### 2.2 Non-goals (firm, from R§6)

- Logged-in identity and email contribution: no hashed-email intake, no
  `ts-hem` cookie, no `tsjs.setEmail()`, no publisher sign-in changes. The
  trait carries a typed `HashedEmail` slot that is always empty in V1; a source
  that requires it is skipped before any call. TS never receives a raw email
  from the page.
- Proxying vendor identity JavaScript through TS.
- Concrete vendor implementations (each gets its own spec, see §13.2 F1).
- Adapters other than Fastly. KV identity remains Fastly-only; Axum,
  Cloudflare and Spin get this design when EC identity is ported to them.
- Cross-browser linking.
- Vendor segment products and ad-server targeting.
- Batch sync and admin write paths, except how batch sync treats a
  provider-owned source (§8.5).
- Removing `ts-eids` or `x-ts-eids`.
- Storing the browser's complete EID set. Browser ingest keeps today's rules
  (§8.4); full fidelity applies only to provider-written IDs.
- Unregistered sources for providers. A provider claims only sources that have
  an `[[ec.partners]]` entry.

### 2.3 Future tasks (R§5, not in this scope)

Listed in §13.2 in their original order, stacked on `main`, each a Tech Lead
decision.

## 3. Agreed decisions

These were agreed before drafting and are binding on the rest of the spec.

| #   | Topic                     | Decision                                                                                                                                                                                                   |
| --- | ------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | Which request may write   | Only the top-level document navigation. `/auction`, `/_ts/page-bids` and integration-proxy requests never write KV.                                                                                        |
| D2  | Provider timing           | Provider requests start when the navigation starts, run concurrently with the origin fetch, and are collected within a shared total budget of about 200 ms. Their results join the navigation's write.     |
| D3  | Which navigations look up | Any navigation where an enabled provider has a due source (missing or expired UID). No-match results are not cached in V1.                                                                                 |
| D4  | Failed write              | Not retried in that page load. KV and the mirror cookie stay unchanged. A new EC is not issued if its row was not created. The next navigation is a new page load and may write.                           |
| D5  | Availability              | Provider IDs are guaranteed to `/auction` from the next page load. On the same page they are used if the write already completed.                                                                          |
| D6  | Mirror cookie             | JavaScript-readable. Holds provider-owned UIDs of `bidstream_enabled` sources only. Never trusted by the server over KV. Filled deterministically up to its size limit.                                    |
| D6b | Mirror consumer           | The tsjs change that reads the mirror and hands IDs to Prebid is a separate optional PR.                                                                                                                   |
| D7  | Expiry                    | Declared by the writing provider per UID. A UID without `exp` is served until replaced or removed. Browser and batch IDs carry no `exp` and no owner.                                                      |
| D8  | Conflicts                 | On a provider-owned source, the provider's UIDs replace that source's UIDs. Browser ingest fills only missing sources and never overwrites provider-owned UIDs. Batch sync rejects provider-owned sources. |
| D9  | Removal                   | Only consent withdrawal (the existing whole-row tombstone) is in scope. A tombstone wins over concurrent and later collection; IDs return only after consent is granted again.                             |
| D10 | Pull sync                 | Re-expressed as the first provider on the trait; the separate post-send dispatcher is removed.                                                                                                             |
| D11 | Consent                   | Existing EID-persistence consent check, plus an optional per-provider TCF vendor ID that must also have vendor consent.                                                                                    |
| D12 | KV schema                 | Stay on `v: 1` with additive optional fields; no migration.                                                                                                                                                |
| D13 | Other consumers           | Provider IDs appear in `x-ts-eids` and `/_ts/api/v1/identify`, subject to `bidstream_enabled`, expiry and consent.                                                                                         |

§5.2.1 (opt-in `collect = "after_response"`) and §5.6 (refresh state) extend
D2 and D3 additively; defaults are unchanged.

### 3.1 Choices this spec makes to apply those decisions

Reviewers should check these; each follows from a decision above.

- **Navigation test (D1).** A request is a page-load navigation when it is a
  `GET` on the publisher-proxy route with `Sec-Fetch-Dest: document` and
  `Sec-Fetch-Mode: navigate`, and its `Sec-Purpose` header, if any, has no
  `prefetch` token. Requests without Fetch Metadata never write. A navigation
  whose final response is a 301, 302, 303, 307 or 308 redirect does not write;
  the redirect target is the page load. A `304` keeps the permit. This is stricter than the existing `is_navigation_request`
  (which falls back to `Accept: text/html`); that helper keeps its other uses,
  and the write permit uses the stricter test.
- **Integration-proxy EC minting stops (D1).** Integration-proxy requests can no
  longer create a KV row, so they no longer mint a new EC. Today a new EC is
  only issued when its row exists, so this keeps that invariant.
- **Withdrawal is the one exception to the one-write rule (D1, D9).** Any
  finalized request that carries a consent withdrawal and the EC cookie writes
  the tombstone, navigation or not, keeping today's behaviour. It tombstones the
  cookie's EC ID and the active EC ID (when they differ), using a bounded CAS
  retry and, when a store listing proves the key exists, an unconditional
  tombstone. It serves no IDs and expires the mirror cookie, and it expires the
  EC cookie only when every tombstone succeeded, so a failed write is retried on
  the next request instead of orphaning the row. Deferring the tombstone to the
  next navigation would leave a withdrawn visitor's row live for as long as they
  only make non-navigation requests, which privacy does not allow.
- **One owner per source (D8).** Config validation rejects two enabled providers
  claiming the same source, so "the owning provider" of a source is always
  unambiguous.
- **No retry loops on the navigation write (D4).** Existing loops on this path
  (EC-ID collision on create, orphan recovery) become a single attempt.

## 4. Terms

| Term           | Meaning                                                                                                                                                       |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| page load      | One top-level document navigation and every request the page makes because of it: `/auction`, `/_ts/page-bids` (SPA view changes), integration proxies.       |
| navigation     | The request that starts a page load (§3.1).                                                                                                                   |
| row            | The EC KV identity-graph entry keyed by EC ID ([`KvEntry`]).                                                                                                  |
| source         | An EID source domain registered in `[[ec.partners]]`.                                                                                                         |
| provider       | A configured `IdentityProvider` instance with an operator-chosen `ProviderId`.                                                                                |
| owner          | The provider that wrote a UID. Browser and batch UIDs have no owner.                                                                                          |
| provider-owned | A source claimed by an enabled provider. A source whose owner is disabled is not provider-owned; its owned UIDs serve until they expire or are pruned.        |
| due source     | A source an enabled provider claims whose owned UIDs are missing or all expired, or one of whose live owned UIDs has refresh state with `from <= now` (§5.6). |
| live UID       | A stored UID with no `exp`, or with `exp` greater than now.                                                                                                   |
| mirror         | The `__Host-ts-eids-mirror` cookie.                                                                                                                           |

## 5. End-to-end flow

### 5.1 Inputs

| Input                             | Arrives on                                | Persisted?                                  |
| --------------------------------- | ----------------------------------------- | ------------------------------------------- |
| `ts-eids` cookie                  | every request                             | Navigation only, via browser ingest (§8.4)  |
| `sharedId` cookie                 | every request                             | Navigation only, applied last in ingest     |
| `/auction` body `user.eids`       | `/auction`                                | Never; forwarded to bidders on that request |
| Provider lookup responses         | navigation (outbound)                     | Navigation write                            |
| After-response provider responses | navigation, after the response (outbound) | Follow-up CAS update (§5.2.1)               |
| Batch sync                        | its own endpoint                          | Its own write path (unchanged)              |

### 5.2 Navigation (the only writer)

```
browser             TS (navigation)                     origin   providers   KV
  | GET / (document)  |                                    |         |        |
  |------------------>| read EC cookie, consent            |         |        |
  |                   | read row snapshot ---------------------------------->|
  |                   | classify: navigation => PageLoadWrite permit          |
  |                   | coordinator.dispatch(): due sources, consent,         |
  |                   |   allowlist  ---------------------------> lookups    |
  |                   | fetch origin ------------------------>|  (concurrent)  |
  |                   |<-------------------- origin headers --|         |      |
  |                   | coordinator.collect(): wait until all done or         |
  |                   |   dispatch + budget (≈200 ms); drop late responses    |
  |                   | validate + merge: browser ingest + provider results   |
  |                   | if anything changed: one write (permit consumed) ---->|
  |                   |<---------------------------------------- ok / failed  |
  |                   | on ok: Set-Cookie EC (new rows)                       |
|                   | mirror from committed row, else snapshot row          |
  |<------------------| response streams                                      |
```

Notes:

- The budget is measured from dispatch. If the origin takes longer than the
  budget, provider collection adds no latency.
- The added delay never exceeds the budget. Collection uses
  `PlatformHttpClient::select_until` (added by PR #1433), which bounds both
  header waits and body reads by an absolute deadline on every adapter, so a
  slow or byte-trickling provider cannot hold the response.
- Response headers, including cookies, depend on the write result, so the write
  completes before headers are sent. This is today's behaviour for new ECs.
- A navigation with nothing due and nothing new from the browser makes no
  provider calls and no write.

### 5.2.1 After-response collection (opt-in)

A provider can be slower than any acceptable page budget (for example, one
that takes about a second). Setting `collect = "after_response"` on that
provider moves its lookup out of the page path:

```
browser             TS (navigation)                          providers   KV
  |                   | page phase as in §5.2, without after-response providers
  |<------------------| response sent (send_to_client)               |      |
  |                   | dispatch after-response providers --------->|      |
  |                   | collect until after-response budget (≈2 s)   |      |
  |                   | read row + generation --------------------------->|
  |                   | one CAS update with their results (if changed) -->|
```

- Default is `collect = "page"`; with no provider set to `after_response`,
  behaviour is exactly §5.2. No existing behaviour changes.
- Dispatch happens only after the response is sent, because `select_until`
  abandons requests still pending at the page deadline on every adapter.
- The budget is `ec.identity_after_response_budget_ms` (default 2000),
  measured from the after-response dispatch. Late responses are dropped as in
  §8.1.1.
- Only a navigation whose permit is confirmed (not a redirect) and whose page
  phase succeeded (consent allows the EC; the page write landed or had nothing
  to change) runs the phase, with an `AfterResponseWrite` permit (§9.1).
  Redirects and non-navigation requests get neither permit.
- The follow-up write contains provider results only; browser ingest stays in
  the page write. It never creates or revives a row: a missing, tombstoned or
  withdrawn row, or one whose EC ID changed during the request, is skipped.
- When no result can change the row (nothing found, nothing to clear), the
  phase skips the read and the write and logs `unchanged`. A remembered
  opt-out clears only sources the row still stores.
- The write is a compare-and-swap on the generation read just before it. On
  conflict the results are dropped; the source stays due and is retried on the
  next navigation.
- Results are not mirrored in the already-sent response. They are served from
  KV from the next page load. A browser fallback UID for an owned source is
  replaced by the provider's UID, as in §8.3.
- Source ownership is validated across both phases: a source claimed by a
  page provider and an after-response provider is rejected at startup.
- Cost: the instance stays busy after the response for up to the
  after-response budget, which lowers per-instance throughput when sandboxes
  are reused. Keep the budget no larger than the slowest provider needs.
- Only the Fastly adapter runs identity providers (§2.2). Other adapters
  refuse to start with an enabled provider.
- Cookie reads and generated IDs of an after-response provider also land only
  in the follow-up write, so cookie-only providers should stay `page`.

### 5.3 What one navigation write contains

Exactly one of these, chosen from the request snapshot:

| Situation                               | Write                                                                         |
| --------------------------------------- | ----------------------------------------------------------------------------- |
| Consent withdrawn, live row             | No page-load write; the tombstone follows §3.1. No lookups, no ingest.        |
| Row tombstoned, no fresh consent        | None. No lookups. Nothing served.                                             |
| New EC (or orphaned EC being recovered) | Create the row with browser ingest. No lookups: the EC has no row yet (§8.1). |
| Returning EC, something changed         | CAS update with browser ingest + valid provider results; expired UIDs pruned. |
| Returning EC, nothing changed           | None.                                                                         |

### 5.4 Later requests in the same page load

`/auction`, `/_ts/page-bids` and integration proxies read the row snapshot and
never write. They do not receive a permit, so the write API cannot be called
(§9.1). EIDs in the `/auction` body go to bidders on that request only.

### 5.5 Lifecycle per ID origin

| ID origin             | Collected                      | Persisted                     | Mirrored               | Served from KV to `/auction`            |
| --------------------- | ------------------------------ | ----------------------------- | ---------------------- | --------------------------------------- |
| `ts-eids`/`sharedId`  | Navigation carrying cookies    | That navigation's write       | Never                  | Next page load (same page via body)     |
| `/auction` body       | `/auction`                     | Never                         | Never                  | That request only, from the body        |
| Provider lookup       | Navigation where source is due | That navigation's write       | Same response (§10)    | Next page load; same page if write done |
| After-response lookup | Navigation where source is due | Follow-up CAS update (§5.2.1) | Never on that response | Next page load                          |
| Batch sync            | Batch endpoint                 | Its own write path            | Never                  | After its write                         |

### 5.6 Expiry and refresh

- A UID with `exp <= now` is never served, mirrored or forwarded.
- An expired owned UID makes its source due; the owning provider is asked again
  on the next navigation. No other provider and no browser input can refresh it.
- Expired UIDs stay in the row until the next write on any navigation, which
  prunes them. Pruning alone never triggers a write.
- UIDs without `exp` and without refresh state are never refreshed; they stay until replaced or removed.
- A provider may keep private refresh state with a UID it writes (§6.3). Once
  the state's `from` time passes, the source is due even though the UID is
  still live, and the provider builds its refresh from the stored state
  (`LookupInput::current`). For example, a token provider keeps the vendor's
  refresh token and refreshes the token server side without any login input.
  Vendor cookies are read first on every navigation; a usable cookie ID is
  used instead of a refresh request. Refresh state that is empty, over 2048
  bytes or has no stated time is dropped and the UID is kept.
- A UID is refresh-due only while it is still live. A failing refresh is
  therefore retried at most until the UID expires, and the provider's
  `rate_limit_per_hour` also bounds it.
- A refresh reply may report an opt-out (`LookupOutcome::OptedOut`). It is
  handled like a cookie opt-out: the provider's sources are cleared from the
  row.
- A provider whose IDs have no vendor server may generate an ID instead of a
  lookup (`IdentityProvider::generate`), for a due source when its cookies hold
  none. The ID is validated and written like a lookup result; no request is
  sent and no cookie is set. For example, a publisher common ID. Generation is
  not rate-limited, and a source holding a live UID is not regenerated.
- A request may carry provider context (`ProviderRequest::context`) that is
  never sent and is handed back with its response, for example a key that
  decrypts the reply.

### 5.7 Consent withdrawal

- On any finalized request with the EC cookie: the tombstone is written outside
  the page-load permit (§3.1); provider calls are not made; any lookup already
  dispatched is discarded; no IDs are served and the mirror is expired.
- A concurrent navigation in another tab that tries to write collection results
  after the tombstone fails its CAS and is not retried, so the tombstone wins. A
  tombstone that loses its CAS to such a write retries, so the withdrawal still
  wins.
- After fresh consent, the existing revive path creates a new row; previous IDs
  are not restored.

## 6. Type design

### 6.1 Principles

- **Every seam is a trait.** Five traits make the solution pluggable; each has
  one production implementation and can be replaced in tests or future work
  without touching shared policy.

  | Trait                 | Varies by                   | Production implementation | Other implementations             |
  | --------------------- | --------------------------- | ------------------------- | --------------------------------- |
  | `IdentityProvider`    | vendor                      | one per provider kind     | test providers                    |
  | `ProviderKind`        | vendor (typed construction) | one per provider kind     | —                                 |
  | `LookupTransport`     | runtime / environment       | `PlatformLookupTransport` | test transport; F4 mock transport |
  | `IdentityCoordinator` | orchestration policy        | `ProviderCoordinator<P>`  | test fake for navigation tests    |
  | `EcKvStore` (exists)  | KV backend                  | Fastly KV                 | existing in-memory test store     |

- **Static dispatch on the request path.** The coordinator, transport and
  navigation code are generic. The new traits use native `async fn` (with
  `#[allow(async_fn_in_trait)]`), not `async_trait`, so they add no boxed
  futures; the existing `PlatformHttpClient` underneath keeps its own
  `async_trait` boxing. The only trait objects are the configured providers: operators choose a
  heterogeneous set, so each is boxed once at startup. `ProviderCoordinator<P>`
  stays generic over `P`, so tests and single-provider builds use a concrete
  type with no boxing.
- **Borrow per-request data.** Provider input borrows the request and the row
  snapshot. Providers parse responses into types that borrow the response body
  (`Cow<'body, str>`, `&'body RawValue`). Only entries that pass validation and
  will be stored become owned.
- **Share config-derived identifiers.** `ProviderId` and `SourceDomain` wrap
  `Arc<str>`; clones made from config cost a reference count. `Serialize` and
  `Deserialize` are implemented by hand: serializing writes the string
  directly, and deserializing visits a `&str` and allocates the `Arc<str>`
  once.
- **Keep opaque JSON opaque in storage.** `ext` objects are stored as `RawJson`,
  a `RawValue` newtype: never parsed into a map between provider response and
  KV. They are parsed once, into `serde_json::Value`, only when an OpenRTB
  request or header is built, because `crate::openrtb` already uses `Value` for
  `ext`. This needs `serde_json`'s `raw_value` feature; no new crate is added.
- **Parse, don't validate.** Raw provider output converts into stored types
  through `TryFrom`; past that boundary an invalid UID, agent type or match
  method cannot be represented.
- **No micro-optimisation.** Small `Vec`s sized by provider or UID count are
  fine; no arenas, no small-vector crates.

### 6.2 Value types

```rust
/// Unix time in whole seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, derive_more::Display)]
#[serde(transparent)]
pub struct UnixSeconds(u64);

/// Operator-chosen identifier of a configured provider instance.
///
/// `Serialize` and `Deserialize` are implemented by hand (§6.1).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, derive_more::Display)]
pub struct ProviderId(Arc<str>);

/// Normalized EID source domain.
///
/// `Serialize` and `Deserialize` are implemented by hand (§6.1);
/// deserializing normalizes and rejects invalid domains.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, derive_more::Display)]
pub struct SourceDomain(Arc<str>);

/// A non-empty UID of at most [`MAX_STORED_UID_LENGTH`] (1024) bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, derive_more::Display)]
#[serde(try_from = "String")]
pub struct UidValue(Box<str>);

impl TryFrom<String> for UidValue {
    type Error = EidValidationError;
    // ...
}

/// Non-negative OpenRTB agent type, including vendor-specific values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "i32")]
pub struct AgentType(i32);

/// An opaque JSON object, compared by its bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RawJson(Box<RawValue>);

impl PartialEq for RawJson {
    fn eq(&self, other: &Self) -> bool {
        self.0.get() == other.0.get()
    }
}

/// Why a provider value was rejected; counted per reason, never logged with
/// the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub enum EidValidationError {
    #[display("UID is empty")]
    EmptyUid,
    #[display("UID is too long")]
    UidTooLong,
    #[display("agent type is negative")]
    NegativeAgentType,
    #[display("match method is unknown")]
    UnknownMatchMethod,
    #[display("ext is not a JSON object within the size limit")]
    InvalidExt,
    #[display("source is not claimed by this provider")]
    UnclaimedSource,
    #[display("expiry is in the past")]
    Expired,
}

impl core::error::Error for EidValidationError {}

/// OpenRTB 2.6 EID match method (`mm`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "i32", try_from = "i32")]
pub enum MatchMethod {
    Unknown,
    NoMatching,
    CookieSync,
    Authenticated,
    Observed,
    Inference,
}

/// When a stored UID stops being served.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Expiry {
    /// Served until replaced or removed.
    #[default]
    Never,
    /// Never served at or after this time.
    At(UnixSeconds),
}

impl Expiry {
    /// Returns whether a UID with this expiry may be served at `now`.
    #[must_use]
    pub fn is_live(self, now: UnixSeconds) -> bool {
        match self {
            Self::Never => true,
            Self::At(exp) => now < exp,
        }
    }
    /// Returns whether this is [`Expiry::Never`]; used to omit `exp`.
    #[must_use]
    pub fn is_never(&self) -> bool {
        matches!(self, Self::Never)
    }
}
```

`Expiry` is stored as an optional `exp` field: absent for `Never`.

```rust
/// Serde adapter storing [`Expiry`] as an optional `exp` number.
mod optional_exp {
    pub fn serialize<S: Serializer>(expiry: &Expiry, serializer: S) -> Result<S::Ok, S::Error> {
        match expiry {
            Expiry::At(exp) => exp.serialize(serializer),
            // Never reached: the field is skipped when `Expiry::is_never`.
            Expiry::Never => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Expiry, D::Error> {
        Ok(Option::<UnixSeconds>::deserialize(deserializer)?.map_or(Expiry::Never, Expiry::At))
    }
}
```

Sketches in this spec omit doc comments on self-describing fields and elide
bodies with `// ...`; implementations follow the documentation rules in
`AGENTS.md`.

### 6.3 Stored row

All additions are optional `serde` fields on schema `v: 1`. Rows written before
this change read unchanged. Rows written after it stay readable by older code
through the retained `uid` field, unless that `uid` (the first live provider
UID) is over 512 bytes: older code rejects such a row whole.

The row also gains `opt_outs`, a map from provider ID to the Unix time until
which that provider's opt-out is remembered (30 days, §8.3).

`uid` is rewritten on every write to the first live UID of `eids`, and new code
ignores it when `eids` is non-empty. A known limitation of staying on `v: 1`
(D12): during a rollout or rollback, older code reads only `uid`, so it can
serve that UID after its `exp`, and an older writer that re-saves the row drops
`eids` and `opt_outs`. A UID served past its `exp` and dropped `eids` end at the
next write by new code; a dropped opt-out is lost, so that provider is called
again. Stored values are built only by validation
(§8.2) or by deserializing rows TS wrote, so their fields are private to the
`ec` module and exposed through accessors.

```rust
/// A partner ID record within a KV entry, keyed by source domain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvPartnerId {
    /// First live UID of `eids`, or the single UID for records written by
    /// browser ingest, batch sync or older versions.
    uid: String,
    /// Full-fidelity entries written by a provider.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    eids: Vec<KvEid>,
}

/// One stored EID entry with its EID-level fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvEid {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inserter: Option<Box<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    matcher: Option<Box<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mm: Option<MatchMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ext: Option<RawJson>,
    uids: Vec<KvUid>,
}

/// One stored UID with its owner and expiry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KvUid {
    id: UidValue,
    /// Provider value, or the source's configured `openrtb_atype`.
    atype: AgentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ext: Option<RawJson>,
    /// Provider that wrote this UID; `None` for browser and batch UIDs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<ProviderId>,
    #[serde(default, rename = "exp", with = "optional_exp", skip_serializing_if = "Expiry::is_never")]
    expiry: Expiry,
    /// Owning provider's private refresh state (§5.6); never served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh: Option<KvRefresh>,
}

/// Refresh state a provider keeps with one of its UIDs. `Debug` hides
/// `state`, which may hold a credential.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct KvRefresh {
    /// Provider-defined state, opaque to the framework (at most 2048 bytes).
    state: String,
    /// When the owning provider should be asked to refresh the UID.
    from: UnixSeconds,
}
```

Outbound EIDs (auction, mirror, `x-ts-eids`) are built from a UID's `id`,
`atype` and `ext` only, so `refresh` never leaves the row. Refresh state is
private: it is never served, mirrored, forwarded or shown. The authenticated
admin view (`/_ts/admin/ec/{id}`) shows each `refresh.state` as `[redacted]`
and keeps `from` visible. It is removed with the UID on opt-out, withdrawal or
expiry pruning. Rows without it read unchanged.

A UID whose refresh state is empty or over 2048 bytes, or has no stated time,
keeps the UID and drops only the refresh state.

The source domain stays the map key in `KvEntry::ids`, so one provider can own
several sources and each UID still records its owner.

### 6.4 Reading the row

Consumers read through borrowed views and build owned OpenRTB values once, at
the point they serialize a request or header:

```rust
impl KvEntry {
    /// Iterates entries for `bidstream_enabled` sources, yielding only live
    /// UIDs and borrowing from the row.
    pub fn live_eids<'row>(
        &'row self,
        registry: &'row PartnerRegistry,
        now: UnixSeconds,
    ) -> impl Iterator<Item = EidView<'row>> + 'row;
}

/// A borrowed EID entry: either a single legacy UID or a full-fidelity entry.
pub enum EidView<'row> {
    Single { source: &'row str, uid: &'row str, atype: AgentType },
    Full { source: &'row str, eid: &'row KvEid, now: UnixSeconds },
}

impl From<EidView<'_>> for crate::openrtb::Eid {
    // ...
}
```

The in-process `crate::openrtb::Eid` gains optional `inserter`, `matcher`,
`mm` and `ext` fields so full-fidelity entries reach bidders unchanged.

## 7. Provider interface and crate boundary

### 7.1 Where things live

| Location                                   | Contents                                                                                                                                                 |
| ------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `trusted-server-core` `ec::eid_types`      | Value types and stored row types (§6).                                                                                                                   |
| `trusted-server-core` `ec::provider`       | `IdentityProvider`, `ProviderKind`, the generic `build_kind` helper, input and output types, errors. No vendor code.                                     |
| `trusted-server-core` `ec::coordinator`    | `IdentityCoordinator`, `ProviderCoordinator<P>`, `LookupTransport`, `PlatformLookupTransport`, validation, merge, mirror encoding.                       |
| `crates/trusted-server-identity-providers` | One module per provider kind, each implementing `ProviderKind` and `IdentityProvider`, and one `build` function that matches `kind` to a `ProviderKind`. |
| `trusted-server-adapter-fastly`            | Builds providers through the provider crate and a `ProviderCoordinator` at startup, and drives the navigation path with `PlatformLookupTransport`.       |

Dependencies point one way: the provider crate depends on core; core never
depends on the provider crate. Adding a provider means adding a module and one
`build` match arm in the provider crate. Shared consent, KV, cookie and auction
code never gains provider-specific branches.

### 7.2 `IdentityProvider`

Providers are pure: they build a request and parse a response. They never do
HTTP, KV, cookie or clock work. The trait is dyn-compatible so a configured set
can hold `Box<dyn IdentityProvider>`.

```rust
/// Builds lookup requests and parses lookup responses for one identity provider.
///
/// Implementations must not perform I/O or read the clock; the coordinator
/// executes requests and applies consent, transport, validation, expiry and
/// persistence policy.
pub trait IdentityProvider {
    /// Describes which sources and EID shapes this provider can return.
    fn capabilities(&self) -> &ProviderCapabilities;

    /// Builds the lookup request for the given due sources.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Request`] when the input cannot form a request.
    fn build_request(&self, lookup: &LookupInput<'_>) -> Result<ProviderRequest, Report<ProviderError>>;

    /// Parses a provider response, borrowing from its body.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::InvalidResponse`] when the body cannot be parsed
    /// at all. Individually malformed entries are returned and dropped by the
    /// coordinator's validation, not reported as an error.
    fn parse_response<'body>(
        &self,
        response: ProviderResponse<'body>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>>;

    /// Generates IDs for due sources instead of a lookup (§5.6).
    ///
    /// The default returns `None`: the provider makes a lookup.
    fn generate(&self, lookup: &LookupInput<'_>) -> Option<Vec<ProviderEid<'static>>> {
        None
    }
}

impl<P: IdentityProvider + ?Sized> IdentityProvider for Box<P> {
    // Delegates each method to `**self`.
}
```

Capabilities:

```rust
/// What a provider can return, used by the coordinator to apply shared policy.
#[derive(Debug, Clone)]
pub struct ProviderCapabilities {
    /// Sources this implementation can return and the input each needs.
    pub sources: Box<[SourceCapability]>,
    /// Limits and fields accepted from one response.
    pub shape: EidShape,
}

#[derive(Debug, Clone)]
pub struct SourceCapability {
    pub source: SourceDomain,
    pub input: SourceInput,
}

/// Visitor input a source needs before it can be looked up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceInput {
    /// Works for any visitor.
    Anonymous,
    /// Needs a hashed email. Always skipped in V1.
    HashedEmail,
}

#[derive(Debug, Clone, Copy)]
pub struct EidShape {
    pub max_eids: NonZeroUsize,
    pub max_uids_per_eid: NonZeroUsize,
    pub eid_level_fields: EidLevelFields,
}

/// Whether `inserter`, `matcher`, `mm` and EID `ext` are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EidLevelFields {
    Preserved,
    Ignored,
}
```

Input and request:

```rust
/// Request-scoped data the coordinator lends to a provider.
#[derive(Debug, Clone, Copy)]
pub struct LookupInput<'a> {
    pub ec_id: &'a str,
    pub consent: &'a ConsentContext,
    pub client: ClientSignals<'a>,
    pub publisher_domain: &'a str,
    /// Sources due for this provider, already filtered by consent and input.
    pub due_sources: &'a [SourceDomain],
    /// This provider's currently stored entries, borrowed from the row.
    pub current: OwnedEntries<'a>,
    /// Always `None` in V1.
    pub hashed_email: Option<&'a HashedEmail>,
}

#[derive(Debug, Clone, Copy)]
pub struct ClientSignals<'a> {
    pub ip: Option<IpAddr>,
    pub user_agent: Option<&'a str>,
}

/// An outbound request description. The coordinator, not the provider, sends it.
///
/// `Debug` is implemented by hand and prints only the method and host, because
/// headers and query strings may carry secrets. Providers mark secret headers
/// with `HeaderValue::set_sensitive(true)`.
pub struct ProviderRequest {
    pub method: http::Method,
    pub url: url::Url,
    pub headers: http::HeaderMap,
    pub body: bytes::Bytes,
    /// Never sent; handed back with the response (§5.6).
    pub context: Option<String>,
}
```

`OwnedEntries<'a>` is a borrowed view over the row that yields this provider's
`(&str, &KvEid)` pairs, keyed by the row's source strings. Provider secrets and settings are not in the
input: they are given to the provider once, when it is built (§7.3).

PR3 ships `LookupInput` without the request's cookies. F1 (#1488) adds
`LookupInput::cookies` and `IdentityProvider::read_request_cookies`, which
returns a `CookieRead`; the cookie-first refresh (§5.6), cookie opt-outs (§8.3)
and scenarios 24 and 26 depend on it.

Response and parsed output:

```rust
#[derive(Debug, Clone, Copy)]
pub struct ProviderResponse<'body> {
    pub status: http::StatusCode,
    pub headers: &'body http::HeaderMap,
    pub body: &'body [u8],
    /// The request's `context`, handed back.
    pub context: Option<&'body str>,
}

#[derive(Debug)]
pub enum LookupOutcome<'body> {
    Found(Vec<ProviderEid<'body>>),
    NoMatch,
    /// The visitor opted out (§8.3).
    OptedOut,
}

/// One entry as returned by a provider, before validation.
#[derive(Debug)]
pub struct ProviderEid<'body> {
    pub source: Cow<'body, str>,
    pub inserter: Option<Cow<'body, str>>,
    pub matcher: Option<Cow<'body, str>>,
    pub mm: Option<i32>,
    pub ext: Option<&'body RawValue>,
    pub uids: Vec<ProviderUid<'body>>,
}

#[derive(Debug)]
pub struct ProviderUid<'body> {
    pub id: Cow<'body, str>,
    pub atype: Option<i32>,
    pub ext: Option<&'body RawValue>,
    pub expiry: ProviderExpiry,
    /// Private refresh state to keep with the UID (§5.6).
    pub refresh: Option<ProviderRefresh<'body>>,
}

#[derive(Debug)]
pub struct ProviderRefresh<'body> {
    pub state: Cow<'body, str>,
    /// When to refresh; resolved like `exp`.
    pub from: ProviderExpiry,
}

/// Expiry as a provider states it. The coordinator turns it into [`Expiry`]
/// using its own clock, so providers never read time.
#[derive(Debug, Clone, Copy)]
pub enum ProviderExpiry {
    Unspecified,
    At(UnixSeconds),
    After(Duration),
}

#[derive(Debug, derive_more::Display)]
pub enum ProviderError {
    #[display("provider request could not be built")]
    Request,
    #[display("provider response could not be parsed")]
    InvalidResponse,
}

impl core::error::Error for ProviderError {}
```

Providers typically deserialize their wire format into private structs with
`#[serde(borrow)]` and map them into `ProviderEid<'body>`.

### 7.3 `ProviderKind` and registration

Construction is typed per provider; only the final value is erased.

```rust
/// Typed constructor for one provider implementation.
pub trait ProviderKind {
    /// Value of `kind` in `[[ec.identity_providers]]`.
    const KIND: &'static str;
    /// The provider-specific `settings` table.
    type Settings: DeserializeOwned;
    /// The constructed provider.
    type Provider: IdentityProvider + 'static;

    /// Builds the provider from shared and provider-specific configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderConfigError::Invalid`] when the settings are unusable.
    fn build(
        common: &ProviderCommon<'_>,
        settings: Self::Settings,
    ) -> Result<Self::Provider, Report<ProviderConfigError>>;
}

/// Shared configuration every provider receives at build time.
pub struct ProviderCommon<'a> {
    pub endpoint: &'a url::Url,
    pub sources: &'a [SourceDomain],
    pub secrets: &'a ProviderSecrets,
}

/// Parses `settings` for `K` and builds a boxed provider. Lives in core.
///
/// `secrets` are resolved by the adapter at startup through the existing
/// secret path, from the key names in `config`.
///
/// # Errors
///
/// Returns [`ProviderConfigError::Invalid`] when `settings` does not
/// deserialize into `K::Settings` or [`ProviderKind::build`] rejects it.
pub fn build_kind<K: ProviderKind>(
    config: &IdentityProviderConfig,
    secrets: &ProviderSecrets,
) -> Result<Box<dyn IdentityProvider>, Report<ProviderConfigError>>;
```

The provider crate's single entry point, shown with the `pull-sync` arm that
PR5 adds (PR3 ships only the fallback arm):

```rust
/// Builds the provider for a configured `kind`.
///
/// # Errors
///
/// Returns [`ProviderConfigError::UnknownKind`] for an unregistered kind, or
/// any error from [`build_kind`].
pub fn build(
    config: &IdentityProviderConfig,
    secrets: &ProviderSecrets,
) -> Result<Box<dyn IdentityProvider>, Report<ProviderConfigError>> {
    match config.kind() {
        PullSync::KIND => build_kind::<PullSync>(config, secrets),
        kind => Err(Report::new(ProviderConfigError::UnknownKind)
            .attach(format!("kind `{kind}` is not registered"))),
    }
}
```

`ProviderSecrets` holds `Redacted<String>` values resolved through the existing
secret path and is passed only at build time.

```rust
#[derive(Debug, derive_more::Display)]
pub enum ProviderConfigError {
    #[display("provider kind is not registered")]
    UnknownKind,
    #[display("provider configuration is invalid")]
    Invalid,
}

impl core::error::Error for ProviderConfigError {}
```

`IdentityProviderConfig` is the deserialized `[[ec.identity_providers]]` entry
(§12.1). `HashedEmail` is a newtype over a validated lowercase hex SHA-256
digest with no constructor reachable from request data in V1.

## 8. Coordinator

### 8.1 Responsibilities

The coordinator owns, for every provider:

1. **Eligibility.** Provider enabled; the EC's row exists and is not
   tombstoned (an EC minted on this navigation, or a cookie EC with no row, is
   not sent to vendors); at least one due source; sources needing `HashedEmail` removed.
2. **Consent.** The existing EID-persistence check passes, and the provider's
   `tcf_vendor_id`, when configured, has vendor consent in the effective TCF
   string. A failure skips that provider only, before any request is built.
3. **Destination.** The built URL must be `https` and its host must be in the
   provider's configured `allowed_domains`. The URL comes from operator config;
   browser data never selects a destination. A rejected destination skips the
   provider and is counted.
4. **Transport.** All eligible requests are started through a
   `LookupTransport` and gathered until all complete or the shared deadline
   passes. Late responses are dropped (§8.1.1).
5. **Validation** (§8.2), **merge** (§8.3) and preparing the single write.
6. **Mirror** encoding (§10) from the row as it stands after the navigation.
7. **Logs and metrics** (§12).

#### 8.1.1 `LookupTransport`

The transport sends provider requests and gathers responses within a deadline.
It is the only coordinator dependency that touches the network, so swapping it
is how tests and the future debug mode (F4) run real providers and the real
coordinator against mock responses.

```rust
/// Sends provider lookups and gathers their responses before a deadline.
#[allow(async_fn_in_trait)]
pub trait LookupTransport {
    /// An in-flight request.
    type Pending;

    /// Starts one request without waiting for its response.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Send`] when the request cannot be started.
    async fn start(
        &self,
        request: ProviderRequest,
        deadline: Deadline,
    ) -> Result<Self::Pending, Report<TransportError>>;

    /// Waits for in-flight requests until all finish or `deadline` passes.
    ///
    /// Requests still in flight at the deadline are abandoned and reported as
    /// [`TransportError::Timeout`].
    async fn gather(
        &self,
        pending: Vec<Tagged<Self::Pending>>,
        deadline: Deadline,
    ) -> Vec<Tagged<Result<TransportResponse, Report<TransportError>>>>;
}

/// A value labelled with the provider it belongs to.
#[derive(Debug)]
pub struct Tagged<T> {
    pub provider: ProviderIndex,
    pub value: T,
}

/// An owned response; providers parse it by borrowing its body.
#[derive(Debug)]
pub struct TransportResponse {
    pub status: http::StatusCode,
    pub headers: http::HeaderMap,
    pub body: bytes::Bytes,
}

/// End of the shared lookup budget.
#[derive(Debug, Clone, Copy)]
pub struct Deadline(web_time::Instant);

/// Position of a provider in the coordinator's configured list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderIndex(u16);

#[derive(Debug, derive_more::Display)]
pub enum TransportError {
    #[display("lookup request could not be sent")]
    Send,
    #[display("lookup did not complete before the deadline")]
    Timeout,
    #[display("lookup response exceeded the body limit")]
    BodyTooLarge,
}

impl core::error::Error for TransportError {}
```

`Deadline` wraps the `web_time::Instant` that `select_until` takes, because
core is built into the Cloudflare Worker (CI wasm clock lint).
`PlatformLookupTransport<'s>` borrows `RuntimeServices` and is a thin adapter
over the platform client: `start` calls `send_async`, and `gather` calls
`select_until` with the shared deadline while requests remain in flight,
stopping when none remain (it rejects an empty list) or it returns `Ok(None)`,
and tags each result with its provider. It adds no timing logic of its own. Response
bodies are capped at 64 KiB.

#### 8.1.2 `IdentityCoordinator`

The navigation path depends on the `IdentityCoordinator` trait, generic over
the coordinator and the transport. The trait covers the coordinator's whole
surface on a navigation: start lookups and collect them (PR B adds the mirror).
It does not plan the row change: the free function `write_plan::plan_write`
merges the results and plans it. Its production implementation is
`ProviderCoordinator<P>`. Tests of coordinator rules (validation, merge,
mirror) run `ProviderCoordinator` with test providers and a test transport; a
fake `IdentityCoordinator` is used only for navigation wiring tests, where
those rules are not under test.

```rust
/// Coordinates server-side identity lookups for one navigation.
///
/// Implementations own eligibility, consent, destination checks, the shared
/// time budget, validation, merging and mirror encoding. Providers never see
/// KV, cookies, HTTP or the clock. Implementations never write KV: the caller
/// plans the change with `plan_write` and spends the [`PageLoadWrite`] permit.
#[allow(async_fn_in_trait)]
pub trait IdentityCoordinator {
    /// Starts lookups for every eligible provider without waiting.
    ///
    /// Returns empty pending lookups when nothing is due, consent does not
    /// permit EID collection, or no provider is enabled.
    async fn dispatch<T: LookupTransport>(
        &self,
        transport: &T,
        lookup: &LookupContext<'_>,
    ) -> PendingLookups<T::Pending>;

    /// Waits for dispatched lookups until all finish or the deadline passes,
    /// then parses and validates each response.
    ///
    /// Never fails: per-provider failures are recorded in [`LookupResults`].
    async fn collect<T: LookupTransport>(
        &self,
        transport: &T,
        pending: PendingLookups<T::Pending>,
    ) -> LookupResults;

    /// Builds the mirror cookie from the row as it stands after this
    /// navigation: the committed row, or the snapshot when no write landed.
    fn mirror(&self, row: Option<&KvEntry>, consent: &ConsentContext, now: UnixSeconds) -> MirrorCookie;
}
```

Supporting types:

```rust
/// Request-scoped inputs to [`IdentityCoordinator::dispatch`].
pub struct LookupContext<'a> {
    pub ec_id: &'a str,
    pub snapshot: &'a EcKvSnapshot,
    pub consent: &'a ConsentContext,
    pub client: ClientSignals<'a>,
    pub publisher_domain: &'a str,
    /// Existing EC rate limiter, used only for `rate_limit_per_hour`.
    pub rate_limiter: &'a dyn RateLimiter,
    pub now: UnixSeconds,
}

/// Lookups in flight for one navigation. Only the coordinator module can
/// create a non-empty value. `Default` (empty) and `Debug` (counts only) are
/// implemented by hand so they do not require bounds on `Pending`.
pub struct PendingLookups<Pending> {
    requests: Vec<Tagged<Pending>>,
    deadline: Option<Deadline>,
    /// Request time, for validating `exp` and resolving `ProviderExpiry::After`.
    now: UnixSeconds,
    skipped: Vec<Tagged<SkipReason>>,
}

/// Validated results of one navigation's lookups.
#[derive(Debug, Default)]
pub struct LookupResults {
    lookups: Vec<ProviderLookup>,
}

#[derive(Debug)]
pub struct ProviderLookup {
    pub provider: ProviderId,
    pub status: LookupStatus,
    /// Validated entries per returned source; empty unless `status` is `Found`.
    pub entries: Vec<(SourceDomain, Vec<KvEid>)>,
}

/// One outcome per provider per navigation, used for logs and metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupStatus {
    Found { kept: u16, dropped: DropCounts },
    NoMatch,
    Timeout,
    HttpError(http::StatusCode),
    TransportFailed,
    InvalidResponse,
    RequestFailed,
    Skipped(SkipReason),
}

/// Validation drops for one response, by reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DropCounts {
    pub invalid_uid: u16,
    pub invalid_field: u16,
    pub unclaimed_source: u16,
    pub expired: u16,
    pub over_limit: u16,
    pub duplicate: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Consent,
    NotDue,
    Input,
    Destination,
    RateLimited,
    OptedOut,
}

/// Inputs to the free function `write_plan::plan_write`, the navigation's
/// planner (it is not a coordinator method).
///
/// `PartnerIdUpdate` (today `pub(crate)` in `ec::kv`) becomes `pub` so this
/// public trait input does not leak a private type.
pub struct WritePlanInput<'a> {
    pub ec_id: &'a str,
    pub snapshot: &'a EcKvSnapshot,
    pub new_entry: Option<&'a KvEntry>,
    /// Updates from `ts-eids` and `sharedId`, already reduced by today's rules.
    pub browser_ingest: &'a [PartnerIdUpdate],
    pub lookups: &'a LookupResults,
    /// For provider ownership.
    pub registry: &'a PartnerRegistry,
    pub now: UnixSeconds,
}

/// What to do with the mirror cookie on this response.
#[derive(Debug)]
pub enum MirrorCookie {
    Set(MirrorValue),
    Expire,
}

/// An encoded mirror value within the size limit (§10).
#[derive(Debug)]
pub struct MirrorValue(String);
```

The production coordinator:

```rust
/// The production coordinator, built once at startup.
///
/// Generic over the provider type: production uses the boxed default, tests
/// and single-provider builds use a concrete type.
pub struct ProviderCoordinator<P = Box<dyn IdentityProvider>> {
    providers: Box<[Registered<P>]>,
    /// Partner config: `openrtb_atype` for validation, `bidstream_enabled`
    /// and config order for the mirror.
    partners: Arc<PartnerRegistry>,
    budget: Duration,
}

/// A provider with the operator policy the coordinator applies to it.
struct Registered<P> {
    id: ProviderId,
    policy: ProviderPolicy,
    provider: P,
}

/// Operator policy for one provider, validated at startup.
struct ProviderPolicy {
    sources: Box<[SourceDomain]>,
    allowed_domains: Box<[Box<str>]>,
    tcf_vendor_id: Option<NonZeroU16>,
    rate_limit_per_hour: Option<NonZeroU32>,
}

impl<P: IdentityProvider> IdentityCoordinator for ProviderCoordinator<P> {
    // ...
}
```

`rate_limit_per_hour` is not a new traffic control: it carries over pull sync's
existing per-EC-hash, per-partner hourly limit (PR5), which needs the `RateLimiter`
state that pure providers cannot hold. Operators may also set it per provider
with the optional `rate_limit_per_hour` config field (§12.1), for example to
bound repeat lookups for sources a provider never resolves; it is unset by
default. Broader traffic controls remain future work (F2, F3).

The navigation path, in order:

```rust
async fn run_navigation<C, T>(coordinator: &C, transport: &T, navigation: Navigation<'_>) -> Response
where
    C: IdentityCoordinator,
    T: LookupTransport,
{
    let Navigation { permit, request, graph, ec, .. } = navigation;
    let pending = coordinator.dispatch(transport, &ec.lookup_context()).await;
    let mut response = fetch_origin(request).await;
    let lookups = coordinator.collect(transport, pending).await;
    let mut row = ec.snapshot_entry();
    let change = permit
        .confirm(response.status())
        .zip(write_plan::plan_write(&ec.write_plan(&lookups)));
    if let Some((permit, change)) = change {
        // One attempt, never retried (§9.2).
        match graph.commit_page_load(permit, ec.id(), change) {
            Ok(committed) => row = Some(committed),
            Err(report) => ec.record_write_failure(&report),
        }
    }
    ec.apply_cookies(&mut response, row.as_ref(), coordinator.mirror(row.as_ref(), ec.consent(), ec.now()));
    response
}
```

### 8.2 Validation

Validation keeps valid entries and drops only invalid ones. Each drop is
counted by reason.

| Level    | Rule                                                                                                                                                                                         |
| -------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| EID      | `source` normalizes to a source this provider claims in config; otherwise dropped.                                                                                                           |
| EID      | `inserter`/`matcher` at most 255 bytes; `mm` within OpenRTB values; `ext` a JSON object of at most 1 KiB. EID-level fields are dropped (not the entry) when `eid_level_fields` is `Ignored`. |
| UID      | `id` non-empty after trimming and at most `MAX_STORED_UID_LENGTH` (1024) bytes; `ext` a JSON object of at most 1 KiB.                                                                        |
| Both     | An `ext` (EID or UID) nests at most 32 levels deep; otherwise it is invalid as above.                                                                                                        |
| UID      | Refresh `state` non-empty and at most 2048 bytes, with a stated `from`; otherwise the refresh state is dropped and the UID kept. `from` is resolved like `exp`.                              |
| UID      | `atype` missing is filled from the source's `openrtb_atype`.                                                                                                                                 |
| UID      | `exp`, when present, must be greater than now; otherwise dropped.                                                                                                                            |
| Counts   | Entries beyond `max_eids`, or UIDs beyond `max_uids_per_eid`, are dropped from the end.                                                                                                      |
| Dedupe   | UIDs with the same (`source`, `id`) keep the first occurrence, as in §11.1.                                                                                                                  |
| Row size | See §12.3 for rows over 16 KiB serialized. This navigation's provider UIDs are dropped, last source and last UID first, until the row without its browser UIDs fits.                         |

An EID entry left with no UIDs is dropped. A response that cannot be parsed at
all is a failure for that provider; other providers and browser input are
unaffected.

### 8.3 Merge into the row

For each provider with a `Found` outcome and each source in its response:

- The validated entries replace every UID of that source in the row, including
  unowned ones (D8). Each written UID records `owner` and `exp`.
- Sources the provider claims but did not return stay unchanged.
- `NoMatch`, failures, timeouts and skipped providers leave the row unchanged.
- `OptedOut` (a cookie or a refresh reply) clears every source the provider
  claims and records the opt-out in the row's `opt_outs` map for 30 days. Until
  it lapses the provider is skipped as `skipped_opted_out` and not called, and
  a result from a lookup dispatched before the opt-out was recorded is
  discarded rather than written over it. Generated and cookie IDs merge like
  `Found`.

Browser ingest (§8.4) is applied in the same in-memory merge, before provider
results, so the write order never lets browser data overwrite provider data.

### 8.4 Browser ingest (navigation only)

Unchanged rules: registered sources only; one UID per source, the first valid
UID of the last matching entry, with `sharedId` applied last; no EID-level
fields; fill missing sources only; UIDs at most `MAX_UID_LENGTH` (512) bytes.
The planner prunes expired UIDs first, so a source holding only expired UIDs
can be filled by the same write. New rule: a provider-owned source is skipped
entirely.

### 8.5 Batch sync

Batch sync keeps its own write path. A record for a source claimed by an
enabled provider is rejected and counted as `provider_owned`; other records in
the same batch are processed as today.

### 8.6 Disabled providers

A disabled provider makes no outbound requests and is not considered for
eligibility, and its sources stop being provider-owned. UIDs it wrote earlier
keep serving until `exp`, or until replaced or removed; once pruned, browser
ingest may fill the source again. Its `tcf_vendor_id` still gates egress of
every ID for its sources, stored or sent by the browser, so disabling a
provider never releases its UIDs without that vendor's consent. Removing the
provider's entry does drop that gate (#1548). Other providers are unaffected.

## 9. KV write counting, failure and retry

### 9.1 Permit

The write limit is enforced by types. A navigation receives a
`NavigationPermit`; it becomes a `PageLoadWrite` only when the final response
is not a redirect; the write consumes it. Neither type is `Clone` or
`Copy`, `classify` is called once, by the publisher-proxy route only, and
`commit_page_load` consumes the permit, so a navigation cannot write twice and
no other request can write at all.

```rust
/// Issued to a request classified as a page-load navigation (§3.1).
#[derive(Debug)]
pub struct NavigationPermit(());

impl NavigationPermit {
    /// Returns a permit when `request` is a top-level document navigation.
    ///
    /// Called once per request, by the router only.
    #[must_use]
    pub fn classify(request: &http::Request<EdgeBody>) -> Option<Self>;

    /// Confirms the permit once the final response status is known.
    ///
    /// Returns `None` for a redirect: the redirect target is the page load.
    #[must_use]
    pub fn confirm(self, status: http::StatusCode) -> Option<PageLoadWrite>;
}

/// Proof that the current request may make the page load's single KV write.
#[derive(Debug)]
pub struct PageLoadWrite(());

// Also on the navigation permit: confirms it together with the permit for the
// after-response phase (§5.2.1).
// pub fn confirm_with_after_response(self, status: http::StatusCode)
//     -> Option<(PageLoadWrite, AfterResponseWrite)>;

/// Proof for the one follow-up update; consumed by `commit_after_response_lookups`.
#[derive(Debug)]
pub struct AfterResponseWrite(());

/// The single row change of a navigation, with its CAS precondition.
#[derive(Debug)]
pub enum RowChange {
    /// Create a row that must not exist (new EC or orphan recovery).
    Create(KvEntry),
    /// Replace the row only if it is still at `expected`.
    Update { entry: KvEntry, expected: Generation },
}

/// Why the single write did not land.
#[derive(Debug, derive_more::Display)]
pub enum KvWriteError {
    #[display("row changed since it was read")]
    Conflict,
    #[display("row already exists")]
    AlreadyExists,
    #[display("KV store rejected the write")]
    Store,
}

impl core::error::Error for KvWriteError {}

impl KvIdentityGraph {
    /// Applies the navigation's single row change in one attempt.
    ///
    /// # Errors
    ///
    /// Returns [`KvWriteError`] on CAS conflict, an existing row on create, or
    /// a store failure. Callers must not retry.
    pub fn commit_page_load(
        &self,
        permit: PageLoadWrite,
        ec_id: &str,
        change: RowChange,
    ) -> Result<KvEntry, Report<KvWriteError>>;
}
```

`Generation` is a newtype over the KV generation captured in
`EcKvSnapshot::Present`. When the snapshot carries no generation,
`plan_write` returns `None` for updates and the navigation counts
`no_generation`: TS never overwrites a row blindly.

After PR2 all browser-driven KV writes except withdrawal tombstones go through
`commit_page_load`: new EC creation, orphan recovery and browser ingest; PR3
adds provider results. Withdrawal tombstones keep their existing path (§3.1). The post-send pull-sync write is the one exception until
PR5 removes it.

### 9.2 Failure behaviour

| Failure                                             | Effect                                                                                                                     |
| --------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| CAS conflict or store error                         | No retry. Row unchanged. Mirror built from the unchanged row. New EC cookie not set. Counted.                              |
| Tombstone write fails                               | Bounded CAS retry first. Mirror expired and no IDs served (consent gating). EC cookie kept so the next request retries it. |
| EC-ID collision on create                           | Same as a CAS conflict; the next navigation generates a new EC.                                                            |
| Provider timeout/outage/invalid                     | That provider's results are dropped; the write proceeds with everything else that is valid.                                |
| All providers fail                                  | The write proceeds if browser ingest changed anything; otherwise no write.                                                 |
| Coordinator budget exceeded                         | Late responses dropped; the response is never held past the budget.                                                        |
| After-response row missing, withdrawn or EC changed | Nothing written (`no_row`); results dropped.                                                                               |
| After-response write conflict                       | Results dropped; the source stays due and is retried on the next navigation. The sent response is unaffected.              |

Failed lookups are retried only by a later navigation where the source is
still due.

## 10. Mirror cookie

| Attribute | Value                                                                                        |
| --------- | -------------------------------------------------------------------------------------------- |
| Name      | `__Host-ts-eids-mirror`                                                                      |
| Flags     | `Path=/; Secure; SameSite=Lax`; no `Domain`; not `HttpOnly`                                  |
| Max-Age   | Same as the EC cookie                                                                        |
| Value     | base64url of `{"v":1,"eids":[...]}`; each entry is an OpenRTB EID whose UIDs may carry `exp` |
| Size      | Encoded value at most 3,584 bytes                                                            |

- **Contents:** live, provider-owned UIDs of `bidstream_enabled` sources, with
  their EID-level and UID-level fields.
- **Fill order:** sources in `[[ec.partners]]` config order; entries and UIDs in
  stored order. Each UID is added if the encoded value still fits; otherwise it
  is skipped and filling continues. KV keeps the complete set.
- **Set** on every consented navigation response from the row as it stands
  after the navigation: the committed row, or the snapshot when no write
  landed. Setting a cookie is not a KV write, and this restores a mirror the
  browser lost.
- **Expired** (`Max-Age=0`) on any response where EID consent is absent or
  withdrawn, whatever happened to the KV write, and on a navigation whose
  mirror would be empty.
- **The server never reads it.** Readers on the page must drop UIDs whose `exp`
  has passed and strip `exp` before use.

## 11. Consumers

All consumers read the row snapshot and gate by current consent and
`bidstream_enabled`, as today. None write KV.

| Consumer               | Behaviour                                                                                                                                                            |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `/auction`             | Resolves the row to EIDs, full-fidelity for provider entries, dropping UIDs that are not live. Merges with the body EIDs (§11.1). Server IDs need no browser resend. |
| `/_ts/page-bids`       | Same as `/auction`.                                                                                                                                                  |
| `x-ts-eids` header     | Built from the merged set with the existing truncation flag.                                                                                                         |
| `/_ts/api/v1/identify` | Includes provider UIDs that are live, under existing authentication and partner gating.                                                                              |
| Integration proxies    | Use the same resolver where they forward EIDs.                                                                                                                       |

### 11.1 Merge for auctions

1. KV entries first, then body EIDs.
2. EID entries are distinct when (`source`, `inserter`, `matcher`, `mm`) differ;
   both are kept.
3. A UID whose `id` already appears under the same source is a duplicate; the
   first occurrence (KV) keeps its fields.
4. Distinct UIDs are kept under their entry.

## 12. Configuration, secrets and observability

### 12.1 Configuration

```toml
[ec]
# Shared time budget for all provider lookups on one navigation.
identity_budget_ms = 200
# Budget for providers with collect = "after_response", run after the
# response is sent (§5.2.1).
identity_after_response_budget_ms = 2000

[[ec.identity_providers]]
id = "example-provider"
kind = "example-kind"
enabled = true
endpoint = "https://ids.example.com/v1/lookup"
allowed_domains = ["ids.example.com"]
# Each source needs an [[ec.partners]] entry and must be in the kind's capabilities.
sources = ["ids.example.com"]
# Optional: require TCF vendor consent for this provider.
tcf_vendor_id = 9999
# Optional: "page" (default) or "after_response" (§5.2.1).
collect = "page"
# Optional: most lookups per EC hash per hour (§8.1); unset means unlimited.
rate_limit_per_hour = 4

# Secret Store key names, resolved like other secret references.
[ec.identity_providers.secrets]
token = "example_provider_token"

[ec.identity_providers.settings]
# Provider-specific settings, parsed by the provider implementation.
```

Startup validation rejects: unknown `kind`; a source with no `[[ec.partners]]`
entry; a source outside the kind's capabilities; a source claimed by two
enabled providers; an `endpoint` host outside `allowed_domains`; placeholder
secrets in deploy validation.

Validation also rejects an empty or duplicate provider `id`, an empty
`sources` list, a non-HTTPS `endpoint`, and identity budgets outside 1–1000 ms
(page) and 1–5000 ms (after-response).

PR3 registers no provider kind, so deploy validation (`ts config validate`,
`push` and `diff`) rejects every enabled provider; a push therefore cannot
take a running service down. F1 replaces that rule: the CLI builds every
enabled provider of a registered kind, as the adapter does at startup, and
rejects enabled providers for a target adapter that does not run them, so
kind-specific setting errors fail before deploy. Core's own validation cannot
do this because provider kinds live outside core. A disabled provider's
`secrets.token` is not resolved.

Secrets are Secret Store key references, resolved at startup into
`Redacted<String>` values through the existing secret path. They are never written to cookies or logs, and source-controlled
examples use placeholders.

### 12.2 Logs and metrics

Per provider and navigation, one outcome: `found`, `no_match`, `timeout`,
`http_error`, `transport_error`, `request_error`, `invalid_response`, `skipped_no_row`, `skipped_consent`,
`skipped_not_due`, `skipped_input`, `skipped_rate_limited`, `skipped_opted_out`,
`destination_rejected`; plus the totals of UIDs kept and dropped. The page-load write emits no
outcome label in PR A: a skipped write is logged with its error (a conflict,
an existing row or a store error), and an update without a generation is
logged as `no_generation`. A navigation that runs the after-response phase
(§5.2.1) logs one write outcome: `written`, `unchanged`, `no_row`, `conflict`
or `failed`. The outcome log holds the provider ID, the outcome and the kept
and dropped totals; other logs hold the EC ID only in its existing truncated
form. Raw UIDs, EID values and encrypted envelopes are never logged.

### 12.3 Compatibility and rollout

PR A changes behaviour that operators and integrators can observe:

- **EC minting.** A new EC is minted, and its row written, only on a
  top-level publisher navigation: a `GET` with `Sec-Fetch-Dest: document`,
  `Sec-Fetch-Mode: navigate`, no `prefetch` in `Sec-Purpose`, and a response
  that is not a 301, 302, 303, 307 or 308 redirect. Clients without Fetch
  Metadata get no EC, and integration proxies no longer mint one.
- **`user.id` withheld.** An EC minted on the current navigation is not sent
  as `user.id`, and that navigation's auction carries no browser `ts-eids`,
  until a later navigation finds its row.
- **Browser EIDs.** `/auction` no longer stores its body EIDs, and neither
  `/auction` nor page-bids writes `ts-eids` or `sharedId`; the next navigation
  ingests those cookies.
- **Withdrawal.** The EC cookie is expired only once every tombstone is
  written or the row is proven absent; while a tombstone fails, the cookie is
  kept so the next request retries. On main the cookie was expired first.
- **Vendor consent.** On Fastly, a provider's `tcf_vendor_id` gates every ID
  of its sources at egress, browser-supplied ones included, and keeps doing so
  while the provider is disabled (§8.6).
- **Row size cap.** Rows are capped at 16 KiB (`MAX_ENTRY_BYTES`). Batch sync
  reports `row_full`. A row written before the cap is trimmed only when a
  write also carries a real change: its largest stored records are dropped
  first, so the change that triggered the write still lands. A provider
  refresh that no longer fits keeps the record it would replace.
- **Fastly snapshot.** The final auction snapshot may wait up to 1 ms per
  partially received body and reads on into a response whose headers arrived
  by the cutoff.
- **Config blob rollback.** A blob that configures identity providers or sets
  a non-default identity budget cannot be loaded by an older build; default
  values are left out of the blob so an unchanged config stays compatible.
- **Row rollback.** Rows with a provider UID over 512 bytes fail to load on an
  older build, and an older writer drops `eids` and `opt_outs` (§6.3).
- **Providers.** PR A registers no provider kind, so deploy validation and
  every runtime refuse an enabled provider.

## 13. PR breakdown

### 13.1 Required

The work below is delivered as two pull requests:

- **PR A** — the work units PR1 to PR4, on top of the merged platform PR
  #1433, which provides the deadline-bounded `select_until` that
  `PlatformLookupTransport` uses.
- **PR B** — the work units PR5 and PR6, stacked on PR A.

PR A alone keeps the old post-send pull-sync write as a separate channel
(§9), as it is on main; PR A registers no provider kind, so no provider write
adds to it. PR B removes it. Within each pull
request the units are separate, ordered commits so they stay reviewable.

| PR  | Title                                    | Contents                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | Depends on      |
| --- | ---------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------- |
| PR0 | Spec                                     | This document.                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | —               |
| PR1 | Full-fidelity EID storage and resolution | `ec::eid_types` value types (§6.2); `KvEid`/`KvUid` additive schema; EID-level fields on `crate::openrtb::Eid`; resolver drops UIDs that are not live; auction merge (§11.1); `x-ts-eids` and identify use it. No new writers.                                                                                                                                                                                                                                                           | PR0             |
| PR2 | Page-load write permit                   | Navigation classifier; `PageLoadWrite` and `commit_page_load`; single attempt; non-navigation requests read-only except withdrawal tombstones; browser ingest on navigation only; withdrawal handling (§3.1, §5.7); integration-proxy EC minting stops.                                                                                                                                                                                                                                  | PR0             |
| PR3 | Provider trait and coordinator           | `ec::provider` (`IdentityProvider`, `ProviderKind`), `ec::coordinator` (`IdentityCoordinator`, `ProviderCoordinator<P>`, `LookupTransport`, `PlatformLookupTransport`), provider crate with no kinds, config and validation, consent, allowlist, budgeted fan-out, validation, merge into the navigation write, provider-owned skip in ingest, metrics, example config.                                                                                                                  | PR1, PR2, #1433 |
| PR4 | Batch sync on provider-owned sources     | Reject and count records for sources an enabled provider claims.                                                                                                                                                                                                                                                                                                                                                                                                                         | PR3             |
| PR5 | Pull sync as a provider                  | `pull-sync` kind built from existing `[[ec.partners]]` pull-sync settings with no config change; existing per-partner rate limit kept as provider-scoped eligibility; post-send dispatcher and completeness marker removed, leftover marker cookies expired. Behaviour change for existing pull-sync partners: their sources become provider-owned, so browser ingest skips them and, with PR4, batch sync rejects them; once PR6 lands their IDs are mirrored when `bidstream_enabled`. | PR3             |
| PR6 | Mirror cookie                            | Adds `IdentityCoordinator::mirror` (PR3 ships the trait without it); encoding, fill order, set and expiry rules (§10).                                                                                                                                                                                                                                                                                                                                                                   | PR3             |

### 13.2 Optional and future (Tech Lead decision)

| ID  | Task                              | Notes                                                                                                                                                                                                                                                          | Depends on |
| --- | --------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------- |
| O1  | tsjs reads the mirror (D6b)       | Reads the mirror, drops expired UIDs, strips `exp`, passes entries to Prebid user IDs. Separate PR.                                                                                                                                                            | PR6        |
| F1  | Concrete provider implementations | R§5.1. One spec and one PR per vendor named in R§6, each a module and `build` arm in the provider crate. Also adds cookie reads to the core coordinator (`LookupInput::cookies`, `read_request_cookies`) and the kind- and adapter-aware deploy check (§12.1). | PR3        |
| F2  | Provider lookup traffic controls  | R§5.2. Best-effort dedup per EC and provider, bounded no-match caching kept distinct from failures, refresh jitter. Strict cross-instance dedup is a separate decision.                                                                                        | PR3        |
| F3  | Per-provider cooldown             | R§5.3. Pause after repeated failures, then probe for recovery.                                                                                                                                                                                                 | PR3        |
| F4  | Provider debug mode               | R§5.4. A mock `LookupTransport` runs real providers and the real coordinator against configurable responses; no real provider calls, no production KV or cookie mutation.                                                                                      | PR3        |

## 14. Acceptance scenarios

1. **First navigation, consent granted.** No EC. One provider is enabled and
   due. → One KV create holding the browser-ingested UIDs; the EC cookie is
   set; the provider is not called (§8.1). The next navigation finds the row,
   calls the provider once and writes its UIDs (with owner and `exp`) in one
   CAS update; the mirror is set on that response.
2. **One write per page load.** A navigation is followed by three `/auction`
   calls, one `/_ts/page-bids` call and two integration-proxy calls, all
   carrying new `ts-eids` content. → At most one KV write, on the navigation.
3. **Same-page and next-page availability.** Provider IDs written by the
   navigation → the next page load's `/auction` contains them without the
   browser resending them; a same-page `/auction` contains them only if it
   read the row after the write.
4. **Slow provider.** Provider A answers in 50 ms, provider B in 500 ms, budget
   200 ms. → A's UIDs are written; B's response is dropped and counted as
   `timeout`; the response is not held past the budget.
5. **Partial invalid response.** A provider returns three UIDs, one empty and
   one over 1024 bytes. → One UID is stored; two drops are counted.
6. **Write failure.** The navigation's CAS fails. → No retry; mirror built from
   the unchanged row;
   on a new EC no EC cookie is set; the next navigation looks up again.
7. **Expiry.** A provider UID's `exp` passes. → It is excluded from `/auction`,
   `x-ts-eids`, identify and the mirror; the next navigation asks only its
   owning provider.
8. **No expiry.** A provider UID without `exp` → served until the provider
   replaces it or consent is withdrawn; never refreshed unless it carries refresh state
   (scenario 24).
9. **Withdrawal on a navigation.** → Tombstone written; no provider calls; EC
   cookie and mirror expired; no IDs served.
10. **Withdrawal on `/auction`.** → Tombstone written (§3.1), EC cookie and
    mirror expired, no IDs served.
11. **Tombstone beats a concurrent write.** Tab A writes a tombstone while tab
    B's navigation is collecting provider results. → B's CAS fails and is not
    retried; the provider IDs are never stored or served.
12. **Provider-owned source.** A browser `ts-eids` UID and a batch record arrive
    for a source an enabled provider claims. → Browser UID ignored; batch
    record rejected and counted; provider UIDs unchanged.
13. **Multi-source provider.** One provider returns entries for two claimed
    sources with two UIDs each and EID-level fields. → Both sources stored with
    all four UIDs and fields, each owned by that provider; `/auction` forwards
    them at full fidelity.
14. **Disabled provider.** One of two providers is disabled. → No outbound
    request to it; the other provider and browser ingest behave as before.
15. **Destination rule.** A provider builds a URL whose host is not in
    `allowed_domains`. → No request is sent; counted as `destination_rejected`.
16. **Hashed-email source.** A source requires `HashedEmail`. → Skipped before
    any call, counted as `skipped_input`.
17. **Mirror trimming.** Provider UIDs exceed 3,584 encoded bytes. → The mirror
    holds the deterministic config-order fill of §10; KV holds all of them.
18. **No secrets or IDs in logs.** Every outcome above produces logs and metrics
    without raw UIDs, envelopes or secrets.
19. **Nothing due.** A returning visitor with all owned UIDs live and no new
    browser input. → No provider calls, no KV write.
20. **Redirect.** The navigation's origin response is a 302. → No KV write; the
    redirect target navigation may write.
21. **Pull-sync partner after PR5.** A partner with pull sync enabled also
    receives a browser `ts-eids` UID and a batch record. → Both are ignored or
    rejected as for any provider-owned source; the pull-sync provider's UID is
    stored and served.
22. **After-response provider.** Provider A is `collect = "page"` and answers
    in 50 ms; provider B is `collect = "after_response"` and answers in
    900 ms; page budget 200 ms. → The response is sent without waiting for B;
    the page write holds A's UIDs and browser ingest; after the response, one
    CAS update adds B's UIDs with owner and `exp`; the next page load serves
    them.
23. **After-response on a withdrawn or changed row.** The row is tombstoned,
    or written by another request, between the page write and B's result. →
    No row is created or revived; on a generation conflict B's results are
    dropped and B is asked again on the next navigation.
24. **Provider refresh.** A provider-written UID is live until 5000 and holds
    refresh state with `from` 900; the navigation is at 1000 and the request carries no usable vendor
    cookie. → The source is
    due; the provider's request is built from the stored state; the refreshed
    UID and its new refresh state are written. With `from` 2000 the source is
    not due and nothing is sent.
25. **Refresh reports an opt-out.** A refresh reply reports an opt-out. → The
    provider's sources are cleared from the row; nothing is stored for them.
26. **Generated ID.** A provider that generates IDs has a due source and no
    cookie ID. → No request is sent; the generated UID is validated and written
    with its owner; on the next navigation the source holds a live UID and is
    not regenerated.
27. **Refresh state is never served.** A row holds a UID with refresh state.
    → `/auction`, the mirror cookie and `x-ts-eids` carry the UID's `id`,
    `atype` and `ext` only.

## Review log

### Round 1 (2026-11-09, PR #1487)

Reviewer findings and resolutions:

- Withdrawal outside permitted navigations: tombstone moved outside the
  permit with bounded CAS retry (§3.1).
- Stored provider UIDs served after vendor consent withdrawal: egress gated on
  the owning provider's TCF vendor ID.
- Config blob and 1024-byte UIDs breaking rollback: identity defaults left
  out of the blob; non-provider writers stay at 512 bytes.
- Unknown kinds skipped silently, duplicate provider IDs, deeply nested `ext`,
  batch sync aborting on a full row, minted ECs sent to vendors: fixed.
- Negative caching (#1547), a maximum provider UID lifetime (#1548),
  after-response retry (#1553), lenient readers (#1554) and client EIDs after
  an opt-out (#1555): deferred to follow-up issues.

### Round 2 (2026-11-10/11, PR #1487 at 889159cd6)

- Opt-out race on after-response and confirmation-read adoption: results of
  an opted-out provider are discarded (§8.3).
- Size cap: trimming alone plans no write; this navigation's browser UIDs are
  dropped before any stored record.
- Lookups only for an EC with an existing consented row (§8.1).
- Deploy validation rejects enabled providers while no kind is registered;
  disabled providers' secrets are not resolved (§12.1).
- Disabled providers keep their vendor-consent gate on stored UIDs; `ext`
  nesting bounded to 32 levels; raw client IP passed to providers.
- Spec drift corrected (§5.3, §6.3, §7, §8.4, §9.2, §12, §13).

### Round 3 (2026-11-12, PR #1487 at 4e7a6e02b)

- Size cap: round 2's fix was incomplete. A row over the cap that gains an ID
  now loses its largest stored records first, so the added ID lands; this
  navigation's provider UIDs compete with the stored row only, and a refresh
  that no longer fits keeps the record it replaces (§8.2, §12.3).
- A remembered opt-out clears only sources the row still stores, so the
  after-response path skips its re-read once they are gone.
- Orphan lookup retargeting removed: dispatch never looks up an EC without a
  row. Such lookups are counted as `skipped_no_row` (§12.2).
- Client-EID vendor filter tested at both auction call sites; main's direct
  tombstone tests restored.
- Remaining drift corrected (§3.1, §5.3, §6.2, §8.1.2, §8.2, §8.6, §9.1,
  §12.2, §12.3, §13.1, §14). Round 2's "Spec drift corrected" missed these.
- Deferred: deploy validation per adapter and kind (#1552), a full-row pull
  sync loop (#1551).

### Round 4 (2026-11-12, PR #1487 at 1ec3be72e, Claude Code (Opus 5.5) review agent)

- No blocking findings. Re-checked the round-3 changes: the size cap's
  over-cap trimming, provider UID dropping and stored-record restore; the
  after-response re-read skip (a newly reported opt-out always clears at least
  one source, since every provider claims one); `skipped_no_row` precedence
  over consent; and that no caller still needed orphan retargeting.
- §5.2.1 now states that the after-response phase skips its read when no
  result can change the row.

### Round 5 (2026-11-12, PR #1487 at 7a291be23, Claude Code (Opus 5.5) review agent)

Angle: data flow and edge cases end to end, from dispatch through
validation, the write planner, the page-load and after-response commits, to
egress.

- Blocking, fixed: a disabled provider's `tcf_vendor_id` was dropped from the
  egress gate when an enabled provider claimed the same source, so its stored
  UIDs were served under the new owner's vendor consent only, or with none
  when the new owner sets no vendor ID. This broke §8.6. Each partner now
  keeps the vendor IDs of every configured provider claiming its source and
  is withheld unless all have consent, in the bidstream, identify and the
  client-EID filter.
- Non-blocking, no change: a live UID written by a provider that no longer
  claims the source keeps the source not due for its new owner until the UID
  expires (§5.6, §8.6 "keep serving until `exp`"). This is the intended
  behaviour, already pinned by a test; the §4 "due source" entry reads
  "owned UIDs" as UIDs on a provider-owned source.

### Round 6 (2026-11-12, PR #1487 at 6719f19ea, Claude Code (Opus 5.5) review agent)

Angle: adversarial. Malformed provider replies and browser input, config
combinations and adapter differences, on the tree after round 5's fix.

- No blocking findings. Re-checked round 5's gate across enabled, disabled
  and reassigned providers and all three egress paths; provider replies with
  unclaimed, unnormalized or oversized sources, empty or oversized UIDs,
  negative `atype`, deep or non-object `ext`, past expiry and unusable
  refresh state; per-phase coordinators validating only their own providers
  while deploy validation covers the whole set; zero or out-of-range budgets,
  vendor IDs and rate limits rejected at parse.
- Non-blocking, no change: a client EID whose `source` fails normalization,
  for example with surrounding whitespace or a port, matches no partner and
  so passes the vendor filter unchanged. Such a source names no configured
  partner; the page that sends it controls the request anyway.
- Non-blocking, no change: only the Fastly adapter builds the registry with
  provider ownership and vendor gates (§12.3 "On Fastly"); per-adapter deploy
  validation stays with #1552.
