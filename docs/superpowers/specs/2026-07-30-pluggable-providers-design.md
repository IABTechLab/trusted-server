# Design Spec: Pluggable Edge Cookie, Device, and Geo Modules

**Status:** Proposed. The implementation is carried by PR #1043 (Edge Cookie
module seam) and PR #1044 (device and geo selection), neither of which is
yet merged to main. Revised against that implementation, 2026-08-25. Updated on
2026-09-01, where `[geo] default_country` is retired and the fallback baseline
moves to the top node of the `rules:` tree in `permissions.yaml`, where
`jurisdiction` is a per-node inherited attribute and the two `[consent]`
applicability lists retire into the same tree (see the permission-model spec,
§3.2, §3.4 and §12). Updated on 2026-10-06, where what an operator selects
is a module and the selector key reads `module`, with `provider` kept for the
two auction tables (§2.1).
**Author:** Engineering
**Issue references:** #777, #778, #780, #781
**Related specs:** `2026-07-30-permission-model-design.md`,
`2026-07-30-provider-migration-rollout-design.md`,
`2026-07-30-client-cycle-ec-resolve-design.md`
**Last updated:** 2026-10-06

> **Context.** PR #838 proposed a first implementation of this epic in a single
> change. Review of that PR surfaced design gaps this spec exists to close
> before a second implementation pass: an identity abstraction that owned
> creating but not recognition, per-adapter divergence in module selection,
> silent misconfiguration modes, and speculative trait surface with no
> production caller. This spec is the authoritative statement of what the
> module architecture must do; where it contradicts PR #838, this spec wins.
> The second pass has now landed (PR #1043 and PR #1044, with permission
> enforcement in PR #1045), and this revision restates the spec to match the
> implemented code. A final section records every divergence from the
> 2026-07-31 draft.

---

## 1. Overview and goals

Trusted Server makes three per-request data decisions that were previously
hard-wired: whether to create or keep an Edge Cookie (EC) identity, how to
classify the requesting device, and whether to resolve geolocation. Each is
now a **module**, a selectable component chosen in operator configuration,
with a deliberately neutral default.

Goals, as implemented:

- A deployment picks an implementation per concern (including none) without a
  code change to Trusted Server core.
- Defaults are neutral. With no configuration, no EC is created, device
  classification uses only the User-Agent, and no geolocation is performed. A
  default deployment makes no third-party or host-specific call.
- An **EC module declares** the permissions its data use requires
  (`required_permissions` on the trait), and **core enforces** that
  declaration before creating or using an identity. A module cannot
  authorize itself. The enforcement machinery is the permission model's
  subject and lands with it in PR #1045 (see the permission model spec).
  Geo and device carry the same declaration method with an empty default,
  for the reasons spelled out in section 5.
- All adapters (Fastly, Axum, Cloudflare, Spin) route selection through the
  same core builders, so identical configuration selects identical modules
  everywhere. A selection the deployment cannot satisfy fails loudly rather
  than degrading. The EC API routes (identify, batch-sync, ec/resolve) are
  registered by the Fastly entry point only today, because the portability
  adapters do not yet wire a platform KV store. The Spin adapter's route
  list documents that gap explicitly rather than leaving those paths silent.

Non-goals:

- No vendor module ships in this epic beyond the host-platform
  implementations named below. The `crates/edgecookie/` directory holds a
  README describing where vendor EC crates will live.
- The client-cycle (browser round-trip) module type has its own spec. The
  trait ships the seam for it (`resolve_from_client`, a no-op by default)
  and a demonstration module (`client_fixed`) compiled only into test and
  demonstration builds. Production selection of the demo module is a
  startup error.

## 2. Module taxonomy

### 2.1 The configuration convention every module type follows

Every pluggable thing in Trusted Server is a **module**, meaning a
component a deployment selects by name in operator configuration rather
than one core hard-wires. Each module type is one top-level table, and a
selector key inside that table selects what runs. The key is `module`,
except in the two auction tables, where it is `provider`, because what they
select is an auction provider instance, the one meaning `provider` has on
`main`:

```toml
[<type>]
module = "<name>"          # a string where one runs, a list where several run

[<type>.<name>]              # only when that module has settings
setting = "value"
```

The seven types, all singular:

| Type                | What it decides                          | Selector  | It takes                   |
| ------------------- | ---------------------------------------- | --------- | -------------------------- |
| `ec`                | Edge Cookie identity                     | `module`  | a string, one runs         |
| `geo`               | location resolution                      | `module`  | a string, one runs         |
| `device`            | device classification                    | `module`  | a string, one runs         |
| `permission-signal` | which permission signal schemes run      | `modules` | a list, in the order given |
| `demand`            | the sources bids are requested from      | `modules` | a list, all run            |
| `ad-server`         | the ad server that decides what is shown | `module`  | a string, one runs         |
| `integration`       | the vendor modules a page loads          | `modules` | a list, all run            |

The rules, which hold for all seven:

- A `[<type>.<name>]` table exists only where that module has something to
  set. A module with no settings is selected by name alone.
- The word `providers` appears nowhere in configuration. The type table,
  and the `module` or `modules` key inside it, carry the whole selection.
- The name is the implementation, unless the table carries an
  `implementation` line, which names the implementation by its module path.
  A demand table always carries one, because `demand` is not the type its
  implementations are named under, and it is how two Prebid Servers run side
  by side under different names, each with its own endpoint and its own
  settings.
- A module from a crate is named by its folder below `crates/`, with `.`
  between the parts, and may be written without its own section's type
  folder. A `demand` or `ad-server` name is snake_case, because it may be
  a label of the operator's own.
- A `[<type>.<name>]` table that its type's selector does not select
  refuses startup, because an unreferenced table is a mistyped selector or a
  stale block, and accepting one silently invites configuration drift.
- An implementation this build does not have refuses startup, and the
  message lists the implementations it does have.
- A `demand` or `ad-server` endpoint must be HTTPS, or HTTP to a loopback
  host only (`127.0.0.1`, `::1`, `localhost`).
- Every module rejects settings it does not know, so a typo fails loudly
  rather than being quietly ignored.
- A secret setting holds the **name** of a key in `trusted_server_secrets`,
  never a value, so no secret is ever written into a configuration file.

`[auction]` is not a module type of its own. It keeps `enabled`,
`timeout_ms`, the creative settings, `allowed_context_keys`, and
`[auction.bidders.<code>] module = "<demand name>"`, which maps a bidder
code a page asks for onto one of the declared demand providers. Its
`modules` list selects the page modules the auction runs, such as `prebid`
(seam design §2).

### 2.2 The three module types this spec defines

| Concern     | Type     | Trait              | Default                     | Opt-in implementations                                                                                                      |
| ----------- | -------- | ------------------ | --------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| EC identity | `ec`     | `EdgeCookieModule` | none (stateless)            | `hmac` (HMAC over client IP, preserves today's identity), `host_signals` (see below), and `client_fixed` (demo builds only) |
| Device      | `device` | `DeviceProvider`   | `builtin` (User-Agent only) | `fastly` (TLS JA4 and HTTP/2 signals through an injected `HostSignals` service)                                             |
| Geo         | `geo`    | `PlatformGeo`      | `none` (no location)        | `platform` (host geo lookup)                                                                                                |

The geo trait is the existing `PlatformGeo` in `platform/traits.rs` rather
than a new `GeoModule` name. The EC trait lives in `ec/module.rs` and the
device trait in `ec/device.rs`.

Core names none of these implementations. Each one is registered by an
integration builder, which is the same seam a vendor crate uses, and a
builder that supplies an implementation does not have to be a page
integration, so a crate can ship an identity, geo or device implementation
and no browser JavaScript at all. The integration seam spec states that rule
in full and this spec follows it.

Selection keys are strings in operator configuration
(`trusted-server.example.toml` carries the commented template):

```toml
[ec]
module = "hmac"

[ec.hmac]
passphrase_secret = "ec_hmac_passphrase"   # names a trusted_server_secrets key

[device]
module = "builtin"   # default. "fastly" opts into TLS/H2 signal evidence

[geo]
module = "platform"           # default is "none" (no location, no host call)
# The baseline for a request with no resolvable country lives at the top of the
# rules tree in permissions.yaml, not here.
# assume_single_jurisdiction = true   # required when EC runs with no geo
```

**The `host_signals` EC module** (identity from HMAC over the host TLS JA4
and HTTP/2 signals plus the client IP) was deliberately dropped from the
2026-07-31 draft. It has since shipped in PR #1044 as an opt-in
implementation (`[ec.host_signals]`), implemented against the host-agnostic
`HostSignals` capability rather than a Fastly API, so any host that supplies
the signals can run it and a host that supplies none cannot build it.
When the host supplies no signal at all the module defers with a
warning instead of degrading to an IP-only identifier under the
`host_signals` name. **An open review question stands on whether this module should ship
in the series at all**, because its identifier shape shares the built-in
HMAC grammar and a sign-off row defers host signal processing. The
question is flagged for the series review and this spec does not present
the module as settled either way.

## 3. The identity lifecycle contract

This is the section PR #838 lacked. Its trait abstracted **creating** an
identifier but left **recognition** and **KV key normalization** hard-coded
to the built-in HMAC shape, so a module whose identifiers did not match
`{64hex}.{6alnum}` created cookies that the very next request discarded.

The implemented contract routes every lifecycle operation core performs on
an EC value through the selected module:

| Lifecycle operation | Where core uses it                                                                                                                                                                                                                                                | Contract                                                                                                                                                                                                                                                                                                                                                                                                                        |
| ------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Create**          | EC generation on first eligible request, and the client-cycle resolve endpoint                                                                                                                                                                                    | The module returns the identifier (`generate` server-side, `resolve_from_client` for the client cycle) and only core writes the cookie, after enforcing the global bounds below.                                                                                                                                                                                                                                                |
| **Recognize**       | Reading `ts-ec` back from the request, deciding `ec_was_present`, withdrawal checks, and every path that hands the value onward: the origin URL in `append_ec_id`, the click-target URL in `handle_first_party_click`, and the proxied body an integration builds | `accepts_id` answers whether a value is a well-formed identifier the module issues. A value the selected module does not recognize is treated as absent, so it is never used or egressed, while the raw cookie value stays visible to withdrawal handling. The egress paths reach the same answer through `edge_cookie::recognized_ec_id`, and a deployment with no module selected recognizes nothing and so egresses nothing. |
| **KV key**          | Identity-graph row reads and writes                                                                                                                                                                                                                               | `normalize_id_for_kv` returns the key form. The default lowercases the built-in HMAC hash segment and preserves the suffix, keeping today's keys. An opaque or case-sensitive module overrides to the identity function so distinct identifiers never collapse into one row.                                                                                                                                                    |
| **Withdraw**        | Expiring the cookie and writing revocation markers                                                                                                                                                                                                                | The identifiers eligible for a **graph tombstone** are exactly those the selected module owns, dispatched on the `{code}~` prefix first and then `accepts_id`, never a shape check the module cannot influence. Expiring the **cookie** is broader, because it keys off the raw cookie being present, so it still fires for an identifier the selected module does not own (see the switching case, §6.1).                      |

**Invariant:** for every module `P` and every identifier `id` created by
`P`, `id` round-trips read-back byte for byte. A test in `ec/mod.rs` proves
the round-trip with a non-default module whose identifiers are opaque, and
a second test in `ec/resolve.rs` proves the client-cycle value survives the
full scenario verbatim.

The draft's richer lifecycle surface, a canonicalizing `parse` with
per-module equivalence fixtures, a core-constructed graph key built from a
module `graph_key_suffix`, a declared cluster-prefix capability, declared
namespace descriptors with a startup disjointness proof, and a reusable
conformance suite driven by fixtures, is **not implemented in these PRs**.
Recognition plus KV normalization proved sufficient for the operations core
actually performs today, and each deferred piece is tracked as follow-up
work rather than silently dropped (see the revision record). Until the key
grammar lands, the KV key is the module's normalized identifier verbatim,
which keeps every pre-epic HMAC row reachable.

The pre-epic IP-cluster prefix listing runs unchanged, but the key space it
lists over does not. A fresh create is keyed `hmac~<hash>.<suffix>`, so the
prefix `evaluate_cluster` derives is `hmac~<hash>` for a coded row while a
legacy bare row still lists under `<hash>` on its own. Prefix matching is
anchored at the start of the key, so two rows for the same client IP that
straddle the envelope never count each other, and `cluster_size` under-reports
for as long as both populations coexist.

That undercount is accepted rather than bridged, for three reasons.
`cluster_size` is reported in the identify response and gates nothing, and the
only place its value is read at all is a cache short circuit in
`evaluate_cluster` that tests whether a value is stored, not what it is, and
the `cluster_trust_threshold` and `cluster_recheck_secs` settings that a
reader might expect to gate on it have no readers in the code. The undercount
is bounded by the legacy bare-identifier read window in section 6.1 and ends
when the last pre-epic cookie expires. And bridging it would mean a second
prefix scan on every identify request for the whole of that window. It becomes
a real fault only if a later change makes the count gate something, and that
is the change that has to build the bridge.

One global rule sits above every module, and it is implemented:

- **Identifier bounds.** A created identifier obeys a global cookie-safe
  alphabet (normatively `[A-Za-z0-9._~-]`, valid cookie octets with no
  separators, whitespace, or control characters) and a global maximum of
  **256 bytes**, stated here so dependent documents reference one number.
  The bound applies to the identifier itself, not only its key form. Core
  enforces the bound wherever an identifier enters the system, at create
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
wrong place, because what a module may see is not the control. What a module
may do with what it sees is the control, and that is the permission model.

So `RequestInfo` carries everything the request carries, whether or not code in
this repository reads it yet. The rule stands for behavioral traits, where a
method with no caller really is dead weight. How the surface observed in PR #838
resolved in the implementation:

- `keys_equal`: **not shipped.** Its legitimate purpose (equivalent-envelope
  comparison, #778) is served structurally, because read-back acceptance and
  KV normalization both route through the module, so no comparison method
  exists to leave uncalled.
- `GeneratedEdgeCookie::response_headers`: **shipped, with a production
  caller.** EC finalization applies module-requested headers to the
  outbound response, and the client-cycle resolve path returns them, which
  is how a client-side module requests further evidence from the page.
  The draft banned the field when nothing consumed it. The consumer landed
  in the same series, satisfying the rule the ban enforced.
- `IdentityInput.permissions` / `IdentityInput.consent`: **shipped and
  populated.** The organic create path passes the request's resolved
  permission state and consent context so a module can read them for
  behavior beyond gating. The gate itself has already run before `generate`
  is called, so a module cannot use the fields to authorize itself.
- `required_permissions` on `DeviceProvider` and `PlatformGeo`: **present,
  with an empty default. A module device declaration is refused rather than
  ignored, and a geo declaration is not consulted.** The draft removed the
  method from both traits because PR
  #838's copies were decorative. The implementation keeps one uniform
  declaration seam across all three traits instead. The built-in device
  and geo modules declare empty sets, and core enforces the declaration
  as a request gate only for the EC module (section 5). Because no
  per-request device gate exists to honor a device declaration, a nonempty
  `required_permissions` from a module-supplied device module is
  rejected when the registry resolves the selection and the deployment
  fails to start, so nothing reads as a gate that is not one. Section 5
  gives the reasoning. The geo circularity argument stands unchanged and
  is restated in section 5.

The implemented `EdgeCookieModule` surface (`ec/module.rs`):

```rust
pub trait EdgeCookieModule: Send + Sync + core::fmt::Debug {
    /// Stable configuration key ("hmac").
    fn id(&self) -> &'static str;
    /// Registered four-character code (provider-code-registry.md), the
    /// `{code}~` namespace of every identifier the module creates.
    /// Mandatory, no default: a module cannot exist without a unique
    /// code, so identifiers from different modules can never collide.
    fn code(&self) -> ProviderCode;
    /// Derives an identifier from the module's injected services and the
    /// request evidence passed at call time. A client-side module defers
    /// here (returns no id) and creates later in resolve_from_client.
    fn generate(
        &self,
        request_info: &dyn RequestInfo,
        input: &IdentityInput<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>>;
    /// Whether `value` is a well-formed identifier this module issues.
    /// Default: the built-in HMAC shape (`<64 hex>.<6 alphanumeric>`).
    fn accepts_id(&self, value: &str) -> bool { /* built-in shape */ }
    /// The KV-key form of `value`. Default: lowercase the HMAC hash
    /// segment, preserve the suffix. Opaque modules return the value
    /// unchanged.
    fn normalize_id_for_kv(&self, value: &str) -> String { /* ... */ }
    /// Permissions this module's data use requires. Default: none, so a
    /// vendor-neutral module requires no permission.
    fn required_permissions(&self) -> PermissionSet { /* none */ }
    /// Client-cycle counterpart to generate: creates from a value the page
    /// posted to the resolve endpoint, after verifying it. Default: no-op,
    /// so a server-side module does not participate. See the
    /// client-cycle spec.
    fn resolve_from_client(
        &self,
        input: &ClientResolveInput<'_>,
    ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> { /* ... */ }
}
```

Core owns the code envelope. At create it prefixes the module's value with
`{code}~`, at read-back it strips and checks the code before the module's
`accepts_id` sees the value part, and the identity graph key preserves the
code verbatim around the module's canonical form. A cookie carrying
another module's code is treated as absent, never adopted, so switching
modules cannot silently mix identity populations, and a withdrawal always
acts on a key that can only belong to one module. The built-in HMAC
module creates `hmac~<64 hex>.<6 alphanumeric>` and dual-reads its
pre-envelope bare form for one release cycle so deployed cookies keep
working, and the bare form belongs to hmac alone. Codes are allocated
append-only in `provider-code-registry.md`, and a leading digit is valid
(`1a2b`).

The draft's alternative shape (`parse` returning a typed `EcId`,
`graph_key_suffix`, `cluster_prefix`, `verify`, and a version-carrying
`GeneratedIdentity`) was not adopted. `verify`, module versions, and
`mint_version` are tracked follow-up work with the migration spec.
Request data reaches a module through injected services and the
`RequestInfo` passed at call time, not through a fixed parameter struct.
`RequestInfo` carries the evidence a module in this workspace reads today,
which is the normalized client IP. Further evidence (headers, cookies, client
hints, the URL) is added to it as a defaulted accessor in the change that first
reads it, so an existing implementation keeps compiling and no accessor lands
ahead of the caller that consumes it.

## 5. Permission enforcement is core's job, for EC modules

Before creating through an EC module, core resolves the request's
permission state and refuses when the module's `required_permissions()`
are not all set. The gate is implemented in `EcContext`. The selected
module is built once at request read time, its declaration is checked
against the resolved state, and generation is skipped (with a log line
naming the jurisdiction) when the requirement is not met. With no module
selected, nothing may create or use an identifier, so the gate is closed
rather than open by default. The enforcement point lands with the
permission model in PR #1045, and the permission model spec governs the
resolution machinery (country and region baselines, signals, and the
requires-signal floor).

**Recognition and withdrawal always run**, permissions or not. Read-back
acceptance and withdrawal eligibility go through `accepts_id` with no
permission check, and withdrawal handling keeps the raw cookie value even
when the identifier is treated as absent, so an opt-out can always reach
the identity it revokes. A blanket execution gate would refuse to run the
module in exactly the state an opt-out produces.

The draft additionally specified an identity activation protocol (a
two-record commit point before any egress), rowless-cookie classification
and per-prefix withdrawal records, negative-record admission rules, and a
typed egress boundary (`AuthorizedIdentity<Scope>`,
`RedactedRequestView`). **None of that is implemented in these PRs.**
Those positions remain recorded in the draft and are tracked as follow-up
work with the permission model spec, which owns identity-state persistence
and egress typing. The revision record lists them as deferred.

The gate applies to EC modules **only**. Geo and device are ungated for
different reasons, stated separately because only one of them is
structural:

- **Geo: circularity.** The permission set is resolved from jurisdiction,
  which is resolved by the geo module. Gating geo on the resolved set is
  unsatisfiable. `PlatformGeo::required_permissions` exists with an empty
  default for interface uniformity, and nothing consults it on the lookup
  path.
- **Device: host evidence is an explicit opt-in, not authorized by
  selection defaults.** Device classification is not an input to permission
  resolution. The neutral `builtin` classifier reads only the User-Agent
  and makes no host call. The draft went further and made selecting a
  host-signal-reading device module a startup error pending a separate
  security design. The implementation instead ships
  `[device] module = "fastly"` as a selectable opt-in. The Fastly adapter injects a
  `HostSignals` service carrying the TLS JA4 and HTTP/2 signals, and
  the module uses them to strengthen the browser/bot gate that guards EC
  writes. Identity rows persist the derived classification fields (the JA4
  class segment and a 12-hex-character hash prefix of the HTTP/2 SETTINGS
  signal), not raw signals, and the neutral default persists
  neither because the builtin module produces no such fields.
- **Device: a declared permission is refused at startup, not ignored at
  request time.** `DeviceProvider::required_permissions` has no
  enforcement point on the device path, so a declaration cannot be
  honored. Before the module seam that was inert, because core and the
  host supplied the only two device modules and both declare the empty
  set. With a vendor seam the method reads as a promise a vendor could
  build on, and a vendor device module declaring a permission would
  still run on every request while appearing to be gated. Core therefore
  rejects a nonempty `required_permissions` from a module-supplied device
  module when the registry resolves the selection, failing the
  deployment at startup with a message naming the module and the
  permissions it declared. That refusal is lifted only when a real
  per-request device gate exists, which is separate design work. The
  built-in and host device modules are unaffected, because both declare
  the empty set.

## 6. Selection, validation, and failure modes

All configuration validation happens at **settings construction**
(`Settings::finalize_deserialized` runs every check below), so a
misconfiguration expressible in configuration alone is a startup error,
never a silent behavior change. A selection that only the running host can
satisfy (an injected vendor module, or host signals) fails loudly
when the module is built, stopping the request rather than degrading.

| Configuration state                                                               | Behavior                                                                                                                                                                                                                                                                                                 |
| --------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `[ec] module` names an implementation whose settings table is required and absent | Startup error naming the missing table. A table is required only where the implementation has a setting it cannot default, as `hmac` cannot default its passphrase secret.                                                                                                                               |
| `[ec.<name>]` table present, `module` unset                                       | **Startup error.** (In PR #838 this silently ran stateless. The half-migrated config becomes a production identity outage detected by revenue drop. Rejecting it is the fix.) An operator who genuinely wants stateless deletes the table.                                                               |
| `module = "none"` (explicit stateless)                                            | Valid, and means exactly what omitting the selector means. Any configured module table alongside it is a startup error, the same stray-table rule as below.                                                                                                                                              |
| A configured `[ec.<name>]` table that `module` does not select                    | **Startup error**, the rule §2.1 states for every module type. An unreferenced table is almost always a mistyped selector or a stale block, and accepting one silently invites configuration drift.                                                                                                      |
| `[ec] module` names an implementation this build does not have                    | Startup error listing the implementations the build does have, so the operator sees at once whether the crate is missing or the name is mistyped.                                                                                                                                                        |
| `module = "host_signals"` on a host that supplies no signals                      | Loud failure when the module is built. A host that cannot produce `HostSignals` cannot run the module.                                                                                                                                                                                                   |
| `module = "client_fixed"` in a production build                                   | Startup error. The demonstration module is compiled only behind the `client-fixed-demo` cargo feature.                                                                                                                                                                                                   |
| No `module`, no module table                                                      | Valid, the neutral default for that concern.                                                                                                                                                                                                                                                             |
| Deprecated `[ec] passphrase`                                                      | Migrated to `module = "hmac"` with the passphrase secret named in `[ec.hmac]`, with a deprecation warning naming the new location. Both forms together are rejected so a half-edited file fails loudly instead of one form silently winning.                                                             |
| Any unknown key in `[ec]`, `[device]`, `[geo]`, or a module's own table           | Startup error. Every module rejects settings it does not know (§2.1), so a typo like `providr`, or a key from a deferred feature (`legacy_providers`, `rewrite_legacy`, `versions`), fails loudly.                                                                                                       |
| `[device] module` names an implementation the build does not have                 | Startup error listing what the build does have. The implementations in this repository are `builtin` (default) and `fastly`.                                                                                                                                                                             |
| `[geo] module` names an implementation the build does not have                    | Startup error listing what the build does have. Valid states are unset (the default, no geolocation), `none` (the same, spelled out), and `platform`.                                                                                                                                                    |
| `permissions.yaml` top node missing `group` or `jurisdiction`                     | **Startup error.** The top node is the permission baseline for a request the geo module leaves unmatched, so there must always be one, its `group` must name a defined group, and its `jurisdiction` states the consent handling for that request. Nodes below inherit both unless they state their own. |
| An EC module configured, no geo module, `assume_single_jurisdiction` unset        | **Startup error.** With geolocation off, every request resolves at the top of the rules tree, so a visitor from any other jurisdiction silently receives that baseline's rules. That is acceptable only as an explicit operator decision.                                                                |

One draft row was not adopted, the startup error for a creating module
with no identity-graph store. `[ec] ec_store` remains optional, because the
portability adapters run without platform KV. The client-cycle resolve
endpoint refuses to create when no graph is available (a cookie without a row
could never be withdrawn through the graph), and the organic path persists
the row whenever the graph is configured. Whether configuration should
force the pairing is follow-up work with the migration spec.

Vendor module tables deserve their own note. Any `[ec.<name>]` table whose
name core does not itself know is captured as raw values (a flattened map),
and the crate that registered the implementation deserializes that table into
its own config type. Core names no vendor, so a new module adds nothing to
core. The vendor crate rejects settings it does not know when it
deserializes, which is the §2.1 rule applied by the party that owns the
settings.

### 6.1 Module switching: what a switch actually does

Switching `[ec] module` **retires every identity the previous module
created**. This section says exactly what that means, because a deployer has
to plan around it rather than discover it.

The draft specified an ordered `legacy_providers` reader list, module
`versions` with `mint_version` rotation, provenance tagging, and retirement
evidence rules, as the mechanism that would carry identities across a
switch. **None of that is implemented in these PRs.** The keys are rejected
as unknown, and the design is tracked follow-up work with the migration
spec. Until it lands there is no continuity across a switch of any kind.

An earlier version of this section claimed shape-based continuity, that old
cookies stay recognized when the newly selected module accepts their
shape. That is not what the code does and never was once core took
ownership of the `{code}~` envelope (§5). Ownership is decided on the code
prefix **before** any module is asked about the shape, so a newly selected
module rejects every identifier the previous one created, whatever its
shape, because the code differs.

What a switch does, precisely:

- **Read-back.** Every identifier carrying the retired module's code is
  treated as absent. It never becomes the request's active identity, never
  egresses to a partner, and is rejected on the pull-sync, batch-sync and
  admin paths too. This is the §5 guarantee and it is the half of the
  behavior that matters most, which is that two modules' identity populations can never
  mix.
- **The browser cookie.** A later withdrawal still expires the `ts-ec`
  cookie, because that path keys off the raw cookie being present rather
  than off who owns it. The browser stops carrying the retired identifier.
- **The identity-graph rows.** A later withdrawal does **not** tombstone the
  retired module's rows. Core cannot derive their canonical keys, because
  the canonical form is the owning module's own normalization and the
  owning module is no longer configured. Those rows stay as they are until
  their one-year entry TTL expires.
- **The `ts-ecr` client-cycle marker.** The marker carries no identity, so
  it is not namespaced by the `{code}~` envelope and a switch would
  otherwise leave it standing with its long `Max-Age` intact. Core expires
  it on any request carrying a `ts-ec` the selected module does not own,
  using the same ownership test as read-back above, so a visitor whose
  identifier has just become unrecognized is not left behind a marker that
  tells the page script a resolve has already succeeded. Without that the
  visitor would sit with no identity instead of a restarted one. The
  client-cycle spec states the rule in full.

**What a deployer must do about revocation.** Treat a module switch as a
one-way retirement of the identity population, and deal with the previous
module's rows before or alongside the switch, not after. A withdrawal
that arrives after the switch clears the browser but leaves the row. Every
row a retired module wrote shares that module's `{code}~` key prefix, so
the set is identifiable and can be listed and cleared with the platform's
own KV tooling. Either clear it at the switch, or accept that the rows
persist until the one-year TTL expires and that withdrawals arriving in the
meantime are recorded only in the browser. Do not switch modules while
identities are live that the deployment may still be obliged to revoke in
the identity graph.

The `cluster_fallback` degradation policy from the draft is likewise
deferred with the cluster capability itself.

### 6.2 Runtime failure modes

Startup validation covers configuration. This covers a healthy
configuration meeting an unhealthy runtime. Implemented behavior, each row
logged, none silent:

| Failure                                                           | Behavior                                                                                                                                                            |
| ----------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `generate` returns an error                                       | No identity this request. The organic caller logs at error level and the request proceeds stateless. No cookie is written.                                          |
| A module creates an identifier outside the global bounds          | Rejected at create, never rewritten. The organic path yields no identity. The resolve endpoint returns 400.                                                         |
| Identity-graph write fails at create                              | The create is undone (no identifier, no cookie), with the error logged. The resolve endpoint returns 503. The next eligible request retries.                        |
| The `host_signals` module finds no TLS/HTTP-2 signals             | Defers with a warning. No identity this request, and no degraded IP-only identifier is created under the `host_signals` name.                                       |
| Geo lookup **fails** (the module errors)                          | Every permission resolves to the requires-signal floor, and the failure is logged at error level. The failure is **not** papered over with the top-node baseline.   |
| Geo resolves **no location**, or a country or region with no node | The rules tree's top-node baseline applies. This is the configured-default case, deliberately distinct from the failure row above (`GeoStatus` in `ec/consent.rs`). |
| An incoming cookie value fails the bounds at read-back            | Treated as absent, with a warning naming the source.                                                                                                                |

The distinction between a failed lookup and no location is resolved in
core, where `EcContext::read_from_request_resolving_geo` runs the
configured geo module itself and classifies the outcome, so every
adapter reports the two states identically. The draft's remaining matrix rows (rowless
withdrawal records, promotion, the negative-intent outbox, the identity
safety breaker, cluster-listing degradation) belong to the deferred
material of sections 5 and 6.3.

### 6.3 Storage contract

The draft specified a delimiter-free physical key grammar with fixed-width
segments, a module-code registry, record classes for family revocation,
authority state, negative-intent outbox, rowless withdrawal, and deployment
metadata, wire schemas with known-answer vectors, and a per-field graph-row
contract. The module-code registry is now implemented, with codes
allocated in `provider-code-registry.md`, carried as the `{code}~` prefix
of every created identifier, and therefore present in every graph key. The
key grammar differs from the draft in one deliberate way, a tilde separator
instead of delimiter-free fixed width, because pre-envelope bare
identifiers remain deployed and a code such as `1a2b` is valid hex, so
delimiter-free parsing could misread a legacy identifier during the
migration window. The remainder (record classes, family revocation,
authority state, outbox, rowless withdrawal, wire schemas, per-field
contract) is not implemented in these PRs and stands as recorded design for
the follow-ups.

The implemented storage today keys the identity graph by the selected
module's `normalize_id_for_kv` output verbatim. For the built-in HMAC
module that is the identifier with the hash segment lowercased, which is
today's key, so every pre-epic row stays reachable and the pre-epic
cluster prefix listing stays intact. For an opaque module the identifier
itself is the key. Rows carry the same JSON envelope as before the epic,
extended with the derived device-classification fields noted in section 5.

## 7. Composition root and adapter parity

Module construction happens in one place per concern, in core, called by
every adapter. No adapter wires a concrete implementation directly into the
request path:

- `build_module` (`ec/module.rs`) constructs the selected EC module,
  injecting the host's `HostSignals` when supplied and matching an
  adapter-injected vendor module by its `id()`. The module is built
  once per request during `EcContext` construction and reused for
  read-back, the permission gate, and creating, so the per-request
  triple-build observed in PR #838 (cloning the secret into a fresh box up
  to three times per request) is gone.
- `build_device_provider` (`ec/device.rs`) returns the `builtin` classifier
  unless `fastly` is selected, in which case the registered implementation
  builds the host-evidence module.
- `build_geo_module` (`platform/mod.rs`) returns `DisabledGeo` unless
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

Host and vendor module crates live in nested directories grouped by
capability, with flat package names following the existing convention:

- `crates/device/fastly` is package `trusted-server-device-fastly`.
- `crates/geo/fastly` is package `trusted-server-geo-fastly`.
- `crates/edgecookie/<vendor>` is the documented home for vendor EC
  crates. The directory currently holds only a README, because the
  built-in modules live in core and no vendor crate exists yet.

The draft mandated flat directories (`crates/trusted-server-geo-fastly`)
and banned placeholder directories. The implementation diverges on both
points. Nested directories scale per vendor as modules multiply, package
names already carry the flat convention, and the README stakes out the
location before the first vendor crate lands. Both divergences are
recorded in the revision table.

Every new crate is in the `.cargo/config.toml` aliases (`check-fastly`,
`clippy-fastly`, `test-fastly`, `build-fastly`), so the module crates are
linted with `-D warnings` and tested by the same gates as every other
workspace member, closing the PR #838 gap where new crates compiled only
transitively.

## 9. Behavior preservation notes

Two defaults chosen for neutrality change effective behavior on existing
Fastly deployments. Both are called out in the migration spec and must be
prominent in release notes:

- **Bot gate.** The pre-module EC bot gate required JA4 and platform
  class. The default `builtin` classifier is User-Agent only, so the gate
  is weaker by default. The stronger gate is available as
  `[device] module = "fastly"` rather than being startup-rejected as the
  draft specified. Release notes call out the weaker default rather than
  presenting selection alone as authorization.
- **Geo.** With no geo module, jurisdiction resolution falls to the
  required top node of the `permissions.yaml` rules tree. The permission model
  constrains the combination so it cannot silently grant permissions to
  mis-attributed traffic. The top node must carry a `group` naming a defined
  group and a `jurisdiction`, a
  deployment running an EC module without geo must set
  `assume_single_jurisdiction = true`, and a failed lookup resolves to the
  requires-signal floor instead of that baseline. The default flip landed in
  the same series as those constraints, honoring the draft's sequencing
  requirement that the constraint exist before the flip.

## 10. Testing strategy

Implemented, in the crates named:

- Round-trip tests with a non-default module, proving an opaque
  identifier survives read-back byte for byte (`ec/mod.rs`) and the
  client-cycle value survives the full scenario as cookie and KV key
  (`ec/resolve.rs`).
- Delegation tests proving the injected-module wrapper forwards
  `accepts_id` and `normalize_id_for_kv` to the inner module, so a
  vendor identifier is never dropped by the built-in defaults.
- Gate tests proving the HMAC module's declared requirement blocks
  generation until the permission is set, and that a module declaring
  nothing requires nothing.
- Settings validation tests covering the section 6 table, including the
  missing block, the block without a selector, explicit `none`, the stray
  block, unknown selector keys for all three concerns, unknown fields in
  every section, the deprecated passphrase migration with its both-forms
  rejection, top-node validation of the rules tree, and the jurisdiction
  acknowledgment.
- Geo builder tests showing the default selects no geo, `none` selects no
  geo explicitly, and `platform` selects the host implementation.
- Host-signals module tests covering creating from host signals,
  deferring without them, and the loud failure of a selected but
  uninjected vendor module.

Deferred with their features are the fixture-driven module conformance
suite, legacy-reader tests, and the parity cases for capability-mismatch
startup failures.

## 11. Implementation order

As landed:

1. **PR #1043, the Edge Cookie module seam.** The trait with recognition
   and KV normalization, the global identifier bounds, selection and
   validation, the vendor block capture, the deprecated-passphrase
   migration, and the round-trip proof with a non-default module.
2. **PR #1044, device and geo selection.** `DeviceProvider` with the
   builtin default and the opt-in Fastly host-evidence module,
   `PlatformGeo` selection with the no-geo default, all four adapters
   routed through the shared builders, and the opt-in `host_signals` EC
   module (carrying the open review question of section 2).
3. **PR #1045, the permission model.** The enforcement point for
   `required_permissions`, the fallback-baseline requirement (shipped as
   `[geo] default_country` and since 2026-09-01 carried by the rules tree's
   top node) and jurisdiction acknowledgment, and the failed-lookup floor. That change
   has its own spec, which this document cross-references rather than
   restates.

The draft's step 4 warning (do not flip the geo neutral default before the
permission model exists) was honored. The flip and its constraints landed
together in the permission model change.

## 12. Divergences from issue #778

This spec supersedes #778 on the following points, so implementation has
one acceptance contract:

| #778 says                                                  | This spec says                                                                                                                        | Why                                                                                                                                                                                     |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Identifier comparison is a module operation (`keys_equal`) | Comparison is structural. Read-back acceptance and KV normalization route through the module, so no comparison method exists (§3, §4) | Satisfies the same requirement with no method to leave uncalled                                                                                                                         |
| A module can return response headers                       | Kept, with a production consumer. EC finalization applies them, and the client-cycle path uses them (§4)                              | The caller the minimalism rule demands landed in the same series                                                                                                                        |
| One built-in module (HMAC) preserving today's behavior     | HMAC preserved verbatim, plus the opt-in `host_signals` implementation (open question, §2) and the demo client-cycle module           | A switch retires the previous module's identity population outright (§6.1), and carrying identities across a switch (`legacy_providers`) remains follow-up work with the migration spec |

## 13. Revision record vs the 2026-07-31 draft

One row per divergence between the 2026-07-31 draft and the implementation
this revision describes.

| Draft position                                                                                                                      | Implemented position                                                                                                                                                                                                                                                                                                                                                            | Why                                                                                                                                                                                                   |
| ----------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Trait surface is a canonicalizing `parse` returning a typed id, plus `graph_key_suffix`, `cluster_prefix`, and `verify`             | `accepts_id` (recognition) plus `normalize_id_for_kv` (KV key form), defaults matching the built-in shape. No typed id, key suffix, cluster capability, or `verify`. `keys_equal` stays out, as the draft required.                                                                                                                                                             | Recognition and KV keying are the two operations core performs today. A byte-for-byte round-trip test with a non-default module pins the contract.                                                    |
| `GeneratedEdgeCookie::response_headers` and `IdentityInput.permissions` / `.consent` banned as speculative surface                  | Shipped with production consumers. Finalization applies module headers, the resolve path returns them, and the organic create path populates the input fields.                                                                                                                                                                                                                  | The client-cycle resolve path landed in the same series and is their caller, satisfying the minimalism rule the ban enforced.                                                                         |
| Identifier bounds enforced at create and parse                                                                                      | Enforced at create (`generate` and the resolve endpoint), cookie read-back, and cookie write. Violations rejected outright, never rewritten. `MAX_EC_ID_LEN` in `ec/cookies.rs`.                                                                                                                                                                                                | Every identifier entry point is covered, and the pre-epic sanitizing rewrite was removed as a silent-divergence hazard.                                                                               |
| `module = "none"` is valid alongside `legacy_providers` blocks                                                                      | `none` (or an omitted selector) with any configured module block is a startup error.                                                                                                                                                                                                                                                                                            | No `legacy_providers` exists in these PRs, so a block alongside statelessness can only be a mistake.                                                                                                  |
| Every selection key is closed and unknown keys are startup errors                                                                   | Device and geo keys are closed. EC vendor keys are open. Unknown blocks are captured as raw values in core, the adapter deserializes its own block, and a selected key with no injected module fails loudly.                                                                                                                                                                    | Core never names a vendor, so a vendor module adds no core change.                                                                                                                                    |
| Capability mismatch is a startup error at adapter wiring time                                                                       | Configuration coherence fails at startup. A host-capability mismatch (missing `HostSignals`, uninjected vendor) fails loudly when the module is built, stopping the request.                                                                                                                                                                                                    | The adapter capability declaration that would move the check to startup is deferred with the capability matrix.                                                                                       |
| A creating module with no identity-graph store is a startup error                                                                   | Not implemented. `ec_store` stays optional. The resolve endpoint refuses to create without a graph. The organic path persists rows whenever the graph is configured.                                                                                                                                                                                                            | Portability adapters run without platform KV. Whether configuration should force the pairing is follow-up work.                                                                                       |
| `[device] module = "fastly"` is startup-rejected pending a separate security design                                                 | Shipped as a selectable opt-in. The Fastly adapter injects `HostSignals`, the module strengthens the browser/bot gate, and rows persist derived classes, not raw signals.                                                                                                                                                                                                       | Selection is an explicit operator opt-in and the neutral default makes no host signal call.                                                                                                           |
| The `host_signals` EC module is deliberately dropped and its selection rejected                                                     | Shipped in PR #1044 as an opt-in implementation that defers with a warning when the host supplies no signals. **Open, flagged for the series review**, not settled either way.                                                                                                                                                                                                  | Its identifier shape shares the HMAC grammar, and a sign-off row defers host signal processing, so the review decides whether the module ships in the series.                                         |
| Geo default flip sequenced into the later permission-model step, with an acknowledgment guard                                       | Landed as specified in the same series, with the default of none, a required and validated fallback baseline (`default_country` at the time, the rules tree's top node since 2026-09-01), the `assume_single_jurisdiction` acknowledgment, and a failed lookup resolving to the requires-signal floor with error logging (`GeoStatus`, resolved in core so all adapters agree). | The permission model shipped in PR #1045, so the constraints exist where the draft required them.                                                                                                     |
| All adapters serve the full EC feature set identically                                                                              | Selector behavior is identical through the shared builders and core constructors. The EC API routes (identify, batch-sync, ec/resolve) are Fastly-only, documented in the Spin route list.                                                                                                                                                                                      | The portability adapters do not yet wire platform KV, and the gap is documented rather than silent.                                                                                                   |
| Conformance suite, adapter capability matrix, delimiter-free key grammar, `verify`, `legacy_providers`, `versions` / `mint_version` | None of these are in PR #1043 or #1044. All are tracked follow-up work, deferred, not silently dropped.                                                                                                                                                                                                                                                                         | The shipped seam did not need them, and each returns with the feature that gives it a production caller, per the spec's own minimalism rule.                                                          |
| `required_permissions` removed from the device and geo traits, added to the EC trait only at the permission-model step              | Present on all three traits from the start, with empty defaults. Core enforces the EC declaration (gate in `EcContext`, landing in PR #1045). No device or geo per-request enforcement point exists, so a nonempty device declaration from a module-supplied module is refused when the registry resolves the selection and the deployment fails to start.                      | One uniform declaration seam keeps the interface stable, and refusing at startup what core cannot honor per request avoids the decorative-gate hazard the draft aimed at. The geo circularity stands. |
| Flat crate directories (`crates/trusted-server-geo-fastly`), no placeholder directories                                             | Nested directories per capability (`crates/device/fastly`, `crates/geo/fastly`, `crates/edgecookie/<vendor>`), flat package names. `crates/edgecookie` ships a README before its first crate.                                                                                                                                                                                   | Nested directories scale per vendor, package names already carry the naming convention, and the README stakes out the vendor location.                                                                |
