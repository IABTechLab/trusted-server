# Design Spec: Pluggable Edge Cookie, Device, and Geo Providers

**Status:** Implemented in PR #1043 (Edge Cookie provider seam) and PR #1044
(device and geo selection); revised against the implementation, 2026-08-25.
**Author:** Engineering
**Issue references:** #777, #778, #780, #781
**Related specs:** `2026-07-30-permission-model-design.md`,
`2026-07-30-provider-migration-rollout-design.md`,
`2026-07-30-client-cycle-ec-resolve-design.md`
**Last updated:** 2026-08-25

> **Context.** PR #838 proposed a first implementation of this epic in a single
> change. Review of that PR surfaced design gaps this spec exists to close
> before a second implementation pass: an identity abstraction that owned
> minting but not recognition, per-adapter divergence in provider selection,
> silent misconfiguration modes, and speculative trait surface with no
> production caller. This spec is the authoritative statement of what the
> provider architecture must do; where it contradicts PR #838, this spec wins.
> The second pass has now landed (PR #1043 and PR #1044, with permission
> enforcement in PR #1045), and this revision restates the spec to match the
> implemented code. A final section records every divergence from the
> 2026-07-31 draft.

---

## 1. Overview and goals

Trusted Server makes three per-request data decisions that were previously
hard-wired: whether to create or keep an Edge Cookie (EC) identity, how to
classify the requesting device, and whether to resolve geolocation. Each is
now a **provider**, a selectable component chosen in operator configuration,
with a deliberately neutral default.

Goals, as implemented:

- A deployment picks an implementation per concern (including none) without a
  code change to Trusted Server core.
- Defaults are neutral. With no configuration, no EC is created, device
  classification uses only the User-Agent, and no geolocation is performed. A
  default deployment makes no third-party or host-specific call.
- An **EC provider declares** the permissions its data use requires
  (`required_permissions` on the trait), and **core enforces** that
  declaration before minting or using an identity. A provider cannot
  authorize itself. The enforcement machinery is the permission model's
  subject and lands with it in PR #1045 (see the permission model spec).
  Geo and device carry the same declaration method with an empty default,
  for the reasons spelled out in section 5.
- All adapters (Fastly, Axum, Cloudflare, Spin) route selection through the
  same core builders, so identical configuration selects identical providers
  everywhere. A selection the deployment cannot satisfy fails loudly rather
  than degrading. The EC API routes (identify, batch-sync, ec/resolve) are
  registered by the Fastly entry point only today, because the portability
  adapters do not yet wire a platform KV store. The Spin adapter's route
  list documents that gap explicitly rather than leaving those paths silent.

Non-goals:

- No vendor provider ships in this epic beyond the host-platform
  implementations named below. The `crates/edgecookie/` directory holds a
  README describing where vendor EC crates will live.
- The client-cycle (browser round-trip) provider type has its own spec. The
  trait ships the seam for it (`resolve_from_client`, a no-op by default)
  and a demonstration provider (`client-fixed`) compiled only into test and
  demonstration builds. Production selection of the demo provider is a
  startup error.

## 2. Provider taxonomy

| Concern     | Trait                | Built-in default            | Opt-in implementations                                                                                                                        |
| ----------- | -------------------- | --------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| EC identity | `EdgeCookieProvider` | none (stateless)            | `hmac` (in core, HMAC over client IP, preserves today's identity), `host-signals` (in core, see below), and `client-fixed` (demo builds only) |
| Device      | `DeviceProvider`     | `builtin` (User-Agent only) | `fastly` (TLS JA4 and HTTP/2 signals through an injected `HostSignals` service)                                                               |
| Geo         | `PlatformGeo`        | none (no location)          | `platform` (host geo lookup)                                                                                                                  |

The geo trait is the existing `PlatformGeo` in `platform/traits.rs` rather
than a new `GeoProvider` name. The EC trait lives in `ec/provider.rs` and the
device trait in `ec/device.rs`.

Selection keys are strings in operator configuration
(`trusted-server.example.toml` carries the commented template):

```toml
[ec]
provider = "hmac"

[ec.providers.hmac]
passphrase = "replace-with-32-plus-byte-random-secret"

[device]
provider = "builtin"   # default. "fastly" opts into TLS/H2 signal evidence

[geo]
provider = "platform"           # default is none (no location, no host call)
default_country = "FR"          # required (section 6)
# assume_single_jurisdiction = true   # required when EC runs with no geo
```

**The `host-signals` EC provider** (identity from HMAC over the host TLS JA4
and HTTP/2 fingerprints plus the client IP) was deliberately dropped from the
2026-07-31 draft. It has since shipped in PR #1044 as an opt-in built-in
(`[ec.providers.host-signals]`), implemented against the host-agnostic
`HostSignals` capability rather than a Fastly API, so any host that supplies
the fingerprints can run it and a host that supplies none cannot build it.
When the host supplies no fingerprint at all the provider defers with a
warning instead of degrading to an IP-only identifier under the host-signals
name. **An open review question stands on whether this provider should ship
in the series at all**, because its identifier shape shares the built-in
HMAC grammar and a sign-off row defers host fingerprint processing. The
question is flagged for the series review and this spec does not present
the provider as settled either way.

## 3. The identity lifecycle contract

This is the section PR #838 lacked. Its trait abstracted **minting** an
identifier but left **recognition** and **KV key normalization** hard-coded
to the built-in HMAC shape, so a provider whose identifiers did not match
`{64hex}.{6alnum}` minted cookies that the very next request discarded.

The implemented contract routes every lifecycle operation core performs on
an EC value through the selected provider:

| Lifecycle operation | Where core uses it                                                                                                                                                                                                                                                | Contract                                                                                                                                                                                                                                                                                                                                                                                                                              |
| ------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Mint**            | EC generation on first eligible request, and the client-cycle resolve endpoint                                                                                                                                                                                    | The provider returns the identifier (`generate` server-side, `resolve_from_client` for the client cycle) and only core writes the cookie, after enforcing the global bounds below.                                                                                                                                                                                                                                                    |
| **Recognize**       | Reading `ts-ec` back from the request, deciding `ec_was_present`, withdrawal checks, and every path that hands the value onward: the origin URL in `append_ec_id`, the click-target URL in `handle_first_party_click`, and the proxied body an integration builds | `accepts_id` answers whether a value is a well-formed identifier the provider issues. A value the selected provider does not recognize is treated as absent, so it is never used or egressed, while the raw cookie value stays visible to withdrawal handling. The egress paths reach the same answer through `edge_cookie::recognized_ec_id`, and a deployment with no provider selected recognizes nothing and so egresses nothing. |
| **KV key**          | Identity-graph row reads and writes                                                                                                                                                                                                                               | `normalize_id_for_kv` returns the key form. The default lowercases the built-in HMAC hash segment and preserves the suffix, keeping today's keys. An opaque or case-sensitive provider overrides to the identity function so distinct identifiers never collapse into one row.                                                                                                                                                        |
| **Withdraw**        | Expiring the cookie and writing revocation markers                                                                                                                                                                                                                | The identifiers eligible for a **graph tombstone** are exactly those the selected provider owns, dispatched on the `{code}~` prefix first and then `accepts_id`, never a shape check the provider cannot influence. Expiring the **cookie** is broader: it keys off the raw cookie being present, so it still fires for an identifier the selected provider does not own (see the switching case, §6.1).                              |

**Invariant:** for every provider `P` and every identifier `id` minted by
`P`, `id` round-trips read-back byte for byte. A test in `ec/mod.rs` proves
the round-trip with a non-default provider whose identifiers are opaque, and
a second test in `ec/resolve.rs` proves the client-cycle value survives the
full scenario verbatim.

The draft's richer lifecycle surface, a canonicalizing `parse` with
per-provider equivalence fixtures, a core-constructed graph key built from a
provider `graph_key_suffix`, a declared cluster-prefix capability, declared
namespace descriptors with a startup disjointness proof, and a reusable
conformance suite driven by fixtures, is **not implemented in these PRs**.
Recognition plus KV normalization proved sufficient for the operations core
actually performs today, and each deferred piece is tracked as follow-up
work rather than silently dropped (see the revision record). Until the key
grammar lands, the KV key is the provider's normalized identifier verbatim,
which keeps every pre-epic HMAC row reachable.

The pre-epic IP-cluster prefix listing runs unchanged, but the key space it
lists over does not. A fresh mint is keyed `hmac~<hash>.<suffix>`, so the
prefix `evaluate_cluster` derives is `hmac~<hash>` for a coded row while a
legacy bare row still lists under `<hash>` on its own. Prefix matching is
anchored at the start of the key, so two rows for the same client IP that
straddle the envelope never count each other, and `cluster_size` under-reports
for as long as both populations coexist.

That undercount is accepted rather than bridged, for three reasons.
`cluster_size` is reported in the identify response and gates nothing: the
only place its value is read at all is a cache short circuit in
`evaluate_cluster` that tests whether a value is stored, not what it is, and
the `cluster_trust_threshold` and `cluster_recheck_secs` settings that a
reader might expect to gate on it have no readers in the code. The undercount
is bounded by the legacy bare-identifier read window in section 6.1 and ends
when the last pre-epic cookie expires. And bridging it would mean a second
prefix scan on every identify request for the whole of that window. It becomes
a real fault only if a later change makes the count gate something, and that
is the change that has to build the bridge.

One global rule sits above every provider, and it is implemented:

- **Identifier bounds.** A minted identifier obeys a global cookie-safe
  alphabet (normatively `[A-Za-z0-9._~-]`, valid cookie octets with no
  separators, whitespace, or control characters) and a global maximum of
  **256 bytes**, stated here so dependent documents reference one number.
  The bound applies to the identifier itself, not only its key form. Core
  enforces the bound wherever an identifier enters the system, at mint
  (both `generate` and the resolve endpoint), at cookie read-back, and at
  cookie write. The constant is `MAX_EC_ID_LEN` in `ec/cookies.rs`. A
  violating value is rejected outright and logged. No sanitizing rewrite
  exists anywhere on the path, so an identifier survives byte for byte or
  not at all, and the cookie value and the identity-graph key can never
  silently diverge.

## 4. Trait surface: minimalism rule, and where it does not apply

Every trait method must have at least one production (non-test) caller in
the same change that introduces it. **This rule does not apply to an evidence
interface, and applying it there was a mistake we made and are correcting.**

An evidence interface describes what a request carries, not what today's code
happens to read. Holding it to the caller rule produces an interface that grows
a method each time a vendor arrives, which is not something a vendor can write
against and cannot be stable across a release. It also puts the boundary in the
wrong place, because what a provider may see is not the control. What a provider
may do with what it sees is the control, and that is the permission model.

So `RequestInfo` carries everything the request carries, whether or not code in
this repository reads it yet. The rule stands for behavioural traits, where a
method with no caller really is dead weight. How the surface observed in PR #838
resolved in the implementation:

- `keys_equal`: **not shipped.** Its legitimate purpose (equivalent-envelope
  comparison, #778) is served structurally, because read-back acceptance and
  KV normalization both route through the provider, so no comparison method
  exists to leave uncalled.
- `GeneratedEdgeCookie::response_headers`: **shipped, with a production
  caller.** EC finalization applies provider-requested headers to the
  outbound response, and the client-cycle resolve path returns them, which
  is how a client-side provider requests further evidence from the page.
  The draft banned the field when nothing consumed it. The consumer landed
  in the same series, satisfying the rule the ban enforced.
- `IdentityInput.permissions` / `IdentityInput.consent`: **shipped and
  populated.** The organic mint path passes the request's resolved
  permission state and consent context so a provider can read them for
  behavior beyond gating. The gate itself has already run before `generate`
  is called, so a provider cannot use the fields to authorize itself.
- `required_permissions` on `DeviceProvider` and `PlatformGeo`: **present,
  with an empty default and no enforcement point.** The draft removed the
  method from both traits because PR #838's copies were decorative. The
  implementation keeps one uniform declaration seam across all three traits
  instead. The built-in device and geo providers declare empty sets, and
  core enforces the declaration only for the EC provider (section 5), so
  nothing reads as a gate that is not one. The geo circularity argument
  stands unchanged and is restated in section 5.

The implemented `EdgeCookieProvider` surface (`ec/provider.rs`):

```rust
pub trait EdgeCookieProvider: Send + Sync + core::fmt::Debug {
    /// Stable configuration key ("hmac").
    fn id(&self) -> &'static str;
    /// Registered four-character code (provider-code-registry.md), the
    /// `{code}~` namespace of every identifier the provider mints.
    /// Mandatory, no default: a provider cannot exist without a unique
    /// code, so identifiers from different providers can never collide.
    fn code(&self) -> ProviderCode;
    /// Derives an identifier from the provider's injected services and the
    /// request evidence passed at call time. A client-side provider defers
    /// here (returns no id) and mints later in resolve_from_client.
    fn generate(
        &self,
        request_info: &dyn RequestInfo,
        input: &IdentityInput<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>>;
    /// Whether `value` is a well-formed identifier this provider issues.
    /// Default: the built-in HMAC shape (`<64 hex>.<6 alphanumeric>`).
    fn accepts_id(&self, value: &str) -> bool { /* built-in shape */ }
    /// The KV-key form of `value`. Default: lowercase the HMAC hash
    /// segment, preserve the suffix. Opaque providers return the value
    /// unchanged.
    fn normalize_id_for_kv(&self, value: &str) -> String { /* ... */ }
    /// Permissions this provider's data use requires. Default: none, so a
    /// vendor-neutral provider requires no permission.
    fn required_permissions(&self) -> PermissionSet { /* none */ }
    /// Client-cycle counterpart to generate: mints from a value the page
    /// posted to the resolve endpoint, after verifying it. Default: no-op,
    /// so a server-side provider does not participate. See the
    /// client-cycle spec.
    fn resolve_from_client(
        &self,
        input: &ClientResolveInput<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> { /* ... */ }
}
```

Core owns the code envelope. At mint it prefixes the provider's value with
`{code}~`, at read-back it strips and checks the code before the provider's
`accepts_id` sees the value part, and the identity graph key preserves the
code verbatim around the provider's canonical form. A cookie carrying
another provider's code is treated as absent, never adopted, so switching
providers cannot silently mix identity populations, and a withdrawal always
acts on a key that can only belong to one provider. The built-in HMAC
provider mints `hmac~<64 hex>.<6 alphanumeric>` and dual-reads its
pre-envelope bare form for one release cycle so deployed cookies keep
working; the bare form belongs to hmac alone. Codes are allocated
append-only in `provider-code-registry.md`, and a leading digit is valid
(`51dd`).

The draft's alternative shape (`parse` returning a typed `EcId`,
`graph_key_suffix`, `cluster_prefix`, `verify`, and a version-carrying
`GeneratedIdentity`) was not adopted. `verify`, provider versions, and
`mint_version` are tracked follow-up work with the migration spec.
Request data reaches a provider through injected services and the
`RequestInfo` passed at call time, not through a fixed parameter struct.
`RequestInfo` carries the evidence a provider in this workspace reads today,
which is the normalized client IP. Further evidence (headers, cookies, client
hints, the URL) is added to it as a defaulted accessor in the change that first
reads it, so an existing implementation keeps compiling and no accessor lands
ahead of the caller that consumes it.

## 5. Permission enforcement is core's job, for EC providers

Before minting through an EC provider, core resolves the request's
permission state and refuses when the provider's `required_permissions()`
are not all set. The gate is implemented in `EcContext`. The selected
provider is built once at request read time, its declaration is checked
against the resolved state, and generation is skipped (with a log line
naming the jurisdiction) when the requirement is not met. With no provider
selected, nothing may mint or use an identifier, so the gate is closed
rather than open by default. The enforcement point lands with the
permission model in PR #1045, and the permission model spec governs the
resolution machinery (country and region baselines, signals, and the
requires-signal floor).

**Recognition and withdrawal always run**, permissions or not. Read-back
acceptance and withdrawal eligibility go through `accepts_id` with no
permission check, and withdrawal handling keeps the raw cookie value even
when the identifier is treated as absent, so an opt-out can always reach
the identity it revokes. A blanket execution gate would refuse to run the
provider in exactly the state an opt-out produces.

The draft additionally specified an identity activation protocol (a
two-record commit point before any egress), rowless-cookie classification
and per-prefix withdrawal records, negative-record admission rules, and a
typed egress boundary (`AuthorizedIdentity<Scope>`,
`RedactedRequestView`). **None of that is implemented in these PRs.**
Those positions remain recorded in the draft and are tracked as follow-up
work with the permission model spec, which owns identity-state persistence
and egress typing. The revision record lists them as deferred.

The gate applies to EC providers **only**. Geo and device are ungated for
two different reasons, stated separately because only one of them is
structural:

- **Geo: circularity.** The permission set is resolved from jurisdiction,
  which is resolved by the geo provider. Gating geo on the resolved set is
  unsatisfiable. `PlatformGeo::required_permissions` exists with an empty
  default for interface uniformity, and nothing consults it on the lookup
  path.
- **Device: host evidence is an explicit opt-in, not authorized by
  selection defaults.** Device classification is not an input to permission
  resolution. The neutral `builtin` classifier reads only the User-Agent
  and makes no host call. The draft went further and made selecting a
  fingerprint-reading device provider a startup error pending a separate
  security design. The implementation instead ships `[device] provider =
"fastly"` as a selectable opt-in. The Fastly adapter injects a
  `HostSignals` service carrying the TLS JA4 and HTTP/2 fingerprints, and
  the provider uses them to strengthen the browser/bot gate that guards EC
  writes. Identity rows persist the derived classification fields (the JA4
  class segment and a 12-hex-character hash prefix of the HTTP/2 SETTINGS
  fingerprint), not raw fingerprints, and the neutral default persists
  neither because the builtin provider produces no such fields.

## 6. Selection, validation, and failure modes

All configuration validation happens at **settings construction**
(`Settings::finalize_deserialized` runs every check below), so a
misconfiguration expressible in configuration alone is a startup error,
never a silent behavior change. A selection that only the running host can
satisfy (an injected vendor provider, or host fingerprints) fails loudly
when the provider is built, stopping the request rather than degrading.

| Configuration state                                                            | Behavior                                                                                                                                                                                                                                              |
| ------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `[ec] provider` set, its `[ec.providers.<key>]` block missing                  | Startup error naming the missing block. There is no closed key list in core for EC, because a vendor key is legitimate when its block is present, so an unknown key with no block fails this same check.                                              |
| `[ec.providers.<key>]` block present, `provider` unset                         | **Startup error.** (In PR #838 this silently ran stateless. The half-migrated config becomes a production identity outage detected by revenue drop. Rejecting it is the fix.) An operator who genuinely wants stateless deletes the block.            |
| `provider = "none"` (explicit stateless)                                       | Valid, and means exactly what omitting the selector means. Any configured provider block alongside it is a startup error, the same stray-block rule as below.                                                                                         |
| A configured `[ec.providers.<key>]` block that is not the selected one         | **Startup error** (checked for the `hmac` block and every vendor block). An unreferenced block is almost always a mistyped selector or a stale block, and accepting it silently invites configuration drift.                                          |
| A selected vendor key whose provider the adapter did not inject                | Loud failure when the provider is built, naming the key, so the deployment never silently runs stateless.                                                                                                                                             |
| `provider = "host-signals"` on a host that supplies no fingerprints            | Loud failure when the provider is built. A host that cannot produce `HostSignals` cannot run the provider.                                                                                                                                            |
| `provider = "client-fixed"` in a production build                              | Startup error. The demonstration provider is compiled only behind the `client-fixed-demo` cargo feature.                                                                                                                                              |
| No `provider`, no providers block                                              | Valid, the neutral default for that concern.                                                                                                                                                                                                          |
| Deprecated `[ec] passphrase`                                                   | Migrated to `provider = "hmac"` with the passphrase in `[ec.providers.hmac]`, with a deprecation warning naming the new location. Both forms together are rejected so a half-edited file fails loudly instead of one form silently winning.           |
| Any unknown key in `[ec]`, `[device]`, `[geo]`, or a built-in provider block   | Startup error. `deny_unknown_fields` is on `Ec`, `DeviceConfig`, `GeoConfig`, and both built-in provider config structs, so a typo like `providr`, or a key from a deferred feature (`legacy_providers`, `rewrite_legacy`, `versions`), fails loudly. |
| `[device] provider` names an unknown key                                       | Startup error. Valid keys are `builtin` (default) and `fastly`.                                                                                                                                                                                       |
| `[geo] provider` names an unknown key                                          | Startup error. Valid states are unset (default, no geolocation), `none` (the same, spelled out), and `platform`.                                                                                                                                      |
| `[geo] default_country` unset, or matching no `permissions.yaml` rule          | **Startup error.** The value is the permission baseline for a request the geo provider leaves unmatched, so there must always be one and the value must resolve to a real rule.                                                                       |
| An EC provider configured, no geo provider, `assume_single_jurisdiction` unset | **Startup error.** With geolocation off, every request resolves as `default_country`, so a visitor from any other jurisdiction silently receives the default jurisdiction's rules. That is acceptable only as an explicit operator decision.          |

One draft row was not adopted, the startup error for a minting provider
with no identity-graph store. `[ec] ec_store` remains optional, because the
portability adapters run without platform KV. The client-cycle resolve
endpoint refuses to mint when no graph is available (a cookie without a row
could never be withdrawn through the graph), and the organic path persists
the row whenever the graph is configured. Whether configuration should
force the pairing is follow-up work with the migration spec.

Vendor provider blocks deserve their own note. Any `[ec.providers.<key>]`
block whose key is not a built-in is captured in core as raw values (a
flattened map), and the adapter that injects the vendor provider
deserializes its own block into the vendor crate's config type. Core never
names a vendor, so a new provider adds nothing to core. The vendor crate
applies its own `deny_unknown_fields` when it deserializes.

### 6.1 Provider switching: what a switch actually does

Switching `[ec] provider` **retires every identity the previous provider
minted**. This section says exactly what that means, because a deployer has
to plan around it rather than discover it.

The draft specified an ordered `legacy_providers` reader list, provider
`versions` with `mint_version` rotation, provenance tagging, and retirement
evidence rules, as the mechanism that would carry identities across a
switch. **None of that is implemented in these PRs.** The keys are rejected
as unknown, and the design is tracked follow-up work with the migration
spec. Until it lands there is no continuity across a switch of any kind.

An earlier version of this section claimed shape-based continuity, that old
cookies stay recognized when the newly selected provider accepts their
shape. That is not what the code does and never was once core took
ownership of the `{code}~` envelope (§5). Ownership is decided on the code
prefix **before** any provider is asked about the shape, so a newly selected
provider rejects every identifier the previous one minted, whatever its
shape, because the code differs.

What a switch does, precisely:

- **Read-back.** Every identifier carrying the retired provider's code is
  treated as absent. It never becomes the request's active identity, never
  egresses to a partner, and is rejected on the pull-sync, batch-sync and
  admin paths too. This is the §5 guarantee and it is the half of the
  behavior that matters most: two providers' identity populations can never
  mix.
- **The browser cookie.** A later withdrawal still expires the `ts-ec`
  cookie, because that path keys off the raw cookie being present rather
  than off who owns it. The browser stops carrying the retired identifier.
- **The identity-graph rows.** A later withdrawal does **not** tombstone the
  retired provider's rows. Core cannot derive their canonical keys, because
  the canonical form is the owning provider's own normalization and the
  owning provider is no longer configured. Those rows stay as they are until
  their one-year entry TTL expires.

**What a deployer must do about revocation.** Treat a provider switch as a
one-way retirement of the identity population, and deal with the previous
provider's rows before or alongside the switch, not after. A withdrawal
that arrives after the switch clears the browser but leaves the row. Every
row a retired provider wrote shares that provider's `{code}~` key prefix, so
the set is identifiable and can be listed and cleared with the platform's
own KV tooling. Either clear it at the switch, or accept that the rows
persist until the one-year TTL expires and that withdrawals arriving in the
meantime are recorded only in the browser. Do not switch providers while
identities are live that the deployment may still be obliged to revoke in
the identity graph.

The `cluster_fallback` degradation policy from the draft is likewise
deferred with the cluster capability itself.

### 6.2 Runtime failure modes

Startup validation covers configuration. This covers a healthy
configuration meeting an unhealthy runtime. Implemented behavior, each row
logged, none silent:

| Failure                                                        | Behavior                                                                                                                                                                   |
| -------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `generate` returns an error                                    | No identity this request. The organic caller logs at error level and the request proceeds stateless. No cookie is written.                                                 |
| A provider mints an identifier outside the global bounds       | Rejected at mint, never rewritten. The organic path yields no identity. The resolve endpoint returns 400.                                                                  |
| Identity-graph write fails at mint                             | The mint is undone (no identifier, no cookie), with the error logged. The resolve endpoint returns 503. The next eligible request retries.                                 |
| The host-signals provider finds no TLS/HTTP-2 fingerprints     | Defers with a warning. No identity this request, and no degraded IP-only identifier is minted under the host-signals name.                                                 |
| Geo lookup **fails** (the provider errors)                     | Every permission resolves to the requires-signal floor, and the failure is logged at error level. The failure is **not** papered over with the `default_country` baseline. |
| Geo resolves **no location**, or a country/region with no rule | The `[geo] default_country` baseline applies. This is the configured-default case, deliberately distinct from the failure row above (`GeoStatus` in `ec/consent.rs`).      |
| An incoming cookie value fails the bounds at read-back         | Treated as absent, with a warning naming the source.                                                                                                                       |

The distinction between a failed lookup and no location is resolved in
core, where `EcContext::read_from_request_resolving_geo` runs the
configured geo provider itself and classifies the outcome, so every
adapter reports the two states identically. The draft's remaining matrix rows (rowless
withdrawal records, promotion, the negative-intent outbox, the identity
safety breaker, cluster-listing degradation) belong to the deferred
material of sections 5 and 6.3.

### 6.3 Storage contract

The draft specified a delimiter-free physical key grammar with fixed-width
segments, a provider-code registry, record classes for family revocation,
authority state, negative-intent outbox, rowless withdrawal, and deployment
metadata, wire schemas with known-answer vectors, and a per-field graph-row
contract. The provider-code registry is now implemented: codes are
allocated in `provider-code-registry.md`, carried as the `{code}~` prefix
of every minted identifier, and therefore present in every graph key. The
key grammar differs from the draft in one deliberate way, a tilde separator
instead of delimiter-free fixed width, because pre-envelope bare
identifiers remain deployed and a code such as `51dd` is valid hex, so
delimiter-free parsing could misread a legacy identifier during the
migration window. The remainder (record classes, family revocation,
authority state, outbox, rowless withdrawal, wire schemas, per-field
contract) is not implemented in these PRs and stands as recorded design for
the follow-ups.

The implemented storage today keys the identity graph by the selected
provider's `normalize_id_for_kv` output verbatim. For the built-in HMAC
provider that is the identifier with the hash segment lowercased, which is
today's key, so every pre-epic row stays reachable and the pre-epic
cluster prefix listing stays intact. For an opaque provider the identifier
itself is the key. Rows carry the same JSON envelope as before the epic,
extended with the derived device-classification fields noted in section 5.

## 7. Composition root and adapter parity

Provider construction happens in one place per concern, in core, called by
every adapter. No adapter wires a concrete implementation directly into the
request path:

- `build_provider` (`ec/provider.rs`) constructs the selected EC provider,
  injecting the host's `HostSignals` when supplied and matching an
  adapter-injected vendor provider by its `id()`. The provider is built
  once per request during `EcContext` construction and reused for
  read-back, the permission gate, and minting, so the per-request
  triple-build observed in PR #838 (cloning the secret into a fresh box up
  to three times per request) is gone.
- `build_device_provider` (`ec/device.rs`) returns the builtin classifier
  unless `fastly` is selected, in which case the adapter's closure builds
  the host-evidence provider.
- `build_geo_provider` (`platform/mod.rs`) returns `DisabledGeo` unless
  `platform` is selected, in which case the adapter's host geo
  implementation is used. All four adapters (Fastly, Axum, Cloudflare,
  Spin) route their host geo through this selector when they assemble
  their runtime services, verified in each adapter's platform wiring.

All four adapters construct the EC request state through the same core
constructors (`EcContext::read_from_request_resolving_geo` and its
variants), so selector behavior and the geo failure classification are
identical everywhere. The cross-adapter parity suite
(`trusted-server-integration-tests`) asserts geo response parity across
adapters. The EC API routes are Fastly-only today, as section 1 notes, and
the Spin adapter's route list records why.

The draft's adapter capability matrix (declared per-record-class
consistency semantics, durability and retention proofs, activation and
lease qualification) is **not implemented in these PRs** and is tracked
follow-up work. The matrix's motivating rule is preserved for that
follow-up, which is that "has KV" says nothing about whether a revocation
is observable, so eligibility for identity features must eventually be
declared and checked, not assumed.

## 8. Crate layout and CI

Host and vendor provider crates live in nested directories grouped by
capability, with flat package names following the existing convention:

- `crates/device/fastly` is package `trusted-server-device-fastly`.
- `crates/geo/fastly` is package `trusted-server-geo-fastly`.
- `crates/edgecookie/<vendor>` is the documented home for vendor EC
  crates. The directory currently holds only a README, because the
  built-in providers live in core and no vendor crate exists yet.

The draft mandated flat directories (`crates/trusted-server-geo-fastly`)
and banned placeholder directories. The implementation diverges on both
points. Nested directories scale per vendor as providers multiply, package
names already carry the flat convention, and the README stakes out the
location before the first vendor crate lands. Both divergences are
recorded in the revision table.

Every new crate is in the `.cargo/config.toml` aliases (`check-fastly`,
`clippy-fastly`, `test-fastly`, `build-fastly`), so the provider crates are
linted with `-D warnings` and tested by the same gates as every other
workspace member, closing the PR #838 gap where new crates compiled only
transitively.

## 9. Behavior preservation notes

Two defaults chosen for neutrality change effective behavior on existing
Fastly deployments. Both are called out in the migration spec and must be
prominent in release notes:

- **Bot gate.** The pre-provider EC bot gate required JA4 and platform
  class. The default `builtin` classifier is User-Agent only, so the gate
  is weaker by default. The stronger gate is available as `[device]
provider = "fastly"` rather than being startup-rejected as the draft
  specified. Release notes call out the weaker default rather than
  presenting selection alone as authorization.
- **Geo.** With no geo provider, jurisdiction resolution falls to the
  required `[geo] default_country`. The permission model constrains the
  combination so it cannot silently grant permissions to mis-attributed
  traffic. The default must resolve to a real `permissions.yaml` rule, a
  deployment running an EC provider without geo must set
  `assume_single_jurisdiction = true`, and a failed lookup resolves to the
  requires-signal floor instead of the default. The default flip landed in
  the same series as those constraints, honoring the draft's sequencing
  requirement that the constraint exist before the flip.

## 10. Testing strategy

Implemented, in the crates named:

- Round-trip tests with a non-default provider, proving an opaque
  identifier survives read-back byte for byte (`ec/mod.rs`) and the
  client-cycle value survives the full scenario as cookie and KV key
  (`ec/resolve.rs`).
- Delegation tests proving the injected-provider wrapper forwards
  `accepts_id` and `normalize_id_for_kv` to the inner provider, so a
  vendor identifier is never dropped by the built-in defaults.
- Gate tests proving the HMAC provider's declared requirement blocks
  generation until the permission is set, and that a provider declaring
  nothing requires nothing.
- Settings validation tests covering the section 6 table, including the
  missing block, the block without a selector, explicit `none`, the stray
  block, unknown selector keys for all three concerns, unknown fields in
  every section, the deprecated passphrase migration with its both-forms
  rejection, `default_country` validation, and the jurisdiction
  acknowledgment.
- Geo builder tests showing the default selects no geo, `none` selects no
  geo explicitly, and `platform` selects the host implementation.
- Host-signals provider tests covering minting from fingerprints,
  deferring without them, and the loud failure of a selected but
  uninjected vendor provider.

Deferred with their features are the fixture-driven provider conformance
suite, legacy-reader tests, and the parity cases for capability-mismatch
startup failures.

## 11. Implementation order

As landed:

1. **PR #1043, the Edge Cookie provider seam.** The trait with recognition
   and KV normalization, the global identifier bounds, selection and
   validation, the vendor block capture, the deprecated-passphrase
   migration, and the round-trip proof with a non-default provider.
2. **PR #1044, device and geo selection.** `DeviceProvider` with the
   builtin default and the opt-in Fastly host-evidence provider,
   `PlatformGeo` selection with the no-geo default, all four adapters
   routed through the shared builders, and the opt-in host-signals EC
   provider (carrying the open review question of section 2).
3. **PR #1045, the permission model.** The enforcement point for
   `required_permissions`, the `default_country` requirement and
   jurisdiction acknowledgment, and the failed-lookup floor. That change
   has its own spec, which this document cross-references rather than
   restates.

The draft's step 4 warning (do not flip the geo neutral default before the
permission model exists) was honored. The flip and its constraints landed
together in the permission model change.

## 12. Divergences from issue #778

This spec supersedes #778 on the following points, so implementation has
one acceptance contract:

| #778 says                                                    | This spec says                                                                                                                          | Why                                                                                                                                                                                   |
| ------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Identifier comparison is a provider operation (`keys_equal`) | Comparison is structural. Read-back acceptance and KV normalization route through the provider, so no comparison method exists (§3, §4) | Satisfies the same requirement with no method to leave uncalled                                                                                                                       |
| A provider can return response headers                       | Kept, with a production consumer. EC finalization applies them, and the client-cycle path uses them (§4)                                | The caller the minimalism rule demands landed in the same series                                                                                                                      |
| One built-in provider (HMAC) preserving today's behavior     | HMAC preserved verbatim, plus the opt-in host-signals built-in (open question, §2) and the demo client-cycle provider                   | A switch retires the previous provider's identity population outright (§6.1); carrying identities across a switch (`legacy_providers`) remains follow-up work with the migration spec |

## 13. Revision record vs the 2026-07-31 draft

One row per divergence between the 2026-07-31 draft and the implementation
this revision describes.

| Draft position                                                                                                                      | Implemented position                                                                                                                                                                                                                                                                                                         | Why                                                                                                                                                                       |
| ----------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Trait surface is a canonicalizing `parse` returning a typed id, plus `graph_key_suffix`, `cluster_prefix`, and `verify`             | `accepts_id` (recognition) plus `normalize_id_for_kv` (KV key form), defaults matching the built-in shape. No typed id, key suffix, cluster capability, or `verify`. `keys_equal` stays out, as the draft required.                                                                                                          | Recognition and KV keying are the two operations core performs today. A byte-for-byte round-trip test with a non-default provider pins the contract.                      |
| `GeneratedEdgeCookie::response_headers` and `IdentityInput.permissions` / `.consent` banned as speculative surface                  | Shipped with production consumers. Finalization applies provider headers, the resolve path returns them, and the organic mint path populates the input fields.                                                                                                                                                               | The client-cycle resolve path landed in the same series and is their caller, satisfying the minimalism rule the ban enforced.                                             |
| Identifier bounds enforced at mint and parse                                                                                        | Enforced at mint (`generate` and the resolve endpoint), cookie read-back, and cookie write. Violations rejected outright, never rewritten. `MAX_EC_ID_LEN` in `ec/cookies.rs`.                                                                                                                                               | Every identifier entry point is covered, and the pre-epic sanitizing rewrite was removed as a silent-divergence hazard.                                                   |
| `provider = "none"` is valid alongside `legacy_providers` blocks                                                                    | `none` (or an omitted selector) with any configured provider block is a startup error.                                                                                                                                                                                                                                       | No `legacy_providers` exists in these PRs, so a block alongside statelessness can only be a mistake.                                                                      |
| Every selection key is closed and unknown keys are startup errors                                                                   | Device and geo keys are closed. EC vendor keys are open. Unknown blocks are captured as raw values in core, the adapter deserializes its own block, and a selected key with no injected provider fails loudly.                                                                                                               | Core never names a vendor, so a vendor provider adds no core change.                                                                                                      |
| Capability mismatch is a startup error at adapter wiring time                                                                       | Configuration coherence fails at startup. A host-capability mismatch (missing `HostSignals`, uninjected vendor) fails loudly when the provider is built, stopping the request.                                                                                                                                               | The adapter capability declaration that would move the check to startup is deferred with the capability matrix.                                                           |
| A minting provider with no identity-graph store is a startup error                                                                  | Not implemented. `ec_store` stays optional. The resolve endpoint refuses to mint without a graph. The organic path persists rows whenever the graph is configured.                                                                                                                                                           | Portability adapters run without platform KV. Whether configuration should force the pairing is follow-up work.                                                           |
| `[device] provider = "fastly"` is startup-rejected pending a separate security design                                               | Shipped as a selectable opt-in. The Fastly adapter injects `HostSignals`, the provider strengthens the browser/bot gate, and rows persist derived classes, not raw fingerprints.                                                                                                                                             | Selection is an explicit operator opt-in and the neutral default makes no host fingerprint call.                                                                          |
| The `host-signals` EC provider is deliberately dropped and its selection rejected                                                   | Shipped in PR #1044 as an opt-in built-in that defers with a warning when the host supplies no signals. **Open, flagged for the series review**, not settled either way.                                                                                                                                                     | Its identifier shape shares the HMAC grammar, and a sign-off row defers host fingerprint processing, so the review decides whether the provider ships in the series.      |
| Geo default flip sequenced into the later permission-model step, with an acknowledgment guard                                       | Landed as specified in the same series, with the default of none, `default_country` required and validated against `permissions.yaml`, the `assume_single_jurisdiction` acknowledgment, and a failed lookup resolving to the requires-signal floor with error logging (`GeoStatus`, resolved in core so all adapters agree). | The permission model shipped in PR #1045, so the constraints exist where the draft required them.                                                                         |
| All adapters serve the full EC feature set identically                                                                              | Selector behavior is identical through the shared builders and core constructors. The EC API routes (identify, batch-sync, ec/resolve) are Fastly-only, documented in the Spin route list.                                                                                                                                   | The portability adapters do not yet wire platform KV, and the gap is documented rather than silent.                                                                       |
| Conformance suite, adapter capability matrix, delimiter-free key grammar, `verify`, `legacy_providers`, `versions` / `mint_version` | None of these are in PR #1043 or #1044. All are tracked follow-up work, deferred, not silently dropped.                                                                                                                                                                                                                      | The shipped seam did not need them, and each returns with the feature that gives it a production caller, per the spec's own minimalism rule.                              |
| `required_permissions` removed from the device and geo traits, added to the EC trait only at the permission-model step              | Present on all three traits from the start, with empty defaults. Core enforces the EC declaration (gate in `EcContext`, landing in PR #1045). No device or geo enforcement point exists.                                                                                                                                     | One uniform declaration seam, with an empty default that gates nothing, avoids the decorative-gate hazard while keeping the interface stable. The geo circularity stands. |
| Flat crate directories (`crates/trusted-server-geo-fastly`), no placeholder directories                                             | Nested directories per capability (`crates/device/fastly`, `crates/geo/fastly`, `crates/edgecookie/<vendor>`), flat package names. `crates/edgecookie` ships a README before its first crate.                                                                                                                                | Nested directories scale per vendor, package names already carry the naming convention, and the README stakes out the vendor location.                                    |
